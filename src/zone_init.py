"""zone-init: the agent inside a zone.

Runs as PID 2 of the zone's pid namespace (bwrap is PID 1 and reaps).
Listens on a socket the host made and handed over as fd $ZONE_CTL_FD (its
path, $XDG_RUNTIME_DIR/appzones/ZONE/agent.sock on the host, is not in the
zone, so nothing in the zone can replace it). One JSON request line per connection:

  {"op": "ping"}                         -> {"ok": true, "zone": Z, "pid": N}
  {"op": "ps"}                           -> {"ok": true, "procs": [[pid, cmdline], ...], "init": pid,
                                              "helpers": [pid, ...]} (the zone's dbus-daemon: not an app)
  {"op": "run", "argv": [...], "env": {...}, "cwd": "...", "wait": bool, "bus": "host"|"zone"|"none"}
        with "wait", the client passes its stdin/stdout/stderr as SCM_RIGHTS
        (pipes or files only; never a tty), and gets {"exit": code} at the end.
        Without, the app's output goes through zone-init to its stdout (the
        journal), each line prefixed "[app pid]", and the last lines are kept.
        bus: "host" (default) the filtered proxy to the host bus at
        $XDG_RUNTIME_DIR/bus; "zone" the zone's own bus (a dbus-daemon
        started here on first use, $XDG_RUNTIME_DIR/zone-bus/bus): names on it
        exist only in this zone, so a single-instance app can own its name;
        "none" no bus at all: the app runs in its own mount namespace (nested
        bwrap) where neither socket exists, without DBUS_SESSION_BUS_ADDRESS.
  {"op": "status", "pid": N}             -> {"ok": true, "running": bool, "exit": code|null, "tail": [lines]}
  {"op": "shell", "rows": R, "cols": C, "term": T, "argv": [...]}
        a pty is allocated here, inside the zone; after {"ok": true, "pid": N}
        the connection carries raw bytes both ways. No host tty fd ever
        crosses into the zone.
  {"op": "resize", "pid": N, "rows": R, "cols": C}   resize a shell's pty
  {"op": "mounts"}                       -> {"ok": true, "mounts": [[path, "ro"|"rw"], ...]} under /mnt/host
  {"op": "busy", "path": "/mnt/host/N"}  -> {"ok": true, "procs": [[pid, cmdline, what], ...]}: what holds
        a runtime mount (cwd, root, exe, open files, mapped files), for `appzones umount`

Anything inside the zone can reach this socket too; it can only start
processes in its own zone, which it can do anyway.
"""
import collections
import fcntl
import json
import os
import pty
import select
import selectors
import shutil
import signal
import socket
import stat
import struct
import subprocess
import sys
import termios
import threading

ZONE = os.environ.get("ZONE", "?")
# Mozilla apps own org.mozilla.<remoting name>.<profile> on the host bus for
# their "already running" remote. Every zone's profile is named the same, so
# each zone's app gets its own D-Bus app name (MOZ_DBUS_APP_NAME), and the proxy lets the zone own
# only that one (appzones/portals.py dbus_rules): a second launch reaches this zone's
# Firefox and never another zone's or the host's. Keep in step with appzones/portals.py.
MOZ_APPS = ("firefox", "firefox-esr", "firefox-beta", "firefox-devedition", "firefox-nightly", "librewolf")


def moz_remoting_name(zone, app):
    return f"{app.replace('-', '_')}_zone_{zone.replace('-', '_')}"


ENV_ALLOW = {"TERM", "COLUMNS", "LINES", "LANG", "LC_ALL", "MOZ_HEADLESS", "NO_AT_BRIDGE"}
SHELL = ["/bin/bash", "-i"]  # NixOS zones have /bin too (the system's sw/bin)

ZRT = os.environ.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}")
ZBUS_DIR = f"{ZRT}/zone-bus"
ZBUS_ADDR = f"unix:path={ZBUS_DIR}/bus"
TAIL = 20

