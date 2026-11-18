"""The user commands: up, run, shell, stop, list, which, new, check, apps, autostart, active, logs."""
from __future__ import annotations

import fcntl
import json
import os
import secrets
import select
import shlex
import shutil
import signal
import socket
import stat
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import zonecfg

from .common import die, load_cfg, load_zone, settings_hash, systemctl, use_systemd, warn
from . import agent, common, server


def up(name: str, quiet: bool = False) -> None:
    if agent.ping(name):
        return
    load_zone(name)
    if not use_systemd():
        spawn(name, quiet)
        return
    if not quiet:
        warn(f"starting zone {name}")
    r = systemctl("start", f"appzones@{name}.service", check=False)
    if r.returncode != 0:
        die(f"zone {name} failed to start; see `appzones logs {name}`\n{r.stderr.strip()}")
    for _ in range(200):
        if agent.ping(name):
            return
        time.sleep(0.05)
    die(f"zone {name} did not answer within 10 s; see `appzones logs {name}`")


def _log_tail(name: str, lines: int = 15) -> str:
    """The last LINES of the zone's log, from a bounded read at its end."""
    try:
        with open(common.log_path(name), "rb") as f:
            f.seek(max(0, f.seek(0, os.SEEK_END) - 16384))
            text = f.read().decode(errors="replace")
    except OSError:
        return ""
    return "\n".join("  " + l for l in text.splitlines()[-lines:])


def _open_log(name: str) -> int:
    """The zone's log for a server we start: appended, rotated once at start when big."""
    common.LOG_DIR.mkdir(parents=True, exist_ok=True)
    path = common.log_path(name)
    try:
        if path.stat().st_size > common.LOG_MAX:
            path.replace(path.with_name(path.name + ".1"))
    except FileNotFoundError:
        pass
    return os.open(path, os.O_WRONLY | os.O_CREAT | os.O_APPEND | os.O_CLOEXEC | os.O_NOFOLLOW, 0o600)


def _start_server(name: str, lock: int) -> int:
    """Start `appzones serve NAME` detached (its own session, cwd /, stdin /dev/null, the
    log for output), handing it the LOCK we hold and a readiness pipe; returns the pipe's
    read end. A client killed after this does not stop the server."""
    if not zonecfg.NAME_RE.fullmatch(name):
        die(f"not a zone name: {name!r}")
    try:
        log = _open_log(name)
    except OSError as e:
        die(f"zone {name}: cannot open its log {common.log_path(name)}: {e.strerror or e}")
    r, w = os.pipe()
    env = {k: v for k, v in os.environ.items() if k not in ("NOTIFY_SOCKET", "LISTEN_FDS", "LISTEN_PID")}
    # the server runs in /: the config and state it reads must not be relative paths
    env["APPZONES_CONFIG"] = str(Path(common.CONFIG).absolute())
    env.pop("ZONES_CONFIG", None)
    if zonecfg.env("STATE"):
        env["APPZONES_STATE"] = str(Path(common.STATE).absolute())
        env.pop("ZONES_STATE", None)
    argv = [sys.executable, "-B", str(common.HERE / "zone.py"), "serve", name, "--lock-fd", str(lock),
            "--notify-fd", str(w)]
    pid = os.fork()
    if pid == 0:  # the intermediate: start the server in a new session, then go
        try:
            subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=log, stderr=log, cwd="/", env=env,
                             pass_fds=(lock, w), start_new_session=True)
            os._exit(0)
        except BaseException:
            os._exit(1)
    os.close(w)
    os.close(log)
    _, status = os.waitpid(pid, 0)
    if os.waitstatus_to_exitcode(status) != 0:
        os.close(r)
        die(f"zone {name}: could not start its server")
    return r


def spawn(name: str, quiet: bool = False) -> None:
    """Without systemd: start zone NAME's server ourselves, or wait for the one that is
    starting. The zone's lock decides who starts it: exactly one server per zone."""
    r = claim(name, quiet)
    if r is not None:
        wait_ready(name, r, quiet)


