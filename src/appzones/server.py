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
    """FD is open for writing, and a pipe, FIFO, file, socket or character device."""
    try:
        mode = os.fstat(fd).st_mode
        flags = fcntl.fcntl(fd, fcntl.F_GETFL)
    except (OSError, OverflowError):
        return False
    if flags & os.O_ACCMODE not in (os.O_WRONLY, os.O_RDWR):
        return False
    return stat.S_ISFIFO(mode) or stat.S_ISREG(mode) or stat.S_ISSOCK(mode) or stat.S_ISCHR(mode)


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


def supervise(name: str, notify_fd: int | None = None) -> int:
    """The zone NAME, in the foreground (`appzones serve`): builds it, holds it, and
    returns when it ends. SIGTERM or SIGINT stops it and its helpers."""
    if notify_fd is not None:
        if notify_fd < 3 or not notify_fd_ok(notify_fd):
            die(f"--notify-fd {notify_fd}: not a pipe, file or socket above 2, open for writing")
        os.set_inheritable(notify_fd, False)  # never into the zone or a helper
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
                             stdout=subprocess.PIPE, text=True)
        helpers.append(h)
        if h.stdout.readline().strip() != "ready":
            stop_all(); die("zone-wayland did not become ready")
    else:
        print(f"[zone] no WAYLAND_DISPLAY: zone {name} starts without a Wayland socket", flush=True)
    if os.environ.get("DBUS_SESSION_BUS_ADDRESS"):
        helpers.append(subprocess.Popen(portals.dbus_proxy_argv(z, zrt)))
        for _ in range(100):
            if (zrt / "bus").exists():
                break
            time.sleep(0.05)
        else:
            stop_all(); die("xdg-dbus-proxy did not create the zone's bus")
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
    # ready: zone-init answers; then the autostart apps
    for _ in range(200):
        if child.poll() is not None or agent.ping(name):
            break
        time.sleep(0.05)
    if child.poll() is None and not agent.ping(name):
        print(f"[zone] {name}: zone-init did not answer within 10 s; stopping", flush=True)
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
    i = argv.index("--")
    argv = argv[:i] + ["--setenv", "ZONE_CTL_FD", str(ctl)] + argv[i:]
    argv = pin_binds(argv)  # after fd 3 and the agent's: pinned descriptors come next
    os.execvp(argv[0], argv)