sel = selectors.DefaultSelector()
waiting: dict[int, socket.socket] = {}
shells: dict[int, tuple[socket.socket, int]] = {}  # pid -> (conn, pty master)
outputs: dict[int, tuple[int, str, bytearray]] = {}  # pipe fd -> (pid, label, partial line)
tails: dict[int, collections.deque] = {}  # pid -> last output lines
running: dict[int, subprocess.Popen] = {}  # held, so Popen.__del__ never queues them for subprocess's own reaping
finished: "collections.OrderedDict[int, tuple[int, list[str]]]" = collections.OrderedDict()
zbus: subprocess.Popen | None = None


def log(msg):
    print(f"[zone-init] {msg}", flush=True)


# ── the zone's own bus ────────────────────────────────────────────────────
# Service files the zone bus does not offer: the portals and the host's
# session services would start *inside* the zone, where they cannot work
# (no host bus, no layer-shell) and would only make apps wait for them.
ZBUS_SKIP = ("org.freedesktop.portal.", "org.freedesktop.impl.portal.", "org.freedesktop.Flatpak",
             "org.flatpak.", "org.freedesktop.systemd1", "org.freedesktop.secrets", "org.gnome.keyring",
             "org.kde.kwalletd", "org.a11y.", "org.freedesktop.Notifications", "org.kde.StatusNotifierWatcher",
             "org.kde.plasmashell", "org.kde.KSplash", "org.kde.krunner", "org.kde.LogoutPrompt",
             "org.kde.Shutdown", "org.freedesktop.impl.")

ZBUS_CONF = """<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <keep_umask/>
  <listen>unix:path={dir}/bus</listen>
  <auth>EXTERNAL</auth>
  <servicedir>{dir}/services</servicedir>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"""


def service_links():
    """Link the activatable services the zone may start on its own bus."""
    out = f"{ZBUS_DIR}/services"
    os.makedirs(out, exist_ok=True)
    dirs = os.environ.get("XDG_DATA_DIRS", "/usr/share").split(":")
    for d in dirs:
        sd = os.path.join(d, "dbus-1/services")
        try:
            names = sorted(os.listdir(sd))
        except OSError:
            continue
        for n in names:
            src = os.path.join(sd, n)
            name = ""
            try:
                with open(src, errors="replace") as f:
                    for line in f:
                        if line.startswith("Name="):
                            name = line[5:].strip()
            except OSError:
                continue
            if not n.endswith(".service") or not name or name.startswith(ZBUS_SKIP):
                continue
            dst = os.path.join(out, n)
            if not os.path.lexists(dst):  # the first data dir wins, as for the host bus
                os.symlink(os.path.realpath(src), dst)


def ensure_zone_bus():
    """Start the zone's dbus-daemon (once); returns an error string or None."""
    global zbus
    if zbus and zbus.poll() is None:
        return None
    daemon = os.environ.get("ZONE_DBUS_DAEMON") or shutil.which("dbus-daemon")
    if not daemon:
        return "zone bus: no dbus-daemon on this host (install dbus) for bus = \"zone\""
    os.makedirs(ZBUS_DIR, mode=0o700, exist_ok=True)
    try:
        os.unlink(f"{ZBUS_DIR}/bus")
    except FileNotFoundError:
        pass
    with open(f"{ZBUS_DIR}/session.conf", "w") as f:
        f.write(ZBUS_CONF.format(dir=ZBUS_DIR))
    service_links()
    env = dict(os.environ, DBUS_SESSION_BUS_ADDRESS=ZBUS_ADDR)
    r, w = os.pipe()
    try:
        zbus = subprocess.Popen([daemon, f"--config-file={ZBUS_DIR}/session.conf", "--nofork", "--nopidfile",
                                 f"--print-address={w}"], env=env, pass_fds=(w,), stdin=subprocess.DEVNULL,
                                start_new_session=True)
    except OSError as e:
        os.close(r); os.close(w)
        return f"zone bus: {daemon}: {e.strerror or e}"
    os.close(w)
    ready, _, _ = select.select([r], [], [], 5)
    line = os.read(r, 4096) if ready else b""
    os.close(r)
    if not line.strip():
        return "zone bus: dbus-daemon did not start within 5 s"
    log(f"zone bus up ({line.decode().strip()}), pid {zbus.pid}")
    return None