def claim(name: str, quiet: bool = False, wait: bool = True) -> int | None:
    """Start zone NAME's server if nobody runs or starts it: its readiness pipe, or None
    when the zone answers (or, with WAIT, once it does; without WAIT, at once when
    someone else is starting it)."""
    path = server.lock_path(name)
    common.RT.mkdir(parents=True, exist_ok=True)
    os.chmod(common.RT, 0o700)
    started = time.monotonic()
    waited_note = False
    while True:
        if agent.ping(name):
            return None
        lock = os.open(path, os.O_RDWR | os.O_CREAT | os.O_CLOEXEC | os.O_NOFOLLOW, 0o600)
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            os.close(lock)  # someone else starts it, or runs it: wait for it to answer
            if not wait:
                return None
            if time.monotonic() - started > 2 * server.START_TIMEOUT:
                pids = server_pids(name)
                die(f"zone {name} is held by a server" + (f" (pid {', '.join(map(str, pids))})" if pids else "")
                    + f" but does not answer; see `appzones logs {name}`")
            if not waited_note and not quiet and time.monotonic() - started > 2:
                warn(f"waiting for zone {name} to start")
                waited_note = True
            time.sleep(0.1)
            continue
        if agent.ping(name):  # answered in between (an older server without a lock)
            os.close(lock)
            return None
        break
    if not quiet:
        warn(f"starting zone {name}")
    try:
        return _start_server(name, lock)
    finally:
        os.close(lock)  # the server holds it now


def wait_ready(name: str, r: int, quiet: bool = False) -> None:
    """Until zone NAME's server signals readiness on R; dies with the end of its log when
    it ended first. No total limit: the server has per-step deadlines, packages may build."""
    try:
        note = time.monotonic() + 3
        poll = select.poll()
        poll.register(r, select.POLLIN)
        while True:
            ready = poll.poll(1000)
            if ready:
                if os.read(r, 1) == b"\n":
                    return
                die(f"zone {name} did not start; the end of its log ({common.log_path(name)}):\n{_log_tail(name)}")
            if not quiet and note and time.monotonic() > note:
                warn(f"zone {name} is still starting (Ctrl-C leaves it starting; log: {common.log_path(name)})")
                note = 0
    finally:
        os.close(r)


def stale(name: str, z: zonecfg.Zone) -> bool:
    try:
        return (common.RT / name / "config.sha256").read_text().strip() != settings_hash(z)
    except OSError:
        return False


def outside_paths(z: zonecfg.Zone, args: list[str]) -> list[str]:
    """Arguments that name an existing host file the zone cannot see."""
    visible = [Path(os.path.realpath(zonecfg.expand(b["path"]))) for b in z.settings["binds"]]
    bad = []
    for a in args:
        p = a[7:] if a.startswith("file://") else a
        if not p.startswith("/") or p.startswith(("/nix/store", "/dev/null")) or not os.path.exists(p):
            continue
        rp = Path(os.path.realpath(p))
        if not any(rp == v or rp.is_relative_to(v) for v in visible):
            bad.append(a)
    return bad


def run(name: str, words: list[str], wait: bool, restart: bool, bus: str | None = None) -> int:
    cfg = load_cfg(quiet=True)
    if name not in cfg.zones:
        die(f"no zone {name!r} in {common.CONFIG}")
    z = cfg.zones[name]
    if not words:
        die("usage: appzones run ZONE APP [ARGS...] | appzones run ZONE -- CMD [ARGS...]")
    if words[0] == "--":
        argv = words[1:]
        if not argv:
            die("nothing to run after --")
        app = zonecfg.app_for_command(cfg, argv[0])
    else:
        app = words[0]
        try:
            base = zonecfg.resolve_app(cfg, name, words[0])
        except ValueError as e:
            die(f"{e}; to run it anyway: appzones run {name} -- {' '.join(words)}")
        if base is None:
            die(f"no app {words[0]!r} in [apps]; to run a command: appzones run {name} -- {' '.join(words)}")
        argv = base + words[1:]
    bus = bus or zonecfg.app_bus(cfg, name, app)
    if bus == "none" and not z.settings["userns"]:
        die(f"--bus none needs userns = true in zone {name} (the app gets its own mount namespace)")
    bad = outside_paths(z, argv[1:])
    if bad:
        die(f"zone {name} cannot see {', '.join(bad)} (not inside its binds); "
            f"move the file into a bind or use the app's file chooser")
    if agent.ping(name) and stale(name, z):
        n = len(agent.zone_procs(name))
        if restart and n == 0:
            warn(f"zones.toml changed for {name}; restarting it (no app runs in it)")
            restart_zone(name)
        elif restart:
            warn(f"zones.toml changed for {name}, but {n} process(es) run in it; not restarting "
                 f"(`appzones restart {name}` applies it)")
        else:
            warn(f"zones.toml changed for {name}; `appzones restart {name}` (or `appzones run --restart`) applies it")
    up(name, quiet=wait)
    cwd = os.getcwd()
    cwd = cwd if any(Path(cwd).is_relative_to(Path(zonecfg.expand(b["path"]))) for b in z.settings["binds"]) else None
    req = {"op": "run", "argv": argv, "wait": wait, "cwd": cwd, "bus": bus,
           "env": {k: os.environ[k] for k in ("TERM", "MOZ_HEADLESS", "COLUMNS", "LINES") if k in os.environ}}
    if wait:
        fds = [0, 1, 2]
        if any(os.isatty(f) for f in fds):
            die("--wait passes stdin/stdout/stderr into the zone and refuses a tty; use pipes, or `appzones shell`")
        bad = [n for n, f in zip(("stdin", "stdout", "stderr"), fds) if not stdio_fd_ok(f)]
        if bad:
            die(f"--wait passes only pipes, regular files and /dev/null into the zone; {', '.join(bad)} is "
                f"something else (a directory or socket would hand the zone a way to the host)")
        s = agent.request(name, req, fds, timeout=None)
    else:
        s = agent.request(name, req)
    first, rest = agent.readline(s)
    if not first.get("ok"):
        die(first.get("error", "run failed"))
    if not wait:
        s.close()
        return watch(name, first["pid"], argv, bus, app)
    return agent.readline(s, rest)[0].get("exit", 255)


