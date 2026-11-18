"""The zone server (`appzones serve`), `_netsetup`, the agent socket, colours."""
from __future__ import annotations

import fcntl
import os
import secrets
import select
import shutil
import signal
import socket
import stat
import struct
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import zonecfg

from .common import die, load_cfg, settings_hash, tool
from . import agent, common, fs, net, portals


# ── supervisor ─────────────────────────────────────────────────────────────
def write_colours(cfg: zonecfg.Config) -> None:
    common.RT.mkdir(parents=True, exist_ok=True)
    # one temporary file per writer: zones start concurrently at login
    fd, tmp = tempfile.mkstemp(dir=common.RT, prefix=".colours.")
    try:
        with os.fdopen(fd, "w") as f:
            f.write("".join(f"{n} {z.settings['colour']}\n" for n, z in cfg.zones.items()))
        os.replace(tmp, common.RT / "colours")
    except BaseException:
        Path(tmp).unlink(missing_ok=True)
        raise


def sd_notify(msg: str) -> None:
    addr = os.environ.get("NOTIFY_SOCKET")
    if not addr:
        return
    if addr.startswith("@"):
        addr = "\0" + addr[1:]
    with socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM) as s:
        try:
            s.connect(addr)
            s.sendall(msg.encode())
        except OSError:
            pass


def notify_fd_ok(fd: int) -> bool:
    """FD is open for writing, and a pipe, FIFO, file or character device (s6 uses a pipe)."""
    try:
        mode = os.fstat(fd).st_mode
        flags = fcntl.fcntl(fd, fcntl.F_GETFL)
    except (OSError, OverflowError):
        return False
    if flags & os.O_ACCMODE not in (os.O_WRONLY, os.O_RDWR):
        return False
    return stat.S_ISFIFO(mode) or stat.S_ISREG(mode) or stat.S_ISCHR(mode)


def notify_ready(name: str, notify_fd: int | None) -> None:
    """The zone answers: tell systemd (sd_notify) and, with --notify-fd, a newline on that fd (s6)."""
    sd_notify("READY=1\nSTATUS=zone " + name + " running")
    if notify_fd is not None:
        try:
            os.write(notify_fd, b"\n")
        except OSError as e:
            print(f"[zone] {name}: could not write readiness to fd {notify_fd}: {e.strerror or e}", flush=True)
        finally:
            try:
                os.close(notify_fd)
            except OSError:
                pass


# the limit for each startup step (zone-wayland ready, the bus, zone-init answering);
# `packages` are built before any of them, without a limit
START_TIMEOUT = 30.0
ALREADY_SERVED = 75  # exit status of a second server for a zone (the unit does not restart on it)


def lock_path(name: str) -> Path:
    """The zone's lock: next to its runtime directory (which serve clears), never in the zone."""
    if not zonecfg.NAME_RE.fullmatch(name):
        die(f"not a zone name: {name!r}")
    return common.RT / f"{name}.lock"


def take_lock(name: str, lock_fd: int | None = None) -> int:
    """Hold zone NAME's lock for as long as this process lives (the kernel drops it when
    the process dies). LOCK_FD: the lock already taken by whoever started us, passed on
    without a gap; it must be that very file. A lock lives as long as any copy of its fd:
    the caller closes its own after starting us (or execs into us). Exits ALREADY_SERVED when another server
    holds it, or when an older server without a lock still answers."""
    path = lock_path(name)
    common.RT.mkdir(parents=True, exist_ok=True)
    os.chmod(common.RT, 0o700)
    if lock_fd is None:
        fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_CLOEXEC | os.O_NOFOLLOW, 0o600)
    else:
        fd = lock_fd
        try:
            st, pst = os.fstat(fd), os.stat(path, follow_symlinks=False)
            same = (st.st_dev, st.st_ino) == (pst.st_dev, pst.st_ino)
        except (OSError, OverflowError):
            same = False
        if not same:
            die(f"--lock-fd {lock_fd}: not zone {name}'s lock ({path})")
        os.set_inheritable(fd, False)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)  # held already through --lock-fd: succeeds
    except BlockingIOError:
        from . import client  # here: client imports this module
        pids = client.server_pids(name)
        die(f"zone {name} is already served" + (f" (pid {', '.join(map(str, pids))})" if pids else ""),
            ALREADY_SERVED)
    if agent.ping(name):
        os.close(fd)
        die(f"zone {name} already runs under a server without a lock (from before this version); "
            f"stop it first (appzones stop {name})", ALREADY_SERVED)
    return fd