def no_bus(argv):
    """argv wrapped so the app sees no bus socket at all: a nested bwrap (own
    user + mount namespace) whose $XDG_RUNTIME_DIR holds the zone's other
    sockets, not bus and not zone-bus. (zone-init's own socket is not in the
    zone at all.)
    The link socket and the xdg-open shim stay."""
    bwrap = os.environ.get("ZONE_BWRAP") or "bwrap"
    a = [bwrap, "--dev-bind", "/", "/", "--perms", "0700", "--tmpfs", ZRT]
    for n in sorted(os.listdir(ZRT)):
        if n not in ("bus", "zone-bus"):
            a += ["--bind", f"{ZRT}/{n}", f"{ZRT}/{n}"]
    a += ["--tmpfs", "/run/zone"]
    for n in ("bin", "links.sock"):
        if os.path.lexists(f"/run/zone/{n}"):
            a += ["--bind", f"/run/zone/{n}", f"/run/zone/{n}"]
    return a + ["--remount-ro", "/run/zone", "--unsetenv", "DBUS_SESSION_BUS_ADDRESS", "--", *argv]


def read_output(fd):
    pid, label, part = outputs[fd]
    try:
        data = os.read(fd, 65536)
    except BlockingIOError:
        return
    except OSError:
        data = b""
    if not data:
        if part:
            _line(pid, label, bytes(part))
        sel.unregister(fd)
        os.close(fd)
        del outputs[fd]
        return
    part += data
    *lines, rest = bytes(part).split(b"\n")
    part[:] = rest
    for l in lines:
        _line(pid, label, l)


def _line(pid, label, raw):
    text = raw.decode(errors="replace").rstrip("\r")
    tails.setdefault(pid, collections.deque(maxlen=TAIL)).append(text)
    print(f"[{label} {pid}] {text}", flush=True)


def reply(conn, obj):
    try:
        conn.sendall((json.dumps(obj) + "\n").encode())
    except OSError:
        pass


def _is_null(fd):
    st = os.fstat(fd)
    return stat.S_ISCHR(st.st_mode) and os.major(st.st_rdev) == 1 and os.minor(st.st_rdev) == 3


def _stdio_ok(fd):
    """A pipe, a regular file or /dev/null: never a tty, a directory (openat on
    it reaches host files), a socket or another device."""
    try:
        st = os.fstat(fd)
    except OSError:
        return False
    if os.isatty(fd):
        return False
    return stat.S_ISFIFO(st.st_mode) or stat.S_ISREG(st.st_mode) or _is_null(fd)


def _winsize(fd, rows, cols):
    try:
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", int(rows), int(cols), 0, 0))
    except (OSError, ValueError, TypeError):
        pass


def _env(req):
    env = dict(os.environ)
    for k, v in (req.get("env") or {}).items():
        if k in ENV_ALLOW and isinstance(v, str):
            env[k] = v
    return env


def _cwd(req):
    cwd = req.get("cwd") or os.environ.get("HOME", "/")
    return cwd if os.path.isdir(cwd) else os.environ.get("HOME", "/")