def stdio_fd_ok(fd: int) -> bool:
    """A pipe, a regular file or /dev/null: what --wait may hand into a zone
    (zone-init checks the same on its side)."""
    try:
        st = os.fstat(fd)
    except OSError:
        return False
    if stat.S_ISFIFO(st.st_mode) or stat.S_ISREG(st.st_mode):
        return True
    return stat.S_ISCHR(st.st_mode) and os.major(st.st_rdev) == 1 and os.minor(st.st_rdev) == 3


# What a GApplication (and others) print when the bus refuses its name.
DBUS_OWN_FAILED = ("Failed to register", "org.freedesktop.DBus.Error.ServiceUnknown",
                   "org.freedesktop.DBus.Error.AccessDenied", "Unable to acquire bus name", "Could not acquire name",
                   "RequestName")


def watch(name: str, pid: int, argv: list[str], bus: str, app: str | None) -> int:
    """Watch a launched app for a few seconds; if it exits with an error (or
    with 0 after a refused bus name), print its status and last output, and
    a hint for a refused bus name. An app that hands over to a running
    instance exits 0: that is quiet."""
    try:
        secs = float(zonecfg.env("RUN_CHECK") or "2.5")
    except ValueError:
        secs = 2.5
    end, st = time.time() + secs, None
    while time.time() < end:
        try:
            with agent.request(name, {"op": "status", "pid": pid}, timeout=2) as s:
                st = agent.readline(s)[0]
        except (OSError, ValueError):
            return 0
        if not st.get("ok") or not st.get("running"):
            break
        time.sleep(0.1)
    if not st or not st.get("ok") or st.get("running"):
        return 0
    code, tail, label = st["exit"], st.get("tail") or [], os.path.basename(argv[0])
    refused = any(h in line for line in tail for h in DBUS_OWN_FAILED)
    # Thunar exits 0 after "Failed to register": status 0 is only quiet when
    # the output does not show a refused bus name
    if code == 0 and not refused:
        return 0
    how = f"was killed by signal {-code}" if code < 0 else f"exited with status {code}"
    warn(f"{label} {how} right after it started in zone {name} (bus {bus}); its last output:")
    for line in tail[-12:] or ["(nothing)"]:
        print(f"  {line}", file=sys.stderr)
    if bus == "host" and refused:
        where = (f"bus = \"zone\" in [apps.{app}]" if app else
                 f"an [apps] entry for it with bus = \"zone\" (command = [\"{label}\"])")
        warn(f"hint: {label} could not own its D-Bus name. A single-instance app needs one, and a zone never "
             f"owns names on the host bus. Give it the zone's own bus: add {where} to {common.CONFIG}, "
             f"or once: appzones run --bus zone {name} -- {' '.join(argv)}")
    print(f"  (the whole log: appzones logs {name})", file=sys.stderr)
    return code if code > 0 else 1