def supervise(name: str, notify_fd: int | None = None, lock_fd: int | None = None) -> int:
    """The zone NAME, in the foreground (`appzones serve`): builds it, holds it, and
    returns when it ends. SIGTERM or SIGINT stops it and its helpers. One server per
    zone: take_lock comes before anything is changed."""
    if notify_fd is not None and notify_fd == lock_fd:
        die("--notify-fd and --lock-fd must be different fds (readiness closes the notify fd)")
    if notify_fd is not None:
        if notify_fd < 3 or not notify_fd_ok(notify_fd):
            die(f"--notify-fd {notify_fd}: not a pipe or file above 2, open for writing")
        os.set_inheritable(notify_fd, False)  # never into the zone or a helper
    lock = take_lock(name, lock_fd)  # noqa: F841  (held until this process exits)
    cfg = load_cfg(quiet=False)
    if name not in cfg.zones:
        die(f"no zone {name!r} in {common.CONFIG}")
    z = cfg.zones[name]
    s = z.settings
    zdir, zrt = common.STATE / name, common.RT / name
    zdir.mkdir(parents=True, exist_ok=True)
    common.RT.mkdir(parents=True, exist_ok=True)
    os.chmod(common.RT, 0o700)
    if zrt.exists():
        shutil.rmtree(zrt)
    (zrt / "ctl").mkdir(parents=True, mode=0o700)
    if not (zdir / "machine-id").exists():
        (zdir / "machine-id").write_text(secrets.token_hex(16) + "\n")
    (zrt / "resolv.conf").write_text(net.resolv_conf(s))
    (zrt / "hosts").write_text(f"127.0.0.1 localhost\n::1 localhost\n127.0.0.2 {name}-zone\n")
    instance = f"zone-{name}-{secrets.randbelow(2**31)}"
    (zrt / "flatpak-info").write_text(portals.flatpak_info(z, instance))
    (zrt / "config.sha256").write_text(settings_hash(z) + "\n")
    write_colours(cfg)
    if s["tmp"] == "disk":
        fs.prepare_tmp(zdir)
    fs.prepare_mnt(name, zdir)
    extra_path = fs.package_paths(z, zdir) if s["packages"] else []
    target = portals.link_target(z)
    links_srv = None
    if target:
        (zrt / "ctl/bin").mkdir()
        xo = zrt / "ctl/bin/xdg-open"
        xo.write_text(portals.XDG_OPEN.format(links=s["links"], target=target, python=os.path.realpath(sys.executable)))
        xo.chmod(0o755)
        links_srv = portals.serve_links(z, zrt, target)
    helpers: list[subprocess.Popen] = []
    child: subprocess.Popen | None = None

    def stop_all(*_):
        if child and child.poll() is None:
            child.terminate()
        for h in helpers:
            if h.poll() is None:
                h.terminate()

    signal.signal(signal.SIGTERM, stop_all)
    signal.signal(signal.SIGINT, stop_all)
    if os.environ.get("WAYLAND_DISPLAY"):
        h = subprocess.Popen([tool("zone-wayland"), str(zrt / "wayland-0"), f"dev.mirql.zone.{name}", instance],
                             stdout=subprocess.PIPE, text=True,
                             env=dict(os.environ, ZONE_WAYLAND_PARENT=str(os.getpid())))
        helpers.append(h)
        ready, _, _ = select.select([h.stdout], [], [], START_TIMEOUT)
        if not ready or h.stdout.readline().strip() != "ready":
            stop_all(); die(f"zone-wayland did not become ready within {START_TIMEOUT:.0f} s")
    else:
        print(f"[zone] no WAYLAND_DISPLAY: zone {name} starts without a Wayland socket", flush=True)
    if os.environ.get("DBUS_SESSION_BUS_ADDRESS"):
        helpers.append(subprocess.Popen(portals.dbus_proxy_argv(z, zrt)))  # its bwrap: --die-with-parent
        end = time.monotonic() + START_TIMEOUT
        while not (zrt / "bus").exists():
            if time.monotonic() > end or helpers[-1].poll() is not None:
                stop_all(); die("xdg-dbus-proxy did not create the zone's bus")
            time.sleep(0.05)
    network = s["network"]
    (zrt / "nft.rules").write_text(net.nft_rules(z))
    # bwrap writes its info (child-pid) to fd 3; the portal reads it as
    # $XDG_RUNTIME_DIR/.flatpak/<instance>/bwrapinfo.json
    fp = common.HOST_RT / ".flatpak" / instance
    fp.mkdir(parents=True, exist_ok=True)
    inner = fs.bwrap_argv(z, zdir, zrt, network, extra_path)
    # pasta closes inherited fds, so the info fd is opened by _netsetup, in
    # pasta's namespace, right before it execs bwrap.
    argv = [sys.executable, str(common.HERE / "zone.py"), "_netsetup", str(zrt / "nft.rules") if network == "internet" else "-",
            str(fp / "bwrapinfo.json"), name, "--"] + inner
    if network == "internet":
        argv = net.pasta_argv(z) + argv
    child = subprocess.Popen(argv)
    if not offer_pidfd(name, child):
        print(f"[zone] {name}: _netsetup did not take the server's pidfd; stopping", flush=True)
        stop_all()
        child.wait()
        shutil.rmtree(fp, ignore_errors=True)
        return 1
    # ready: zone-init answers; then the autostart apps
    end = time.monotonic() + START_TIMEOUT
    while time.monotonic() < end:
        if child.poll() is not None or agent.ping(name):
            break
        time.sleep(0.05)
    if child.poll() is None and not agent.ping(name):
        print(f"[zone] {name}: zone-init did not answer within {START_TIMEOUT:.0f} s; stopping", flush=True)
        stop_all()
        child.wait()
        shutil.rmtree(fp, ignore_errors=True)
        return 1
    if child.poll() is None:
        notify_ready(name, notify_fd)
        print(f"[zone] {name} ready (network={network}, dns={s['dns']}, devices={','.join(s['devices']) or '-'})",
              flush=True)
        for app in s["autostart_apps"]:
            try:
                r = agent.send_run(name, zonecfg.resolve_app(cfg, name, app) or [app], bus=zonecfg.app_bus(cfg, name, app))
                print(f"[zone] autostart {app}: {r}", flush=True)
            except (OSError, ValueError) as e:
                print(f"[zone] autostart {app} failed: {e}", flush=True)
    # wait for the zone; a dead helper takes the zone down (a zone without
    # its proxy or Wayland socket is worse than a restart)
    rc = None
    while rc is None:
        rc = child.poll()
        if rc is not None:
            break
        if any(h.poll() is not None for h in helpers):
            print("[zone] a helper (zone-wayland or xdg-dbus-proxy) exited; stopping the zone", flush=True)
            child.terminate()
            try:
                rc = child.wait(10)
            except subprocess.TimeoutExpired:
                child.kill(); rc = child.wait()
            rc = rc or 1
            break
        fds = [links_srv] if links_srv else []
        r, _, _ = select.select(fds, [], [], 0.5)
        if r and links_srv:
            portals.handle_link(links_srv, target)
    stop_all()
    shutil.rmtree(fp, ignore_errors=True)
    fs.prepare_mnt(name, zdir)  # runtime mounts end with the zone
    return rc