def op_run(conn, req, fds):
    argv = req.get("argv") or []
    if not argv or not all(isinstance(a, str) for a in argv):
        for f in fds:
            os.close(f)
        reply(conn, {"ok": False, "error": "argv must be a non-empty list of strings"}); conn.close(); return
    stdio = [None, None, None]
    if req.get("wait"):
        if len(fds) != 3 or not all(_stdio_ok(f) for f in fds):
            for f in fds:
                os.close(f)
            reply(conn, {"ok": False, "error": "wait needs 3 pipe, regular file or /dev/null fds (never a tty, "
                                               "directory or socket)"}); conn.close(); return
        stdio = fds
    else:
        for f in fds:
            os.close(f)
        fds = []
    env, bus, label = _env(req), req.get("bus") or "host", os.path.basename(argv[0])
    if label in MOZ_APPS:
        env["MOZ_DBUS_APP_NAME"] = moz_remoting_name(ZONE, label)
    if bus == "zone":
        err = ensure_zone_bus()
        if err:
            for f in fds:
                os.close(f)
            reply(conn, {"ok": False, "error": err}); conn.close(); return
        env["DBUS_SESSION_BUS_ADDRESS"] = ZBUS_ADDR
    elif bus == "none":
        env.pop("DBUS_SESSION_BUS_ADDRESS", None)
        argv = no_bus(argv)
    elif bus != "host":
        for f in fds:
            os.close(f)
        reply(conn, {"ok": False, "error": f"unknown bus {bus!r} (host, zone, none)"}); conn.close(); return
    out_r = None
    if not req.get("wait"):
        out_r, out_w = os.pipe()
        stdio[1] = stdio[2] = out_w
    try:
        p = subprocess.Popen(argv, env=env, cwd=_cwd(req), stdin=stdio[0] or subprocess.DEVNULL,
                             stdout=stdio[1], stderr=stdio[2], start_new_session=True)
    except OSError as e:
        for f in fds:
            os.close(f)
        if out_r is not None:
            os.close(out_r); os.close(stdio[1])
        reply(conn, {"ok": False, "error": f"{argv[0]}: {e.strerror or e}"}); conn.close()
        return
    for f in fds:
        os.close(f)
    if out_r is not None:
        os.close(stdio[1])
        os.set_blocking(out_r, False)
        outputs[out_r] = (p.pid, label, bytearray())
        sel.register(out_r, selectors.EVENT_READ, ("out", out_r))
    running[p.pid] = p
    log(f"run {label} pid {p.pid} (bus {bus})")
    reply(conn, {"ok": True, "pid": p.pid})
    if req.get("wait"):
        waiting[p.pid] = conn
    else:
        conn.close()


def op_shell(conn, req, fds):
    for f in fds:
        os.close(f)
    argv = req.get("argv") or SHELL
    env = _env(req)
    env["TERM"] = req.get("term") if isinstance(req.get("term"), str) else env.get("TERM", "xterm-256color")
    pid, master = pty.fork()
    if pid == 0:  # child: the pty slave is our controlling tty
        try:
            os.chdir(_cwd(req))
            os.execvpe(argv[0], argv, env)
        finally:
            os._exit(127)
    _winsize(master, req.get("rows", 24), req.get("cols", 80))
    os.set_blocking(master, False)
    shells[pid] = (conn, master)
    reply(conn, {"ok": True, "pid": pid})
    sel.register(conn, selectors.EVENT_READ, ("shell-in", pid))
    sel.register(master, selectors.EVENT_READ, ("shell-out", pid))
    log(f"shell pid {pid}")


def close_shell(pid):
    conn, master = shells.pop(pid, (None, None))
    if conn is None:
        return
    for f in (conn, master):
        try:
            sel.unregister(f)
        except (KeyError, ValueError):
            pass
    try:
        conn.close()
    except OSError:
        pass
    try:
        os.close(master)  # the shell gets SIGHUP if it still runs
    except OSError:
        pass


def host_mounts():
    """[[path, "ro"|"rw"], ...] for the runtime mounts under /mnt/host, as this zone sees them."""
    out = []
    with open("/proc/self/mountinfo") as f:
        for line in f:
            p = line.split()
            mp = p[4].replace("\\040", " ")
            if mp.startswith("/mnt/host/"):
                out.append([mp, "ro" if "ro" in p[5].split(",") else "rw"])
    return out


def busy(path):
    """[[pid, cmdline, what], ...]: processes in the zone with a cwd, root,
    executable, open file or mapped file under PATH."""
    out = []
    for p in sorted(int(x) for x in os.listdir("/proc") if x.isdigit()):
        hits = []
        links = [("cwd", f"/proc/{p}/cwd"), ("root", f"/proc/{p}/root"), ("exe", f"/proc/{p}/exe")]
        try:
            links += [(f"fd {fd}", f"/proc/{p}/fd/{fd}") for fd in os.listdir(f"/proc/{p}/fd")]
        except OSError:
            pass
        for what, link in links:
            try:
                t = os.readlink(link)
            except OSError:
                continue
            if t == path or t.startswith(path + "/"):
                hits.append(f"{what} {t}")
        try:
            with open(f"/proc/{p}/maps") as f:
                for line in f:
                    t = line.split(None, 5)[5].strip() if len(line.split(None, 5)) == 6 else ""
                    if t == path or t.startswith(path + "/"):
                        hits.append(f"maps {t}")
                        break
        except OSError:
            pass
        if hits:
            try:
                with open(f"/proc/{p}/cmdline", "rb") as f:
                    cmd = f.read().replace(b"\0", b" ").decode(errors="replace").strip()
            except OSError:
                cmd = "?"
            out.append([p, cmd, ", ".join(hits[:3]) + (f" (+{len(hits) - 3})" if len(hits) > 3 else "")])
    return out


