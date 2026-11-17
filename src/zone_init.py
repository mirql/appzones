"""zone-init: the agent inside a zone.

Runs as PID 2 of the zone's pid namespace (bwrap is PID 1 and reaps).
Listens on /run/zone/ctl.sock. One JSON request line per connection:

  {"op": "ping"}                         -> {"ok": true, "zone": Z, "pid": N}
  {"op": "ps"}                           -> {"ok": true, "procs": [[pid, cmdline], ...]}
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
        a runtime mount (cwd, root, exe, open files, mapped files), for `zone umount`

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
import signal
import socket
import stat
import struct
import subprocess
import sys
import termios

SOCK = "/run/zone/ctl.sock"
ZONE = os.environ.get("ZONE", "?")
# Mozilla apps own org.mozilla.<remoting name>.<profile> on the host bus for
# their "already running" remote. Every zone's profile is named the same, so
# each zone's app gets its own D-Bus app name (MOZ_DBUS_APP_NAME), and the proxy lets the zone own
# only that one (zone.py dbus_rules): a second launch reaches this zone's
# Firefox and never another zone's or the host's. Keep in step with zone.py.
MOZ_APPS = ("firefox", "firefox-esr", "firefox-beta", "firefox-devedition", "firefox-nightly", "librewolf")


def moz_remoting_name(zone, app):
    return f"{app.replace('-', '_')}_zone_{zone.replace('-', '_')}"


ENV_ALLOW = {"TERM", "COLUMNS", "LINES", "LANG", "LC_ALL", "MOZ_HEADLESS", "NO_AT_BRIDGE"}
SHELL = ["/run/current-system/sw/bin/bash", "-i"]

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
    dirs = os.environ.get("XDG_DATA_DIRS", "/run/current-system/sw/share").split(":")
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
    daemon = os.environ.get("ZONE_DBUS_DAEMON") or "/run/current-system/sw/bin/dbus-daemon"
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
    sockets, not bus and not zone-bus, and whose /run/zone has no ctl.sock
    (through it the app could ask zone-init for a process on the host bus).
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
    data, fds, _, _ = socket.recv_fds(conn, 65536, 3)
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
        reply(conn, {"ok": True, "procs": procs, "init": os.getpid()}); conn.close()
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


def main():
    os.makedirs(os.path.dirname(SOCK), exist_ok=True)
    try:
        os.unlink(SOCK)
    except FileNotFoundError:
        pass
    srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    srv.bind(SOCK)
    os.chmod(SOCK, 0o600)
    srv.listen(16)
    r, w = os.pipe()
    os.set_blocking(w, False)
    signal.set_wakeup_fd(w)
    signal.signal(signal.SIGCHLD, lambda *_: None)
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    sel.register(srv, selectors.EVENT_READ, ("srv", 0))
    sel.register(r, selectors.EVENT_READ, ("sig", 0))
    log(f"zone {ZONE} ready on {SOCK}")
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
            elif kind == "out":
                if pid in outputs:
                    read_output(pid)
            else:
                relay(kind, pid)


if __name__ == "__main__":
    main()