# A bind in bwrap_argv: PIN MODE RAW DEST, replaced by netsetup.
PIN = "--zones-pin"


def pin_binds(argv: list[str]) -> list[str]:
    """Replace every PIN marker by --bind-fd/--ro-bind-fd on a descriptor
    opened here and checked here: the policy runs on the path of the object
    actually opened, and bwrap mounts that same object."""
    out, i = [], 0
    while i < len(argv):
        if argv[i] != PIN:
            out.append(argv[i]); i += 1
            continue
        mode, raw, dest = argv[i + 1:i + 4]
        i += 4
        try:
            fd = os.open(zonecfg.expand(raw), os.O_PATH | os.O_CLOEXEC)
        except OSError as e:
            die(f"bind {raw}: {e.strerror or e}; refusing to start the zone")
        real = zonecfg.fd_path(fd)
        if real is None:
            die(f"bind {raw}: was removed or replaced while the zone started; refusing to start the zone")
        err = zonecfg.policy_error(real, raw) or zonecfg.kind_error(os.fstat(fd).st_mode, raw)
        if err:
            die(f"bind {err}; refusing to start the zone")
        os.set_inheritable(fd, True)
        out += ["--bind-fd" if mode == "rw" else "--ro-bind-fd", str(fd), dest]
    return out