def shell(name: str) -> int:
    import termios
    import tty
    if not os.isatty(0):
        die("appzones shell needs a terminal (for a command with pipes: appzones run --wait ZONE -- CMD)")
    up(name)
    cols, rows = shutil.get_terminal_size()
    s = agent.request(name, {"op": "shell", "rows": rows, "cols": cols, "term": os.environ.get("TERM", "xterm-256color")},
                timeout=None)
    first, rest = agent.readline(s)
    if not first.get("ok"):
        die(first.get("error", "shell failed"))
    pid = first["pid"]
    out = sys.stdout.fileno()
    if rest:
        os.write(out, rest)

    def resize(*_):
        c, r = shutil.get_terminal_size()
        try:
            with agent.request(name, {"op": "resize", "pid": pid, "rows": r, "cols": c}, timeout=1) as rs:
                agent.readline(rs)
        except OSError:
            pass

    signal.signal(signal.SIGWINCH, resize)
    old = termios.tcgetattr(0)
    try:
        tty.setraw(0, termios.TCSANOW)  # not TCSAFLUSH: keep what was typed while the zone started
        while True:
            try:
                r, _, _ = select.select([0, s], [], [])
            except InterruptedError:
                continue
            if s in r:
                data = s.recv(65536)
                if not data:
                    break
                os.write(out, data)
            if 0 in r:
                data = os.read(0, 65536)
                if not data:
                    break
                s.sendall(data)
    finally:
        termios.tcsetattr(0, termios.TCSADRAIN, old)
    return 0


def each_zone(cmd: str, rest: list[str], fn) -> int:
    """`appzones up|stop|restart ZONE|--all`: FN for one zone or every configured one.
    `stop --all` skips zones that do not answer (nothing to stop); `stop ZONE` always tries."""
    if not rest:
        die(f"usage: appzones {cmd} ZONE|--all")
    every = rest[0] == "--all"
    failed = 0
    for n in list(load_cfg(quiet=True).zones) if every else [rest[0]]:
        if cmd == "stop" and every and not agent.ping(n):
            continue
        load_zone(n)
        try:
            fn(n)
        except SystemExit as e:  # said why already; go on with the other zones
            if e.code:
                failed += 1
    return 1 if failed else 0


def _is_server(pid: int, name: str) -> bool:
    """PID is zone NAME's server, as this user (common.server_command)."""
    d = Path(f"/proc/{pid}")
    try:
        if d.stat().st_uid != common.UID:
            return False
        argv = [a.decode(errors="replace") for a in (d / "cmdline").read_bytes().split(b"\0")]
        cwd = os.readlink(d / "cwd")
    except OSError:
        return False
    found = common.server_command(argv, cwd)
    return found is not None and found[1] == name


def server_pids(name: str) -> list[int]:
    """The processes that are zone NAME's server (normally none or one)."""
    out = []
    for d in Path("/proc").iterdir():
        if d.name.isdigit() and int(d.name) != os.getpid() and _is_server(int(d.name), name):
            out.append(int(d.name))
    return out


def stop_zone(name: str) -> None:
    if use_systemd():
        systemctl("stop", f"appzones@{name}.service")
    else:
        pfds = []
        for pid in server_pids(name):
            try:
                pfd = os.pidfd_open(pid)
            except OSError:
                continue  # gone already
            try:
                if _is_server(pid, name):  # still the same process: the pidfd pins it
                    signal.pidfd_send_signal(pfd, signal.SIGTERM)
                    pfds.append(pfd)
                    continue
            except ProcessLookupError:
                pass  # exited just now
            except OSError as e:
                warn(f"zone {name}: could not signal its server (pid {pid}): {e.strerror or e}")
            os.close(pfd)
        # until the servers have exited: their cleanup must not overlap a new start (restart)
        try:
            end = time.monotonic() + 20
            poll = select.poll()
            for p in pfds:
                poll.register(p, select.POLLIN)  # a pidfd is readable once the process exits
            while pfds:
                ready = poll.poll(max(0, int((end - time.monotonic()) * 1000)))
                if not ready:
                    die(f"zone {name}: its server did not exit within 20 s after SIGTERM")
                for p, _ in ready:
                    poll.unregister(p)
                    pfds.remove(p)
                    os.close(p)
        finally:
            for p in pfds:
                os.close(p)
    for _ in range(100):
        if not agent.ping(name):
            return
        time.sleep(0.1)
    die(f"zone {name} still answers: it runs outside {'its unit' if use_systemd() else 'appzones'} "
        f"(an `appzones serve {name}` in a terminal or another process manager); stop it there "
        "(SIGTERM or Ctrl-C)")