def handle(conn: socket.socket):
    conn.settimeout(5)  # a client that connects and says nothing cannot hold zone-init up
    try:
        data, fds, _, _ = socket.recv_fds(conn, 65536, 3)
    except (TimeoutError, OSError):
        conn.close()
        return
    conn.settimeout(None)
    try:
        req = json.loads(data.decode().splitlines()[0])
    except (ValueError, IndexError):
        for f in fds:
            os.close(f)
        reply(conn, {"ok": False, "error": "bad request"}); conn.close(); return
    op = req.get("op")
    if op == "ping":
        reply(conn, {"ok": True, "zone": ZONE, "pid": os.getpid()}); conn.close()
    elif op == "ps":
        procs = []
        for p in sorted(int(x) for x in os.listdir("/proc") if x.isdigit()):
            try:
                with open(f"/proc/{p}/cmdline", "rb") as f:
                    cmd = f.read().replace(b"\0", b" ").decode(errors="replace").strip()
            except OSError:
                continue
            procs.append([p, cmd])
        helpers = [zbus.pid] if zbus and zbus.poll() is None else []  # not apps (linger)
        reply(conn, {"ok": True, "procs": procs, "init": os.getpid(), "helpers": helpers}); conn.close()
    elif op == "run":
        op_run(conn, req, fds)
        return
    elif op == "shell":
        op_shell(conn, req, fds)
        return
    elif op == "status":
        pid = req.get("pid")
        for fd, (p, _, _) in list(outputs.items()):
            if p == pid:
                read_output(fd)  # whatever it wrote last
        if pid in running:
            reply(conn, {"ok": True, "running": True, "exit": None, "tail": list(tails.get(pid, []))})
        elif pid in finished:
            code, tail = finished[pid]
            reply(conn, {"ok": True, "running": False, "exit": code, "tail": tail})
        else:
            reply(conn, {"ok": False, "error": f"no process {pid!r} started by zone-init"})
        conn.close()
    elif op == "mounts":
        reply(conn, {"ok": True, "mounts": host_mounts()}); conn.close()
    elif op == "busy":
        path = req.get("path")
        if not isinstance(path, str) or not path.startswith("/mnt/host/"):
            reply(conn, {"ok": False, "error": "busy: path must be under /mnt/host/"})
        else:
            reply(conn, {"ok": True, "procs": busy(path)})
        conn.close()
    elif op == "resize":
        sh = shells.get(req.get("pid"))
        if sh:
            _winsize(sh[1], req.get("rows", 24), req.get("cols", 80))
            os.kill(req["pid"], signal.SIGWINCH)
        reply(conn, {"ok": bool(sh)}); conn.close()
    else:
        reply(conn, {"ok": False, "error": f"unknown op {op!r}"}); conn.close()
    for f in fds:
        os.close(f)


def relay(kind, pid):
    conn, master = shells.get(pid, (None, None))
    if conn is None:
        return
    try:
        if kind == "shell-in":
            data = conn.recv(65536)
            if not data:
                close_shell(pid); return
            os.set_blocking(master, True)
            os.write(master, data)
            os.set_blocking(master, False)
        else:
            data = os.read(master, 65536)
            if not data:
                close_shell(pid); return
            conn.sendall(data)
    except BlockingIOError:
        pass
    except OSError:  # EIO: the shell side closed
        close_shell(pid)