def agent_listener(name: str) -> int:
    """zone-init's control socket, made here on the host side: a path outside
    everything bound into the zone, so no process in the zone can replace it,
    and handed to zone-init as an inherited fd ($ZONE_CTL_FD)."""
    path = agent.agent_path(name)
    try:
        os.unlink(path)
    except FileNotFoundError:
        pass
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    old = os.umask(0o177)
    try:
        s.bind(str(path))
    finally:
        os.umask(old)
    s.listen(16)
    fd = s.detach()
    os.set_inheritable(fd, True)
    return fd


def handoff_path(name: str) -> Path:
    """Where the server hands its own pidfd to _netsetup (in the zone's runtime dir, 0700)."""
    return common.RT / name / "server.sock"


def offer_pidfd(name: str, child: subprocess.Popen) -> bool:
    """Give _netsetup a pidfd of this server, over a unix socket: a pid cannot be used
    there (pasta's namespaces) and an inherited fd cannot get there (pasta closes them),
    but a passed fd can. zone-init watches it and ends the zone when this process ends.
    False when _netsetup did not come for it in time or the child exited first."""
    path = handoff_path(name)
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as ls:
        ls.bind(str(path))
        ls.listen(1)
        ls.settimeout(0.2)
        try:
            end = time.monotonic() + START_TIMEOUT
            while time.monotonic() < end and child.poll() is None:
                try:
                    conn, _ = ls.accept()
                except TimeoutError:
                    continue
                with conn:
                    me = os.pidfd_open(os.getpid())
                    try:
                        socket.send_fds(conn, [b"pidfd"], [me])
                    finally:
                        os.close(me)
                return True
            return False
        finally:
            path.unlink(missing_ok=True)


def receive_pidfd(name: str) -> int:
    """_netsetup's side of offer_pidfd: the server's pidfd, for zone-init ($ZONE_SERVER_FD)."""
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
        s.settimeout(START_TIMEOUT)
        end = time.monotonic() + START_TIMEOUT
        while True:
            try:
                s.connect(str(handoff_path(name)))
                break
            except (FileNotFoundError, ConnectionRefusedError):
                if time.monotonic() > end:
                    die(f"zone {name}: no pidfd from its server; not starting the zone")
                time.sleep(0.05)
        uid = struct.unpack("3i", s.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, struct.calcsize("3i")))[1]
        msg, fds, _, _ = socket.recv_fds(s, 16, 1)
    if uid != os.getuid() or msg != b"pidfd" or len(fds) != 1:
        for f in fds:
            os.close(f)
        die(f"zone {name}: no valid pidfd from its server; not starting the zone")
    os.set_inheritable(fds[0], True)  # for zone-init, through bwrap
    return fds[0]


def netsetup(rules: str, info: str, name: str, argv: list[str]) -> None:
    if rules != "-":
        r = subprocess.run([tool("nft"), "-f", rules])
        if r.returncode != 0:
            die("nft rules failed to load; refusing to start the zone without them")
    fd = os.open(info, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    if fd != 3:
        os.dup2(fd, 3)
        os.close(fd)
    os.set_inheritable(3, True)
    ctl = agent_listener(name)
    srv = receive_pidfd(name)
    i = argv.index("--")
    argv = argv[:i] + ["--setenv", "ZONE_CTL_FD", str(ctl), "--setenv", "ZONE_SERVER_FD", str(srv)] + argv[i:]
    argv = pin_binds(argv)  # after fd 3 and the agent's: pinned descriptors come next
    os.execvp(argv[0], argv)