def restart_zone(name: str) -> None:
    if use_systemd():
        systemctl("restart", f"appzones@{name}.service")
        for _ in range(200):
            if agent.ping(name):
                return
            time.sleep(0.05)
        return
    stop_zone(name)
    up(name, quiet=True)


def zone_list(as_json: bool = False) -> int:
    cfg = load_cfg(quiet=True)
    rows = [("ZONE", "STATE", "NETWORK", "DNS", "DEVICES", "APPS", "COLOUR")]
    zones = []
    for n, z in cfg.zones.items():
        s = z.settings
        net = s["network"] + ("+netbird" if s["netbird"] else "")
        if agent.ping(n):
            try:
                apps_count = len(agent.zone_procs(n))
                apps_table, apps_json = str(apps_count), apps_count
            except (OSError, ValueError):  # gone, or stopped mid-reply: unknown
                apps_table, apps_json = "?", None
            state = "running" + (" (stale)" if stale(n, z) else "")
        else:
            apps_table, apps_json = "-", None
            state = "stopped"
        devs = [f"usb({'+'.join(s['usb'])})" if d == "usb" else d for d in s["devices"]]
        rows.append((n, state, net, s["dns"], ",".join(devs) or "-", apps_table, s["colour"]))
        zones.append({"name": n, "state": state, "network": net, "dns": s["dns"],
                      "devices": devs, "apps": apps_json, "colour": s["colour"]})
    if as_json:
        print(json.dumps(zones))
        return 0
    w = [max(len(r[i]) for r in rows) for i in range(len(rows[0]))]
    for r in rows:
        print("  ".join(c.ljust(w[i]) for i, c in enumerate(r)).rstrip())
    return 0


def which(name: str) -> int:
    z = load_zone(name, quiet=False)
    for k in zonecfg.KEYS:
        print(f"{k:15} {json.dumps(z.settings[k]):45} ({z.origin[k]})")
    return 0


NEW_BLOCK = """
[zones.{name}]                 # added by `appzones new`: internet only, gpu, nothing from your home
colour    = "{colour}"
network   = "internet"
dns       = "host"
devices   = ["gpu"]
binds     = []
"""


NEW_FILE = """# ~/.config/appzones/zones.toml (written by `appzones new`; reference: https://mirql.github.io/appzones/configuration/reference.html)
version = 1
"""