def reap():
    while True:
        try:
            pid, status = os.waitpid(-1, os.WNOHANG)
        except ChildProcessError:
            return
        if pid == 0:
            return
        code = os.waitstatus_to_exitcode(status)
        if zbus and pid == zbus.pid:
            zbus.returncode = code
            log(f"zone bus exited ({code}); it starts again on the next run with bus = zone")
        if pid in running:
            running.pop(pid).returncode = code
            for fd, (p, _, _) in list(outputs.items()):
                if p == pid:
                    read_output(fd)
            finished[pid] = (code, list(tails.pop(pid, [])))
            while len(finished) > 64:
                finished.popitem(last=False)
            if code != 0:
                log(f"pid {pid} exited with status {code}")
        conn = waiting.pop(pid, None)
        if conn:
            reply(conn, {"exit": code}); conn.close()
        if pid in shells:
            conn, master = shells[pid]
            # flush what the shell wrote last
            try:
                while True:
                    data = os.read(master, 65536)
                    if not data:
                        break
                    conn.sendall(data)
            except OSError:
                pass
            close_shell(pid)


def harden_self():
    """Named `zone-init` (comm). Not dumpable: processes in the zone (same uid, no capabilities) cannot
    ptrace zone-init, read its memory or take its fds (pidfd_getfd), so they
    cannot use its control socket. Apps it starts are dumpable again after exec."""
    PR_SET_NAME, PR_SET_DUMPABLE = 15, 4
    import ctypes
    libc = ctypes.CDLL(None, use_errno=True)
    # the process name the host finds zone-init by (`pgrep -x zone-init`), whatever it is
    # written in; before PR_SET_DUMPABLE, which makes /proc/self/comm root's
    libc.prctl(PR_SET_NAME, ctypes.c_char_p(b"zone-init"), 0, 0, 0)
    if libc.prctl(PR_SET_DUMPABLE, 0, 0, 0, 0) != 0:
        log("could not make zone-init non-dumpable; refusing to run")
        sys.exit(1)


def listener() -> socket.socket:
    """The control socket the host handed over ($ZONE_CTL_FD); never a path in the zone."""
    raw = os.environ.pop("ZONE_CTL_FD", "")
    if not raw.isdigit():
        log("no $ZONE_CTL_FD from the host; refusing to run")
        sys.exit(1)
    fd = int(raw)
    os.set_inheritable(fd, False)  # never into an app
    return socket.socket(fileno=fd)


def server_watch() -> int:
    """The pidfd of this zone's server ($ZONE_SERVER_FD, from the host): it becomes
    readable when the server exits, and then the zone ends too."""
    raw = os.environ.pop("ZONE_SERVER_FD", "")
    if not raw.isdigit():
        log("no $ZONE_SERVER_FD from the host; refusing to run")
        sys.exit(1)
    fd = int(raw)
    os.set_inheritable(fd, False)  # never into an app
    return fd


# ── DNS (network = "internet") ─────────────────────────────────────────────
# zone-init answers DNS at $ZONE_DNS (127.0.0.53) and hands each query over the
# host's /run/zone/dns.sock to the zone's server, which asks the host's resolver
# (appzones/dns.py). Not pasta's forwarder: that needs a recent pasta.
DNS_SOCK = "/run/zone/dns.sock"
DNS_MAX = 4096
dns_slots = threading.BoundedSemaphore(64)


def dns_forward(msg):
    """The answer to MSG from the host side, or None."""
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
            s.settimeout(12)  # longer than the host side's deadline for one query (8 s)
            s.connect(DNS_SOCK)
            s.sendall(struct.pack(">H", len(msg)) + msg)
            head = _recv_exact(s, 2)
            return _recv_exact(s, struct.unpack(">H", head)[0]) if head else None
    except OSError:
        return None


def _recv_exact(s, n):
    buf = b""
    while len(buf) < n:
        chunk = s.recv(n - len(buf))
        if not chunk:
            return None
        buf += chunk
    return buf


def _skip_name(msg, i):
    """The offset after the domain name at I (labels, or a compression pointer)."""
    while i < len(msg):
        n = msg[i]
        if n == 0:
            return i + 1
        if n & 0xC0 == 0xC0:
            return i + 2
        i += 1 + n
    raise ValueError("name past the end")


def udp_limit(query):
    """The largest UDP answer the client takes: its EDNS size (OPT record), else 512."""
    try:
        qd, an, ns, ar = struct.unpack(">HHHH", query[4:12])
        i = 12
        for _ in range(qd):
            i = _skip_name(query, i) + 4
        for _ in range(an + ns + ar):
            j = _skip_name(query, i)
            rtype, rclass = struct.unpack(">HH", query[j:j + 4])
            if rtype == 41:  # OPT: the class is the UDP size
                return max(512, min(rclass, DNS_MAX))
            i = j + 8 + 2 + struct.unpack(">H", query[j + 8:j + 10])[0]
    except (ValueError, struct.error, IndexError):
        pass
    return 512


def truncate(answer):
    """ANSWER cut to its header and question, with TC set: the client asks again over TCP."""
    try:
        qd = struct.unpack(">H", answer[4:6])[0]
        i = 12
        for _ in range(qd):
            i = _skip_name(answer, i) + 4
        return answer[:2] + bytes([answer[2] | 0x02, answer[3]]) + struct.pack(">HHHH", qd, 0, 0, 0) + answer[12:i]
    except (ValueError, struct.error, IndexError):
        return answer[:2] + bytes([answer[2] | 0x02, answer[3]]) + struct.pack(">HHHH", 0, 0, 0, 0)


def _dns_udp_one(sock, query, addr):
    try:
        ans = dns_forward(query)
        if ans:
            sock.sendto(ans if len(ans) <= udp_limit(query) else truncate(ans), addr)
    except OSError:
        pass
    finally:
        dns_slots.release()


def _dns_udp(sock):
    while True:
        try:
            query, addr = sock.recvfrom(DNS_MAX)
        except OSError:
            continue
        if len(query) < 12 or not dns_slots.acquire(blocking=False):
            continue  # too many at once: the client asks again
        threading.Thread(target=_dns_udp_one, args=(sock, query, addr), daemon=True).start()


def _dns_tcp_conn(conn):
    try:
        with conn:
            conn.settimeout(10)
            while True:
                head = _recv_exact(conn, 2)
                if not head:
                    return
                n = struct.unpack(">H", head)[0]
                query = _recv_exact(conn, n) if n <= DNS_MAX else None
                ans = dns_forward(query) if query else None
                if not ans:
                    return
                conn.sendall(struct.pack(">H", len(ans)) + ans)
    except OSError:
        pass
    finally:
        dns_slots.release()


def _dns_tcp(lsock):
    while True:
        try:
            conn, _ = lsock.accept()
        except OSError:
            continue
        if not dns_slots.acquire(blocking=False):
            conn.close()
            continue
        threading.Thread(target=_dns_tcp_conn, args=(conn,), daemon=True).start()


def start_dns(addr):
    """Answer DNS at ADDR:53, UDP and TCP, in threads."""
    try:
        u = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        u.bind((addr, 53))
        t = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        t.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        t.bind((addr, 53))
        t.listen(16)
    except OSError as e:
        log(f"DNS at {addr}:53 not started: {e.strerror or e}; names do not resolve in this zone")
        return
    threading.Thread(target=_dns_udp, args=(u,), daemon=True).start()
    threading.Thread(target=_dns_tcp, args=(t,), daemon=True).start()


def main():
    harden_self()
    if os.environ.get("ZONE_DNS"):
        start_dns(os.environ["ZONE_DNS"])
    srv = listener()
    server = server_watch()
    r, w = os.pipe()
    os.set_blocking(w, False)
    signal.set_wakeup_fd(w)
    signal.signal(signal.SIGCHLD, lambda *_: None)
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    sel.register(srv, selectors.EVENT_READ, ("srv", 0))
    sel.register(r, selectors.EVENT_READ, ("sig", 0))
    sel.register(server, selectors.EVENT_READ, ("server", 0))
    log(f"zone {ZONE} ready (control socket: fd from the host)")
    while True:
        for key, _ in sel.select():
            kind, pid = key.data
            if kind == "srv":
                conn, _ = srv.accept()
                try:
                    handle(conn)
                except Exception as e:  # never let one bad request kill the zone
                    log(f"request failed: {e}")
                    conn.close()
            elif kind == "sig":
                os.read(r, 512)
                reap()
            elif kind == "server":
                # the server is gone: no one reads our log now, and a blocked write
                # must not keep the zone alive; end it at once
                os._exit(0)
            elif kind == "out":
                if pid in outputs:
                    read_output(pid)
            else:
                relay(kind, pid)


if __name__ == "__main__":
    main()