def new(name: str, write: bool) -> int:
    if not zonecfg.NAME_RE.match(name) or name in zonecfg.RESERVED:
        die(f"zone name {name!r} must match {zonecfg.NAME_RE.pattern} and not be one of {sorted(zonecfg.RESERVED)}")
    if not write:
        cfg = load_cfg(quiet=True) if os.path.lexists(common.CONFIG) else zonecfg.Config({}, {}, [])
        if name in cfg.zones:
            die(f"zone {name!r} already exists")
        print(new_block(cfg, name).strip())
        print(f"\n(not written; `appzones new {name} -y` appends it to {common.CONFIG})", file=sys.stderr)
        return 0
    real = Path(os.path.realpath(common.CONFIG))  # a stowed link: write where it points
    real.parent.mkdir(parents=True, exist_ok=True)
    # one writer at a time; the file is replaced whole (never truncated in place)
    with open(real.parent / f".{real.name}.lock", "a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        old = real.read_text() if real.exists() else NEW_FILE
        cfg = zonecfg.load_text(old, real) if old.strip() else zonecfg.Config({}, {}, [])
        if cfg.errors:
            die(f"{common.CONFIG} is invalid:\n  " + "\n  ".join(cfg.errors))
        if name in cfg.zones:
            die(f"zone {name!r} already exists")
        block = new_block(cfg, name)
        text = old + ("" if old.endswith("\n") else "\n") + block
        cfg2 = zonecfg.load_text(text, real)
        if cfg2.errors:
            die("the new block would make zones.toml invalid; nothing written:\n  " + "\n  ".join(cfg2.errors))
        fd, tmp = tempfile.mkstemp(dir=real.parent, prefix=f".{real.name}.")
        try:
            with os.fdopen(fd, "w") as f:
                f.write(text)
            if real.exists():
                os.chmod(tmp, stat.S_IMODE(os.stat(real).st_mode))
            os.replace(tmp, real)
        except BaseException:
            Path(tmp).unlink(missing_ok=True)
            raise
    print(block.strip())
    print(f"{'appended to' if old != NEW_FILE else 'created'} {real}", file=sys.stderr)
    return 0


def new_block(cfg: zonecfg.Config, name: str) -> str:
    used = {z.settings["colour"].lower() for z in cfg.zones.values() if isinstance(z.settings["colour"], str)}
    colour = next((c for c in common.PALETTE if c not in used), "#" + secrets.token_hex(3))
    return NEW_BLOCK.format(name=name, colour=colour)


def check(probe: str | None) -> int:
    cfg = zonecfg.load(common.CONFIG)
    for e in cfg.errors:
        print(f"error: {e}", file=sys.stderr)
    for w in cfg.warnings:
        print(f"warning: {w}", file=sys.stderr)
    if cfg.errors:
        return 1
    print(f"ok: {common.CONFIG}: {len(cfg.zones)} zones ({', '.join(cfg.zones)}), {len(cfg.apps)} apps "
          f"(this zone is release {zonecfg.RELEASE})")
    if not probe:
        return 0
    z = load_zone(probe)
    up(probe, quiet=True)
    tcp = "timeout 3 bash -c '</dev/tcp/{}/{}' 2>/dev/null"
    probes = [
        ("~/.ssh absent", 1, "ls -d ~/.ssh"),
        ("runtime dir: zone sockets only", 0,
         f"[ -z \"$(ls -A {common.ZRT} | grep -v -x -e bus -e zone-bus -e wayland-0 -e doc -e pipewire-0 -e pulse)\" ]"),
        ("no cgroupfs", 1, "ls /sys/fs/cgroup"),
        ("/nix/var absent", 1, "ls -d /nix/var"),
        ("capabilities empty", 0, "grep -q 'CapEff:.0000000000000000' /proc/self/status"),
        ("no_new_privs", 0, "grep -q 'NoNewPrivs:.1' /proc/self/status"),
        ("no host secrets in env", 1, "env | grep -E 'SSH_AUTH|HYPRLAND|TOKEN|SECRET|PASSWORD'"),
        *[(f"{p} absent", 1, f"ls -d {shlex.quote(str(p))}") for p in zonecfg.host_denied_paths()
          if p.exists() and not p.is_relative_to(common.HOME)],
        ("machine-id is the zone's", 0 if z.settings["machine_id"] == "zone" else 1,
         f"[ \"$(cat /etc/machine-id)\" != \"{Path('/etc/machine-id').read_text().strip() if Path('/etc/machine-id').exists() else ''}\" ]"),
    ]
    if z.settings["network"] == "internet":
        probes += [("nft flush refused", 1, "nft flush ruleset 2>/dev/null"),
                   ("LAN 192.168.0.0/16 refused", 1, tcp.format("192.168.1.1", 53)),
                   ("tailnet 100.64.0.0/10 refused", 1, tcp.format("100.64.0.1", 8443)),
                   ("host loopback refused", 1, tcp.format("127.0.0.1", 22))]
    fails = 0
    for title, expect, cmd in probes:
        r = subprocess.run([sys.executable, str(common.HERE / "zone.py"), "run", "--wait", probe, "--",
                            "/run/current-system/sw/bin/bash", "-c", cmd],
                           stdin=subprocess.DEVNULL, capture_output=True, text=True)
        ok = (r.returncode == 0) == (expect == 0)
        fails += not ok
        print(f"{'PASS' if ok else 'FAIL'}  {title}")
    return 1 if fails else 0


def find_desktop(name: str) -> dict:
    dirs = [os.environ.get("XDG_DATA_HOME", str(common.HOME / ".local/share"))]
    dirs += os.environ.get("XDG_DATA_DIRS", "/run/current-system/sw/share").split(":")
    for d in dirs:
        p = Path(d) / "applications" / name
        if p.is_file():
            out, section = {}, None
            for line in p.read_text(errors="replace").splitlines():
                if line.startswith("["):
                    section = line.strip()
                elif section == "[Desktop Entry]" and "=" in line:
                    k, v = line.split("=", 1)
                    out.setdefault(k.strip(), v.strip())
            return out
    return {}


def apps() -> int:
    """Launcher entries "App (zone)", one per app x zone. They go to
    ~/.local/share/appzones/launcher/share/applications (on XDG_DATA_DIRS through
    the NixOS module); host entries are never hidden."""
    cfg = load_cfg(quiet=True)
    common.LAUNCHER.mkdir(parents=True, exist_ok=True)
    for old in [*common.LAUNCHER.glob("appzones-*.desktop"), *common.LAUNCHER.glob("zone-*.desktop")]:  # zone-*: before 0.6
        old.unlink()
    n = 0
    for aid, a in cfg.apps.items():
        host = find_desktop(a["desktop"]) if a.get("desktop") else {}
        name = host.get("Name") or aid.capitalize()
        icon = host.get("Icon") or os.path.basename(a["command"][0]).lower()
        for zn in a.get("zones", []):
            z = cfg.zones[zn]
            lines = ["[Desktop Entry]", "Type=Application", f"Name={name} ({zn})",
                     f"GenericName={host.get('GenericName', name)} in zone {zn}",
                     f"Comment=Runs in app zone {zn} ({z.settings['network']}, colour {z.settings['colour']})",
                     f"Exec=appzones run {zn} {aid} %U", f"Icon={icon}", "Terminal=false",
                     f"Keywords=zone;{zn};{aid};", f"X-Zone={zn}"]
            for k in ("Categories", "MimeType", "StartupNotify"):
                if host.get(k):
                    lines.append(f"{k}={host[k]}")
            (common.LAUNCHER / f"appzones-{aid}-{zn}.desktop").write_text("\n".join(lines) + "\n")
            n += 1
    print(f"{n} launcher entries in {common.LAUNCHER}")
    return 0


def autostart() -> int:
    cfg = load_cfg(quiet=True)
    starting: list[tuple[str, int]] = []
    server.write_colours(cfg)
    apps()
    for n, z in cfg.zones.items():
        if z.settings["autostart"]:
            print(f"autostart: zone {n}")
            if use_systemd():
                systemctl("start", "--no-block", f"appzones@{n}.service", check=False)
            else:
                try:  # start them all first: a slow one (packages) holds up no other
                    r = claim(n, quiet=True, wait=False)
                    if r is not None:
                        starting.append((n, r))
                except SystemExit:
                    pass  # said why; the other zones still start
    for n, r in starting:
        try:
            wait_ready(n, r, quiet=True)
        except SystemExit:
            pass
    return 0


def active(follow: bool, as_json: bool) -> int:
    """Print "[zone]" (or an empty line) for Hyprland's active window, from
    its compositor tags only: never the app-chosen xdgTag, class or title
    --json: Waybar's custom-module JSON."""
    def current() -> str:
        try:
            r = subprocess.run(["hyprctl", "-j", "activewindow"], capture_output=True, text=True, timeout=5)
            tags = json.loads(r.stdout or "{}").get("tags", [])
        except (OSError, subprocess.TimeoutExpired, ValueError, AttributeError):
            tags = []  # no Hyprland (or no hyprctl): no label, never a crash in the bar
        z = next((t[5:].rstrip("*") for t in tags if t.startswith("zone:")), "")
        if as_json:
            return json.dumps({"text": f"[{z}]" if z else "", "class": f"zone-{z}" if z else "host",
                               "tooltip": f"active window runs in app zone {z}" if z else ""})
        return f"[{z}]" if z else ""
    print(current(), flush=True)
    if not follow:
        return 0
    sig = os.environ.get("HYPRLAND_INSTANCE_SIGNATURE")
    if not sig:
        die("no HYPRLAND_INSTANCE_SIGNATURE")
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(str(common.HOST_RT / "hypr" / sig / ".socket2.sock"))
    last, buf = None, b""
    while True:
        data = s.recv(4096)
        if not data:
            return 0
        buf += data
        *lines, buf = buf.split(b"\n")
        if any(l.startswith((b"activewindowv2>>", b"windowtitlev2>>", b"closewindow>>")) for l in lines):
            cur = current()
            if cur != last:
                print(cur, flush=True)
                last = cur


def logs(name: str, follow: bool) -> int:
    load_zone(name)
    if not use_systemd():
        os.execvp("tail", ["tail", "-n", "200", *(["-F"] if follow else []), str(common.log_path(name))])
    os.execvp("journalctl", ["journalctl", "--user", "-u", f"appzones@{name}.service", "-n", "200", "--no-pager",
                             *(["-f"] if follow else [])])
