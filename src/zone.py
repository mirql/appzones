"""appzones: app zones on this host (appzones, https://github.com/mirql/appzones).

    appzones list                          zones: state, network, dns, devices, apps running, colour
    appzones which ZONE                    every setting in effect and where it came from
    appzones run [--restart] [--bus B] ZONE APP [ARGS...]
    appzones run [--restart] [--bus B] [--wait] ZONE -- CMD [ARGS...]
                                           start the zone if needed, launch inside it.
                                           --restart: if zones.toml changed for ZONE and no
                                           app runs in it, restart it first.
                                           --bus host|zone|none: the session bus the app gets
                                           (default: the app's `bus` in [apps], also for
                                           `-- CMD` when CMD is a declared app's command;
                                           else the zone's `bus`).
                                           --wait: CMD gets this stdin/stdout/stderr (pipes
                                           or files, never a tty) and its exit code is ours.
                                           Without --wait, `run` watches the app for a few
                                           seconds ($APPZONES_RUN_CHECK, 0: not at all) and, if
                                           it exits with an error, prints its last output.
    appzones shell ZONE                    an interactive shell inside (pty in the zone)
    appzones ps ZONE | logs ZONE [-f]      processes in the zone | its journal (-f: follow)
    appzones up|stop|restart ZONE|--all
    appzones new NAME [-y]                 print a safe zone block; append it to zones.toml with -y
    appzones check [--probe ZONE]          validate zones.toml; --probe also runs isolation probes in ZONE
    appzones apps                          (re)write launcher entries "App (zone)" from [apps]
    appzones active [--follow] [--json]    "[zone]" for Hyprland's active window (for bars;
                                           --json: Waybar custom-module JSON)
    appzones mount ZONE HOST-DIR [--ro] [--as NAME]
                                           HOST-DIR appears at /mnt/host/NAME in the running
                                           zone (NAME: the folder's name), until umount or
                                           the zone stops; same never-list as binds
    appzones umount ZONE NAME [--lazy]     remove it; busy: lists what holds it (--lazy:
                                           detach now, open files keep it until they close)
    appzones mounts ZONE                   what is mounted in the zone

Internal (systemd and the supervisor):
    appzones _supervise ZONE               the zone itself (appzones@ZONE.service's ExecStart)
    appzones _netsetup RULES INFO -- CMD   load nft RULES in this netns, open INFO as fd 3, exec CMD
    appzones _autostart                    write launcher entries, start zones with autostart = true

Config: ~/.config/appzones/zones.toml ($APPZONES_CONFIG). State: ~/.local/share/appzones.
Before 0.6: ~/.config/zones and ~/.local/share/zones (read, and moved; see migrate_state).
Docs: https://mirql.github.io/appzones/ (docs/ in the repo).
"""
from __future__ import annotations

import fcntl
import fnmatch
import glob
import hashlib
import ipaddress
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

sys.path.insert(0, str(Path(__file__).resolve().parent))
import zonecfg  # noqa: E402

HERE = Path(__file__).resolve().parent
UID, GID = os.getuid(), os.getgid()
HOME = Path.home()
USER = os.environ.get("USER") or os.environ.get("LOGNAME") or "user"
HOST_RT = Path(os.environ.get("XDG_RUNTIME_DIR", f"/run/user/{UID}"))
RT = HOST_RT / "appzones"
LEGACY_STATE = HOME / ".local/share/zones"  # before 0.6; moved by migrate_state


def default_state() -> Path:
    """~/.local/share/appzones, or the pre-0.6 directory while it has not been
    moved yet. Moves nothing (main() does, through migrate_state)."""
    new = HOME / ".local/share/appzones"
    if not os.path.lexists(new) and os.path.lexists(LEGACY_STATE):
        return LEGACY_STATE
    return new


STATE = Path(zonecfg.env("STATE") or default_state())
CONFIG, LEGACY_CONFIG = zonecfg.config_path(HOME)
LAUNCHER = STATE / "launcher/share/applications"
DNS_ADDR = "169.254.53.53"
ZRT = f"/run/user/{UID}"  # the zone's own XDG_RUNTIME_DIR (a fresh tmpfs)
MOUNT_HELPER = zonecfg.env("MOUNT_HELPER") or str(HERE / "appzones-mount-helper")
SUDO = zonecfg.env("SUDO") or "/run/wrappers/bin/sudo"
# netbird = true: the mesh is the network of this interface (NetBird's
# default name is wt0; the NixOS module's netbirdInterface sets it).
NETBIRD_IFACE = zonecfg.env("NETBIRD_IFACE") or "wt0"
TMP_DAYS = {"tmp": 14, "var-tmp": 30}  # tmp = "disk": unused this long, removed at zone start
PALETTE = ["#a6e3a1", "#89b4fa", "#f38ba8", "#f9e2af", "#cba6f7", "#94e2d5", "#fab387", "#f5c2e7",
           "#74c7ec", "#eba0ac", "#b4befe", "#89dceb"]


def tool(name: str) -> str:
    p = zonecfg.env("BIN_" + name.upper().replace("-", "_")) or shutil.which(name)
    if not p:
        die(f"{name} not found on PATH")
    return p


def die(msg: str, code: int = 1):
    print(f"appzones: {msg}", file=sys.stderr)
    sys.exit(code)


def warn(msg: str) -> None:
    print(f"appzones: {msg}", file=sys.stderr)


def load_cfg(quiet: bool = False) -> zonecfg.Config:
    cfg = zonecfg.load(CONFIG)
    if cfg.errors:
        die(f"{CONFIG} is invalid:\n  " + "\n  ".join(cfg.errors))
    if not quiet:
        for w in cfg.warnings:
            warn(f"warning: {w}")
    return cfg


def load_zone(name: str, quiet: bool = True) -> zonecfg.Zone:
    cfg = load_cfg(quiet)
    if name not in cfg.zones:
        die(f"no zone {name!r} in {CONFIG} (zones: {', '.join(cfg.zones) or 'none'})")
    return cfg.zones[name]


def settings_hash(z: zonecfg.Zone) -> str:
    return hashlib.sha256(json.dumps(z.settings, sort_keys=True).encode()).hexdigest()


def use_systemd() -> bool:
    return (zonecfg.env("SUPERVISOR") or "systemd") == "systemd"


def systemctl(*args: str, check: bool = True) -> subprocess.CompletedProcess:
    r = subprocess.run(["systemctl", "--user", *args], capture_output=True, text=True)
    if check and r.returncode != 0:
        die(f"systemctl --user {' '.join(args)}: {r.stderr.strip() or r.returncode}")
    return r


# ── network ────────────────────────────────────────────────────────────────
def host_addrs() -> list[tuple[str, ipaddress.IPv4Interface | ipaddress.IPv6Interface]]:
    """(interface, address/prefix) for every address of the host, from `ip -j addr`."""
    r = subprocess.run(["ip", "-j", "addr", "show"], capture_output=True, text=True)
    if r.returncode != 0:
        raise OSError(f"ip -j addr show: {r.stderr.strip() or r.returncode}")
    out = []
    for link in json.loads(r.stdout or "[]"):
        for a in link.get("addr_info", []):
            try:
                out.append((link.get("ifname", ""), ipaddress.ip_interface(f"{a['local']}/{a['prefixlen']}")))
            except (KeyError, ValueError):
                continue
    return out


def _nft_set(items) -> str:
    return ", ".join(str(i) for i in items)


def nft_rules(z: zonecfg.Zone) -> str:
    """The deny list loaded inside the zone's own netns: pasta turns zone
    traffic into host sockets, so without it the zone reaches the LAN, the
    tailnet and the host's own non-loopback listeners. Computed at zone
    start from the host's addresses: every address of the host is refused
    (never the host itself, also with netbird), then the NetBird networks
    are allowed (netbird = true), then every network the host is directly
    connected to (a LAN with global IPv6 or public IPv4 addresses too) and
    the private, CGNAT, link-local and multicast ranges are refused."""
    addrs = [(i, a) for i, a in host_addrs() if not a.ip.is_loopback]
    mine4 = sorted({a.ip for _, a in addrs if a.version == 4})
    mine6 = sorted({a.ip for _, a in addrs if a.version == 6})
    nets = [a.network for _, a in addrs if not a.ip.is_link_local]
    nb4 = nb6 = []
    if z.settings["netbird"]:
        nbn = [a.network for i, a in addrs if i == NETBIRD_IFACE]
        nb4 = list(ipaddress.collapse_addresses(n for n in nbn if n.version == 4))
        nb6 = list(ipaddress.collapse_addresses(n for n in nbn if n.version == 6))
    lan4 = list(ipaddress.collapse_addresses(n for n in nets if n.version == 4 and n not in nb4))
    lan6 = list(ipaddress.collapse_addresses(n for n in nets if n.version == 6 and n not in nb6))
    rules = ""
    if mine4:
        rules += f"    ip daddr {{ {_nft_set(mine4)} }} reject\n"
    if mine6:
        rules += f"    ip6 daddr {{ {_nft_set(mine6)} }} reject\n"
    if nb4:
        rules += f"    ip daddr {{ {_nft_set(nb4)} }} accept\n"
    if nb6:
        rules += f"    ip6 daddr {{ {_nft_set(nb6)} }} accept\n"
    if lan4:
        rules += f"    ip daddr {{ {_nft_set(lan4)} }} reject\n"
    if lan6:
        rules += f"    ip6 daddr {{ {_nft_set(lan6)} }} reject\n"
    return f"""table inet zone {{
  chain out {{
    type filter hook output priority 0; policy accept;
    oif "lo" accept
    ip daddr {DNS_ADDR} udp dport 53 accept
    ip daddr {DNS_ADDR} tcp dport 53 accept
{rules}    ip daddr {{ 0.0.0.0/8, 10.0.0.0/8, 100.64.0.0/10, 127.0.0.0/8, 169.254.0.0/16, 172.16.0.0/12,
               192.0.0.0/24, 192.168.0.0/16, 198.18.0.0/15, 224.0.0.0/4, 240.0.0.0/4 }} reject
    ip6 daddr {{ ::1, ::ffff:0:0/96, fc00::/7, fe80::/10, ff00::/8 }} reject
  }}
}}
"""


def pasta_argv(z: zonecfg.Zone) -> list[str]:
    a = [tool("pasta"), "--config-net", "--no-map-gw", "-t", "none", "-u", "none", "-T", "none", "-U", "none",
         "--dns-forward", DNS_ADDR, "-q", "-f"]
    # pasta skips loopback nameservers from resolv.conf ("Couldn't get any
    # nameserver address"), so name the resolver explicitly.
    a += ["--dns-host", "1.1.1.1" if z.settings["dns"] == "public" else host_resolver()]
    return a + ["--"]


def resolv_conf(s: dict) -> str:
    """The zone's /etc/resolv.conf. internet: pasta's forwarder (which goes to
    host_resolver() or 1.1.1.1); host: the same choice, direct; none: empty."""
    if s["network"] == "internet":
        return f"nameserver {DNS_ADDR}\n"
    if s["network"] == "host":
        if s["dns"] == "public":
            return "nameserver 1.1.1.1\n"
        if zonecfg.env("DNS_HOST"):
            return f"nameserver {zonecfg.env('DNS_HOST')}\n"
        try:
            return Path("/etc/resolv.conf").read_text()
        except OSError:
            return ""
    return ""


def host_resolver(resolv: str = "/etc/resolv.conf") -> str:
    """The resolver for dns = "host": $APPZONES_DNS_HOST (the module's dnsServer),
    else the first nameserver in the host's resolv.conf (systemd-resolved's
    127.0.0.53, dnsmasq's 127.0.0.1, a LAN resolver, ...)."""
    if zonecfg.env("DNS_HOST"):
        return zonecfg.env("DNS_HOST")
    try:
        for line in Path(resolv).read_text().splitlines():
            parts = line.split()
            if len(parts) >= 2 and parts[0] == "nameserver":
                ipaddress.ip_address(parts[1].split("%")[0])
                return parts[1]
    except (OSError, ValueError):
        pass
    return "127.0.0.53"


# ── filesystem ─────────────────────────────────────────────────────────────
ETC = ["static", "fonts", "ssl", "pki", "passwd", "group", "nsswitch.conf", "localtime", "zoneinfo",
       "mime.types", "xdg", "profile", "hosts", "host.conf", "login.defs", "os-release", "pulse",
       "alsa", "asound.conf", "gtk-3.0", "gtk-2.0", "machine-info", "locale.conf", "vconsole.conf",
       "pipewire", "egl", "vulkan", "inputrc", "bashrc", "set-environment", "terminfo"]

# Copied (dereferenced) into the zone at each start and bound read-only at
# the same place in the zone home: on a host with stowed dotfiles these are
# links into a checkout, which would dangle inside (and checkouts never enter
# a zone).
THEME = [".config/gtk-3.0/settings.ini", ".config/gtk-3.0/gtk.css", ".config/gtk-4.0/settings.ini",
         ".config/gtk-4.0/gtk.css", ".config/qt5ct", ".config/qt6ct", ".config/kdeglobals", ".config/Kvantum",
         ".config/fontconfig", ".gtkrc-2.0", ".icons", ".local/share/fonts", ".local/share/icons/default"]
THEME_MAX = 200 * 1024 * 1024
THEME_MAX_FILES = 50000
THEME_SKIP = ("bookmarks", "*.lock")


class ThemeBudget(Exception):
    pass


def _theme_target_ok(fd: int) -> bool:
    """Whether the file this descriptor refers to may be copied into a zone: a
    regular file whose real path is not on the never-list. Checked on the
    open descriptor, so a link swapped after the check changes nothing."""
    if not stat.S_ISREG(os.fstat(fd).st_mode):
        return False
    real = zonecfg.fd_path(fd)  # None: unlinked since it was opened
    return real is not None and zonecfg.theme_source_error(real) is None


def _copy_theme_file(src: Path, dst: Path, left: list[int], count: bool = True) -> None:
    try:
        fd = os.open(src, os.O_RDONLY | os.O_CLOEXEC | os.O_NONBLOCK)
    except OSError:
        return  # dangling link, no permission: skip quietly
    try:
        if not _theme_target_ok(fd):
            warn(f"theme: not copying {src} (it resolves to a path that never enters a zone)")
            return
        if count:  # a file inside a directory was counted as an entry already
            left[1] -= 1
            if left[1] < 0:
                raise ThemeBudget()
        dst.parent.mkdir(parents=True, exist_ok=True)
        # count the bytes actually read (a file may grow after fstat)
        with os.fdopen(os.dup(fd), "rb") as fin, open(dst, "wb") as fout:
            while chunk := fin.read(1 << 20):
                left[0] -= len(chunk)
                if left[0] < 0:
                    raise ThemeBudget()
                fout.write(chunk)
    finally:
        os.close(fd)


def _copy_theme_dir(src: Path, dst: Path, left: list[int], seen: set[str], depth: int = 0) -> None:
    real = os.path.realpath(src)
    if depth > 12 or real in seen or zonecfg.theme_source_error(Path(real)) is not None:
        return
    seen.add(real)
    try:
        it = os.scandir(src)
    except OSError:
        return
    with it:
        for e in it:
            _copy_theme_entry(e, dst, left, seen, depth)


def _copy_theme_entry(e: os.DirEntry, dst: Path, left: list[int], seen: set[str], depth: int) -> None:
    # every entry visited counts, copied or not (a tree of empty or skipped
    # entries must not make the supervisor walk forever)
    left[1] -= 1
    if left[1] < 0:
        raise ThemeBudget()
    if any(fnmatch.fnmatch(e.name, pat) for pat in THEME_SKIP):
        return
    try:
        is_dir = e.is_dir(follow_symlinks=True)
    except OSError:
        return
    if is_dir:
        _copy_theme_dir(Path(e.path), dst / e.name, left, seen, depth + 1)
    else:
        _copy_theme_file(Path(e.path), dst / e.name, left, count=False)


def copy_theme(zdir: Path) -> list[tuple[Path, str]]:
    """Copy the theme files in; returns (copy, path inside) pairs to bind.
    Symlinks are followed (stowed dotfiles, store paths), but every file is
    checked where it really is, on the open descriptor: a link to a never-list
    path (planted in a theme directory a zone could write to) is not copied."""
    dst_root = zdir / "theme"
    shutil.rmtree(dst_root, ignore_errors=True)
    out, left = [], [THEME_MAX, THEME_MAX_FILES]
    for rel in THEME:
        src = HOME / rel
        if not src.exists():
            continue
        dst = dst_root / rel
        before = list(left)
        try:
            if src.is_dir():
                _copy_theme_dir(src, dst, left, set())
            else:
                _copy_theme_file(src, dst, left)
        except ThemeBudget:
            warn(f"theme: skipping ~/{rel} (over the budget of {THEME_MAX // 2**20} MiB / {THEME_MAX_FILES} files)")
            shutil.rmtree(dst, ignore_errors=True) if dst.is_dir() else dst.unlink(missing_ok=True)
            left[:] = before
            continue
        except OSError as e:
            warn(f"theme: ~/{rel}: {e}")
            continue
        if dst.exists():
            out.append((dst, str(HOME / rel)))
    return out


def gpu_binds() -> list[str]:
    """Render nodes only (never the KMS card node), plus the sysfs that libdrm
    and Mesa read: /sys/dev/char (links) and each render node's own device
    directory, not all of /sys/devices."""
    a = []
    for node in sorted(glob.glob("/dev/dri/renderD*")):
        a += ["--dev-bind", node, node]
        dev = Path("/sys/class/drm") / Path(node).name / "device"
        if dev.exists():
            real = os.path.realpath(dev)
            a += ["--ro-bind", real, real]
    if a:
        a += ["--ro-bind-try", "/sys/dev/char", "/sys/dev/char"]
    return a


def _read(p: Path) -> str:
    try:
        return p.read_text().strip()
    except OSError:
        return ""


def usb_binds(ids: list[str], root: str = "/") -> tuple[list[str], list[str]]:
    """(bwrap args, what was found) for the USB devices whose idVendor (and
    idProduct, for "vvvv:pppp") match IDS, read at zone start: each one's
    /dev/bus/usb/BBB/DDD node, its own sysfs directory (realpath, read-only),
    every /dev/hidrawN below that directory, and udev's records of those nodes
    (/run/udev/data/cMAJ:MIN, which libusb and hidapi read through libudev).
    Plus /sys/bus/usb (libusb enumerates there), and /sys/class/hidraw with a
    hidraw match: listings only, their links dangle for any device not bound.
    Never all of /dev/bus/usb or /dev/hidraw*, never /dev/uinput. A replug
    changes the device number: `appzones restart` after it. ROOT: tests only."""
    def r(p: str) -> Path:
        return Path(root) / p.lstrip("/")

    def host(p: str) -> str:  # a path under ROOT, as the host (and the zone) names it
        return "/" + os.path.relpath(p, os.path.realpath(root)) if root != "/" else p

    want = [tuple(i.split(":")) if ":" in i else (i, None) for i in ids]
    a: list[str] = []
    found: list[str] = []
    try:
        entries = sorted(os.listdir(r("/sys/bus/usb/devices")))
    except OSError:
        return a, found
    hidraws = sorted(glob.glob(str(r("/sys/class/hidraw/hidraw*"))))
    any_hid = False
    for e in entries:
        d = r("/sys/bus/usb/devices") / e
        vid, pid = _read(d / "idVendor").lower(), _read(d / "idProduct").lower()
        if not vid or not any(vid == v and (p is None or pid == p) for v, p in want):
            continue
        real = os.path.realpath(d)
        bus, dev, majmin = _read(d / "busnum"), _read(d / "devnum"), _read(d / "dev")
        if not (bus.isdigit() and dev.isdigit()):
            continue
        node = f"/dev/bus/usb/{int(bus):03d}/{int(dev):03d}"
        try:
            st = os.stat(r(node))
        except OSError:
            warn(f"usb: {vid}:{pid} ({e}) has no {node}; skipped")
            continue
        # the node must be the device sysfs names (not whatever took its number)
        if majmin and f"{os.major(st.st_rdev)}:{os.minor(st.st_rdev)}" != majmin:
            warn(f"usb: {node} is not {vid}:{pid} ({e}); skipped")
            continue
        a += ["--dev-bind", node, node, "--ro-bind", host(real), host(real)]
        a += ["--ro-bind-try", f"/run/udev/data/c{majmin}", f"/run/udev/data/c{majmin}"] if majmin else []
        what = [node]
        for h in hidraws:
            hdev = os.path.realpath(Path(h) / "device")
            if not hdev.startswith(real + "/"):
                continue
            hn = f"/dev/{Path(h).name}"
            hmm = _read(Path(h) / "dev")
            if not os.path.exists(r(hn)):
                continue
            a += ["--dev-bind", hn, hn]
            a += ["--ro-bind-try", f"/run/udev/data/c{hmm}", f"/run/udev/data/c{hmm}"] if hmm else []
            what.append(hn)
            any_hid = True
        found.append(f"{vid}:{pid} {_read(d / 'product') or e} ({', '.join(what)})")
    if a:
        a += ["--ro-bind-try", "/sys/bus/usb", "/sys/bus/usb"]
        if any_hid:
            a += ["--ro-bind-try", "/sys/class/hidraw", "/sys/class/hidraw"]
    return a, found


def package_paths(z: zonecfg.Zone, zdir: Path) -> list[str]:
    """Build the zone's extra `packages` (nixpkgs attributes) with GC roots in
    the zone's state; returns their bin dirs. A failure warns and continues."""
    out = []
    roots = zdir / "packages"
    roots.mkdir(exist_ok=True)
    for attr in z.settings["packages"]:
        link = roots / attr
        expr = (f'(import (builtins.getFlake "nixpkgs").outPath {{ system = builtins.currentSystem; '
                f'config.allowUnfree = true; }}).{attr}')
        r = subprocess.run(["nix", "build", "--impure", "--out-link", str(link), "--expr", expr],
                           capture_output=True, text=True)
        if r.returncode != 0:
            warn(f"packages: {attr} did not build: {r.stderr.strip().splitlines()[-1:] or r.returncode}")
            continue
        out.append(os.path.realpath(link) + "/bin")
    return out


def clean_old(root: Path, days: int) -> int:
    """Remove the files in ROOT neither read nor written (atime, mtime) for
    DAYS, then directories not changed for DAYS once empty. Not ctime: a
    rename or chmod is not use. On a noatime file system (a common /home) that
    means not written. The zone wrote everything in it: never follow a
    symlink (unlink the link itself). Symlinks and directories count by
    mtime only: reading them here (this walk) would itself move their atime.
    Returns the count."""
    cutoff = time.time() - days * 86400

    def walk(d: str) -> int:
        n = 0
        try:
            entries = list(os.scandir(d))
        except OSError:
            return 0
        for e in entries:
            try:
                st = e.stat(follow_symlinks=False)
                if e.is_dir(follow_symlinks=False):
                    n += walk(e.path)
                    if st.st_mtime < cutoff:
                        os.rmdir(e.path)  # fails, and stays, unless empty
                elif (st.st_mtime if e.is_symlink() else max(st.st_atime, st.st_mtime)) < cutoff:
                    os.unlink(e.path)
                    n += 1
            except OSError:
                pass
        return n

    return walk(str(root))


def prepare_tmp(zdir: Path) -> None:
    """tmp = "disk": ~/.local/share/appzones/<zone>/{tmp,var-tmp}, 0700, cleaned (TMP_DAYS)."""
    for sub, days in TMP_DAYS.items():
        d = zdir / sub
        d.mkdir(mode=0o700, exist_ok=True)
        os.chmod(d, 0o700)
        n = clean_old(d, days)
        if n:
            print(f"[zone] tmp: removed {n} file(s) unused for {days} days from {d}", flush=True)


def host_mounts(zone: str) -> list[str]:
    """Names mounted under ~/.local/share/appzones/ZONE/mnt (host view)."""
    pre = str(STATE / zone / "mnt") + "/"
    out = []
    try:
        with open("/proc/self/mountinfo") as f:
            for line in f:
                mp = line.split()[4].replace("\\040", " ")
                if mp.startswith(pre) and "/" not in mp[len(pre):] and mp[len(pre):] not in out:
                    out.append(mp[len(pre):])
    except OSError:
        pass
    return out


def helper(*args: str) -> subprocess.CompletedProcess:
    return subprocess.run([SUDO, "-n", MOUNT_HELPER, *args], capture_output=True, text=True, stdin=subprocess.DEVNULL)


def prepare_mnt(zone: str, zdir: Path) -> None:
    """At zone start and stop: no runtime mount outlives a zone run."""
    mnt = zdir / "mnt"
    mnt.mkdir(mode=0o700, exist_ok=True)
    os.chmod(mnt, 0o700)
    if host_mounts(zone):
        r = helper("umount-all", zone)
        print(f"[zone] mounts: {(r.stdout or r.stderr).strip()}", flush=True)
    for d in mnt.iterdir():
        try:
            if d.is_dir() and not d.is_symlink():
                d.rmdir()  # empty mount points; never what is still mounted (not empty)
        except OSError:
            pass


def bwrap_argv(z: zonecfg.Zone, zdir: Path, zrt: Path, network: str, extra_path: list[str]) -> list[str]:
    s = z.settings
    home = Path(os.path.expanduser(s["home"])) if s["home"] else zdir / "home"
    if s["home"]:
        # checked where it lands (also before it exists) by the validator;
        # pinned and checked again on the descriptor by netsetup
        err = zonecfg.bind_error(s["home"])
        if err and not err.endswith(": does not exist"):
            die(f"home {err}")
    home.mkdir(parents=True, exist_ok=True)
    a = [tool("bwrap"), "--unshare-user", "--unshare-ipc", "--unshare-pid", "--unshare-uts", "--unshare-cgroup-try",
         "--uid", str(UID), "--gid", str(GID), "--hostname", f"{z.name}-zone",
         "--die-with-parent", "--new-session", "--cap-drop", "ALL", "--clearenv"]
    if network == "none":
        a += ["--unshare-net"]
    if not s["userns"]:
        a += ["--disable-userns", "--assert-userns-disabled"]
    a += ["--proc", "/proc", "--dev", "/dev"]
    if s["tmp"] == "disk":
        # kept across restarts, reachable from the host (prepare_tmp: 0700, cleaned at start)
        a += ["--bind", str(zdir / "tmp"), "/tmp", "--bind", str(zdir / "var-tmp"), "/var/tmp"]
    else:
        a += ["--tmpfs", "/tmp"]
    a += ["--tmpfs", "/run", "--perms", "0700", "--tmpfs", ZRT, "--dir", "/run/zone",
          "--ro-bind", "/nix/store", "/nix/store"]
    # runtime mounts (appzones mount): read-only here, so the zone cannot plant
    # anything in it; a host mount below .../mnt propagates in (bwrap makes
    # its mounts slaves of the host's, and /home is shared on the host)
    a += ["--ro-bind", str(zdir / "mnt"), "/mnt/host"]
    if s["nix_ld"]:
        shim, lib = zonecfg.host_nix_ld() or die("nix_ld = true, but this host has no nix-ld")
        a += ["--dir", "/lib64", "--symlink", shim, "/lib64/ld-linux-x86-64.so.2"]
    for link in ("/run/current-system", f"/etc/profiles/per-user/{USER}", "/run/opengl-driver",
                 "/run/opengl-driver-32"):
        if os.path.lexists(link):
            a += ["--symlink", os.path.realpath(link), link]
    a += ["--symlink", "/run/current-system/sw/bin", "/bin"]
    a += ["--symlink", "/run/current-system/sw/bin/env", "/usr/bin/env"]
    for e in ETC:
        p = Path("/etc") / e
        if not os.path.lexists(p) or (e == "hosts" and s["dns"] == "public"):
            continue
        if p.is_symlink():
            a += ["--symlink", os.readlink(p), str(p)]
        else:
            a += ["--ro-bind", str(p), str(p)]
    if s["machine_id"] == "host" and os.path.exists("/etc/machine-id"):
        a += ["--ro-bind", "/etc/machine-id", "/etc/machine-id"]
    else:
        a += ["--ro-bind", str(zdir / "machine-id"), "/etc/machine-id"]
    a += ["--ro-bind", str(zrt / "resolv.conf"), "/etc/resolv.conf"]
    if s["dns"] == "public":
        # a host's /etc/hosts may carry a blocklist (~145k 0.0.0.0 entries), so
        # "public" also means a minimal hosts file.
        a += ["--ro-bind", str(zrt / "hosts"), "/etc/hosts"]
    a += ([PIN, "rw", s["home"], str(HOME)] if s["home"] else ["--bind", str(home), str(HOME)])
    if s["theme"]:
        for src, dst in copy_theme(zdir):
            a += ["--ro-bind", str(src), dst]
    for b in s["binds"]:
        # never a path here: netsetup opens it, checks what it opened, and
        # bwrap mounts that descriptor (no window to swap in a symlink)
        a += [PIN, b.get("mode", "rw"), b["path"], str(zonecfg.expand(b["path"]))]
    devices = s["devices"]
    if "gpu" in devices:
        a += gpu_binds()
    if "camera" in devices and s["camera"] == "device":
        for v in sorted(glob.glob("/dev/video*")):
            a += ["--dev-bind", v, v]
    if "usb" in devices:
        ub, found = usb_binds(s["usb"])
        if found:
            print(f"[zone] usb: {'; '.join(found)}", flush=True)
            if "/sys/dev/char" not in a:  # gpu_binds may have it already
                ub += ["--ro-bind-try", "/sys/dev/char", "/sys/dev/char"]
            a += ub
        else:
            warn(f"usb: no device matches {', '.join(s['usb'])} now; the zone starts without one "
                 f"(plug it in, then `appzones restart {z.name}`)")
    a += ["--ro-bind-try", "/sys/devices/system/cpu", "/sys/devices/system/cpu"]
    a += ["--bind", str(zrt / "ctl"), "/run/zone"]
    if (zrt / "ctl/bin").is_dir():
        # the xdg-open shim: first on PATH, so never writable from inside
        a += ["--ro-bind", str(zrt / "ctl/bin"), "/run/zone/bin"]
    env = {
        "HOME": str(HOME), "USER": USER, "LOGNAME": USER,
        "PATH": ":".join(extra_path + ["/run/zone/bin", f"/etc/profiles/per-user/{USER}/bin",
                                       "/run/current-system/sw/bin"]),
        "LANG": os.environ.get("LANG", "C.UTF-8"), "TZ": os.environ.get("TZ", ":/etc/localtime"),
        "XDG_RUNTIME_DIR": ZRT, "ZONE": z.name, "TERM": "xterm-256color",
        "XDG_DATA_DIRS": f"/etc/profiles/per-user/{USER}/share:/run/current-system/sw/share",
        "XDG_CONFIG_DIRS": "/etc/xdg", "XDG_SESSION_TYPE": "wayland",
        "NIXOS_OZONE_WL": "1", "MOZ_ENABLE_WAYLAND": "1", "QT_QPA_PLATFORM": "wayland;xcb",
        "SDL_VIDEODRIVER": "wayland", "ELECTRON_OZONE_PLATFORM_HINT": "wayland",
        "TZDIR": "/etc/zoneinfo",
    }
    # the host session's desktop name (portal backends and toolkits pick by it)
    desk = os.environ.get("XDG_CURRENT_DESKTOP", "")
    if desk and all(c.isalnum() or c in ":_-" for c in desk):
        env["XDG_CURRENT_DESKTOP"] = desk
    if s["nix_ld"]:
        # as the host's environment.variables (programs.nix-ld): the real
        # loader and the libraries a generic binary gets
        env["NIX_LD"] = f"{lib}/ld.so"
        env["NIX_LD_LIBRARY_PATH"] = lib
    for k in ("LC_ALL", "LC_CTYPE", "LC_TIME", "LC_NUMERIC", "LC_MONETARY", "LC_MESSAGES", "XCURSOR_THEME",
              "XCURSOR_SIZE", "QT_QPA_PLATFORMTHEME", "GTK_THEME", "GDK_SCALE", "QT_SCALE_FACTOR"):
        if os.environ.get(k):
            env[k] = os.environ[k]
    if (zrt / "wayland-0").exists():
        a += ["--bind", str(zrt / "wayland-0"), f"{ZRT}/wayland-0"]
        env["WAYLAND_DISPLAY"] = "wayland-0"
    if (zrt / "bus").exists():
        a += ["--bind", str(zrt / "bus"), f"{ZRT}/bus"]
        env["DBUS_SESSION_BUS_ADDRESS"] = f"unix:path={ZRT}/bus"
    # for zone-init: the zone's own bus (bus = "zone") and the nested bwrap
    # that hides every bus (bus = "none"); store paths, visible inside
    env["ZONE_DBUS_DAEMON"] = os.path.realpath(tool("dbus-daemon"))
    env["ZONE_BWRAP"] = os.path.realpath(tool("bwrap"))
    if "audio" in devices or "mic" in devices:
        # "mic" is an alias of "audio" for now (the whole PipeWire graph);
        # a real mic toggle needs a restricted per-zone socket.
        if (HOST_RT / "pipewire-0").exists():
            a += ["--bind", str(HOST_RT / "pipewire-0"), f"{ZRT}/pipewire-0"]
        if (HOST_RT / "pulse/native").exists():
            a += ["--bind", str(HOST_RT / "pulse/native"), f"{ZRT}/pulse/native"]
            env["PULSE_SERVER"] = f"unix:{ZRT}/pulse/native"
    doc = HOST_RT / "doc/by-app" / f"dev.mirql.zone.{z.name}"
    try:
        if doc.is_dir():
            a += ["--bind", str(doc), f"{ZRT}/doc"]
    except OSError:
        pass
    a += ["--ro-bind", str(zrt / "flatpak-info"), "/.flatpak-info"]
    a += ["--info-fd", "3", "--lock-file", "/.flatpak-info"]
    env.update(s["env"])
    for k, v in env.items():
        a += ["--setenv", k, v]
    return a + ["--", os.path.realpath(sys.executable), str(HERE / "zone_init.py")]


# ── portals: flatpak-info and the D-Bus proxy ──────────────────────────────
def flatpak_info(z: zonecfg.Zone, instance: str) -> str:
    return (f"[Application]\nname=dev.mirql.zone.{z.name}\nruntime=runtime/dev.mirql.zones/x86_64/1\n\n"
            f"[Instance]\ninstance-id={instance}\nsession-bus-proxy=true\nsystem-bus-proxy=false\n"
            f"sandbox-engine=dev.mirql.zones\n")


# The portal interfaces a zone may call on org.freedesktop.portal.Desktop.
# Not flatpak's "org.freedesktop.portal.*" pair: that also grants
# org.freedesktop.portal.Flatpak, whose Spawn acts on the caller's
# .flatpak-info. Left out on purpose: RemoteDesktop and
# InputCapture (input injection), Background and DynamicLauncher (they write
# host autostart and launcher entries), Usb, Device, Clipboard.
PORTALS = ["FileChooser", "OpenURI", "Notification", "Settings", "Email", "Print", "Trash", "Inhibit",
           "NetworkMonitor", "ProxyResolver", "PowerProfileMonitor", "MemoryMonitor", "GameMode", "Realtime",
           "Secret", "Request", "Session", "GlobalShortcuts"]
PORTALS_BY_DEVICE = {"screencast": ["ScreenCast", "Screenshot"], "camera": ["Camera"]}
# zone-init gives these their own D-Bus app name per zone (MOZ_DBUS_APP_NAME) (zone_init.py MOZ_APPS)
MOZ_APPS = ("firefox", "firefox-esr", "firefox-beta", "firefox-devedition", "firefox-nightly", "librewolf")


def moz_remoting_name(zone: str, app: str) -> str:
    return f"{app.replace('-', '_')}_zone_{zone.replace('-', '_')}"


def dbus_rules(z: zonecfg.Zone) -> list[str]:
    devices = z.settings["devices"]
    allowed = list(dict.fromkeys(PORTALS + [p for d, ps in PORTALS_BY_DEVICE.items() if d in devices for p in ps]))
    rules = ["--call=org.freedesktop.portal.Desktop=org.freedesktop.DBus.Properties.*",
             "--call=org.freedesktop.portal.Desktop=org.freedesktop.DBus.Introspectable.*",
             "--call=org.freedesktop.portal.Desktop=org.freedesktop.DBus.Peer.*"]
    rules += [f"--call=org.freedesktop.portal.Desktop=org.freedesktop.portal.{p}.*" for p in allowed]
    rules += ["--broadcast=org.freedesktop.portal.Desktop=@/org/freedesktop/portal/*",
              "--talk=org.freedesktop.portal.Documents", "--talk=org.freedesktop.Notifications",
              "--talk=org.kde.StatusNotifierWatcher", f"--own=dev.mirql.zone.{z.name}.*"]
    # no org.mpris.MediaPlayer2.*: owning implies talking, and that name is
    # shared by the host's and every zone's players; a zone opts in per
    # player with dbus.own (docs/configuration/dbus.md)
    # each Mozilla app's remote name in this zone only: never org.mozilla.firefox.*,
    # which the host's Firefox and every other zone's would share
    rules += [f"--own=org.mozilla.{moz_remoting_name(z.name, a)}.*" for a in MOZ_APPS]
    rules += [f"--talk={n}" for n in z.settings["dbus"].get("talk", [])]
    rules += [f"--own={n}" for n in z.settings["dbus"].get("own", [])]
    if zonecfg.env("DBUS_LOG"):
        rules.append("--log")
    return rules


def dbus_proxy_argv(z: zonecfg.Zone, zrt: Path) -> list[str]:
    host_bus = os.environ["DBUS_SESSION_BUS_ADDRESS"]
    # The proxy runs in its own small bwrap that carries the zone's
    # /.flatpak-info: the portal reads /proc/<peer pid>/root/.flatpak-info,
    # and the peer it sees is the proxy (flatpak-run.c add_bwrap_wrapper).
    return [tool("bwrap"), "--unshare-all", "--share-net", "--die-with-parent",
            "--ro-bind", "/nix/store", "/nix/store", "--bind", str(HOST_RT), str(HOST_RT),
            "--ro-bind", str(zrt / "flatpak-info"), "/.flatpak-info",
            "--", tool("xdg-dbus-proxy"), host_bus, str(zrt / "bus"), "--filter", *dbus_rules(z)]


# ── links: an in-zone xdg-open that hands http(s) URLs to a zone's browser ─
XDG_OPEN = """#!/run/current-system/sw/bin/sh
# zone xdg-open (links = "{links}"): http(s) URLs open in zone {target}'s
# browser through the zone's link socket; anything else goes to the real xdg-open.
case "$1" in
  http://*|https://*)
    exec {python} -c 'import socket,sys; s=socket.socket(socket.AF_UNIX); s.connect("/run/zone/links.sock"); s.sendall(sys.argv[1].encode()+b"\\n"); sys.exit(0 if s.recv(3)==b"ok\\n" else 1)' "$1" ;;
esac
exec /run/current-system/sw/bin/xdg-open "$@"
"""


def link_target(z: zonecfg.Zone) -> str | None:
    links = z.settings["links"]
    if links == "portal":
        return None
    return z.name if links == "zone" else links[5:]


def browser_for(cfg: zonecfg.Config, zone: str) -> str | None:
    """The first declared app for ZONE that is a browser (firefox, chromium, ...)."""
    for aid, a in cfg.apps.items():
        cmd = os.path.basename(a["command"][0]).lower()
        if zone in a.get("zones", []) and any(b in cmd for b in ("firefox", "chromium", "chrome", "librewolf", "brave")):
            return aid
    return None


def serve_links(z: zonecfg.Zone, zrt: Path, target: str) -> socket.socket:
    """Listen on the zone's link socket (host side). Only an http(s) URL, as
    one argv element, ever leaves the zone this way."""
    srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    path = zrt / "ctl" / "links.sock"
    srv.bind(str(path))
    srv.listen(8)
    srv.setblocking(False)
    return srv


def handle_link(srv: socket.socket, target: str) -> None:
    try:
        conn, _ = srv.accept()
    except BlockingIOError:
        return
    with conn:
        conn.settimeout(2)
        try:
            url = conn.recv(8192).decode(errors="replace").strip()
        except OSError:
            return
        ok = (url.startswith(("http://", "https://")) and len(url) < 8000 and not any(c in url for c in "\n\r\0"))
        if ok:
            cfg = zonecfg.load(CONFIG)
            app = browser_for(cfg, target) if not cfg.errors else None
            if app:
                subprocess.Popen([sys.executable, str(HERE / "zone.py"), "run", target, app, url],
                                 stdin=subprocess.DEVNULL, start_new_session=True)
            else:
                ok = False
                print(f"[zone] links: no browser declared for zone {target}", flush=True)
        try:
            conn.sendall(b"ok\n" if ok else b"no\n")
        except OSError:
            pass


# ── supervisor ─────────────────────────────────────────────────────────────
def write_colours(cfg: zonecfg.Config) -> None:
    RT.mkdir(parents=True, exist_ok=True)
    # one temporary file per writer: zones start concurrently at login
    fd, tmp = tempfile.mkstemp(dir=RT, prefix=".colours.")
    try:
        with os.fdopen(fd, "w") as f:
            f.write("".join(f"{n} {z.settings['colour']}\n" for n, z in cfg.zones.items()))
        os.replace(tmp, RT / "colours")
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


def supervise(name: str) -> int:
    cfg = load_cfg(quiet=False)
    if name not in cfg.zones:
        die(f"no zone {name!r} in {CONFIG}")
    z = cfg.zones[name]
    s = z.settings
    zdir, zrt = STATE / name, RT / name
    zdir.mkdir(parents=True, exist_ok=True)
    RT.mkdir(parents=True, exist_ok=True)
    os.chmod(RT, 0o700)
    if zrt.exists():
        shutil.rmtree(zrt)
    (zrt / "ctl").mkdir(parents=True, mode=0o700)
    if not (zdir / "machine-id").exists():
        (zdir / "machine-id").write_text(secrets.token_hex(16) + "\n")
    (zrt / "resolv.conf").write_text(resolv_conf(s))
    (zrt / "hosts").write_text(f"127.0.0.1 localhost\n::1 localhost\n127.0.0.2 {name}-zone\n")
    instance = f"zone-{name}-{secrets.randbelow(2**31)}"
    (zrt / "flatpak-info").write_text(flatpak_info(z, instance))
    (zrt / "config.sha256").write_text(settings_hash(z) + "\n")
    write_colours(cfg)
    if s["tmp"] == "disk":
        prepare_tmp(zdir)
    prepare_mnt(name, zdir)
    extra_path = package_paths(z, zdir) if s["packages"] else []
    target = link_target(z)
    links_srv = None
    if target:
        (zrt / "ctl/bin").mkdir()
        xo = zrt / "ctl/bin/xdg-open"
        xo.write_text(XDG_OPEN.format(links=s["links"], target=target, python=os.path.realpath(sys.executable)))
        xo.chmod(0o755)
        links_srv = serve_links(z, zrt, target)
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
        helpers.append(subprocess.Popen(dbus_proxy_argv(z, zrt)))
        for _ in range(100):
            if (zrt / "bus").exists():
                break
            time.sleep(0.05)
        else:
            stop_all(); die("xdg-dbus-proxy did not create the zone's bus")
    network = s["network"]
    (zrt / "nft.rules").write_text(nft_rules(z))
    # bwrap writes its info (child-pid) to fd 3; the portal reads it as
    # $XDG_RUNTIME_DIR/.flatpak/<instance>/bwrapinfo.json
    fp = HOST_RT / ".flatpak" / instance
    fp.mkdir(parents=True, exist_ok=True)
    inner = bwrap_argv(z, zdir, zrt, network, extra_path)
    # pasta closes inherited fds, so the info fd is opened by _netsetup, in
    # pasta's namespace, right before it execs bwrap.
    argv = [sys.executable, str(HERE / "zone.py"), "_netsetup", str(zrt / "nft.rules") if network == "internet" else "-",
            str(fp / "bwrapinfo.json"), "--"] + inner
    if network == "internet":
        argv = pasta_argv(z) + argv
    child = subprocess.Popen(argv)
    # ready: zone-init answers; then the autostart apps
    for _ in range(200):
        if child.poll() is not None or ping(name):
            break
        time.sleep(0.05)
    if child.poll() is None and not ping(name):
        print(f"[zone] {name}: zone-init did not answer within 10 s; stopping", flush=True)
        stop_all()
        child.wait()
        shutil.rmtree(fp, ignore_errors=True)
        return 1
    if child.poll() is None:
        sd_notify("READY=1\nSTATUS=zone " + name + " running")
        print(f"[zone] {name} ready (network={network}, dns={s['dns']}, devices={','.join(s['devices']) or '-'})",
              flush=True)
        for app in s["autostart_apps"]:
            try:
                r = send_run(name, zonecfg.resolve_app(cfg, name, app) or [app], bus=zonecfg.app_bus(cfg, name, app))
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
            handle_link(links_srv, target)
    stop_all()
    shutil.rmtree(fp, ignore_errors=True)
    prepare_mnt(name, zdir)  # runtime mounts end with the zone
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


def netsetup(rules: str, info: str, argv: list[str]) -> None:
    if rules != "-":
        r = subprocess.run([tool("nft"), "-f", rules])
        if r.returncode != 0:
            die("nft rules failed to load; refusing to start the zone without them")
    fd = os.open(info, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    if fd != 3:
        os.dup2(fd, 3)
        os.close(fd)
    os.set_inheritable(3, True)
    argv = pin_binds(argv)  # after fd 3: pinned descriptors are 4 and up
    os.execvp(argv[0], argv)


# ── client ─────────────────────────────────────────────────────────────────
def request(name: str, req: dict, fds: list[int] | None = None, timeout: float | None = 5.0) -> socket.socket:
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    s.connect(str(RT / name / "ctl" / "ctl.sock"))
    data = (json.dumps(req) + "\n").encode()
    if fds:
        socket.send_fds(s, [data], fds)
    else:
        s.sendall(data)
    return s


def readline(s: socket.socket, buf: bytes = b"") -> tuple[dict, bytes]:
    """One JSON line; returns it and any bytes that came after it (pass those
    back as BUF for the next line: two replies can arrive in one read)."""
    while b"\n" not in buf:
        chunk = s.recv(65536)
        if not chunk:
            break
        buf += chunk
    line, _, rest = buf.partition(b"\n")
    return (json.loads(line.decode()) if line.strip() else {}), rest


def ping(name: str) -> dict | None:
    try:
        with request(name, {"op": "ping"}, timeout=1.0) as s:
            return readline(s)[0]
    except (OSError, ValueError):
        return None


def send_run(name: str, argv: list[str], extra_env: dict | None = None, cwd: str | None = None,
             bus: str = "host") -> int:
    with request(name, {"op": "run", "argv": argv, "env": extra_env or {}, "cwd": cwd, "bus": bus}) as s:
        r = readline(s)[0]
    if not r.get("ok"):
        raise OSError(r.get("error", "run failed"))
    return r["pid"]


def zone_procs(name: str) -> list[list]:
    """Processes in the zone other than bwrap (pid 1) and zone-init."""
    with request(name, {"op": "ps"}) as s:
        r = readline(s)[0]
    init = r.get("init")
    return [p for p in r.get("procs", []) if p[0] not in (1, init)]


def up(name: str, quiet: bool = False) -> None:
    if ping(name):
        return
    load_zone(name)
    if use_systemd():
        if not quiet:
            warn(f"starting zone {name}")
        r = systemctl("start", f"appzones@{name}.service", check=False)
        if r.returncode != 0:
            die(f"zone {name} failed to start; see `appzones logs {name}`\n{r.stderr.strip()}")
    else:
        RT.mkdir(parents=True, exist_ok=True)
        log = open(RT / f"{name}.log", "ab")
        subprocess.Popen([sys.executable, str(HERE / "zone.py"), "_supervise", name], stdout=log, stderr=log,
                         stdin=subprocess.DEVNULL, start_new_session=True)
    for _ in range(200):
        if ping(name):
            return
        time.sleep(0.05)
    die(f"zone {name} did not answer within 10 s; see `appzones logs {name}`")


def stale(name: str, z: zonecfg.Zone) -> bool:
    try:
        return (RT / name / "config.sha256").read_text().strip() != settings_hash(z)
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
        die(f"no zone {name!r} in {CONFIG}")
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
    if ping(name) and stale(name, z):
        n = len(zone_procs(name))
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
        s = request(name, req, fds, timeout=None)
    else:
        s = request(name, req)
    first, rest = readline(s)
    if not first.get("ok"):
        die(first.get("error", "run failed"))
    if not wait:
        s.close()
        return watch(name, first["pid"], argv, bus, app)
    return readline(s, rest)[0].get("exit", 255)


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
            with request(name, {"op": "status", "pid": pid}, timeout=2) as s:
                st = readline(s)[0]
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
             f"owns names on the host bus. Give it the zone's own bus: add {where} to {CONFIG}, "
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
    s = request(name, {"op": "shell", "rows": rows, "cols": cols, "term": os.environ.get("TERM", "xterm-256color")},
                timeout=None)
    first, rest = readline(s)
    if not first.get("ok"):
        die(first.get("error", "shell failed"))
    pid = first["pid"]
    out = sys.stdout.fileno()
    if rest:
        os.write(out, rest)

    def resize(*_):
        c, r = shutil.get_terminal_size()
        try:
            with request(name, {"op": "resize", "pid": pid, "rows": r, "cols": c}, timeout=1) as rs:
                readline(rs)
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


def stop_zone(name: str) -> None:
    if use_systemd():
        systemctl("stop", f"appzones@{name}.service")
        return
    out = subprocess.run(["pgrep", "-f", f"zone.py _supervise {name}$"], capture_output=True, text=True).stdout.split()
    for p in out:
        os.kill(int(p), signal.SIGTERM)
    for _ in range(100):
        if not ping(name):
            return
        time.sleep(0.1)


def restart_zone(name: str) -> None:
    if use_systemd():
        systemctl("restart", f"appzones@{name}.service")
        for _ in range(200):
            if ping(name):
                return
            time.sleep(0.05)
        return
    stop_zone(name)
    up(name, quiet=True)


def zone_list() -> int:
    cfg = load_cfg(quiet=True)
    rows = [("ZONE", "STATE", "NETWORK", "DNS", "DEVICES", "APPS", "COLOUR")]
    for n, z in cfg.zones.items():
        s = z.settings
        net = s["network"] + ("+netbird" if s["netbird"] else "")
        if ping(n):
            try:
                apps = str(len(zone_procs(n)))
            except OSError:
                apps = "?"
            state = "running" + (" (stale)" if stale(n, z) else "")
        else:
            state, apps = "stopped", "-"
        devs = [f"usb({'+'.join(s['usb'])})" if d == "usb" else d for d in s["devices"]]
        rows.append((n, state, net, s["dns"], ",".join(devs) or "-", apps, s["colour"]))
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
        cfg = load_cfg(quiet=True) if os.path.lexists(CONFIG) else zonecfg.Config({}, {}, [])
        if name in cfg.zones:
            die(f"zone {name!r} already exists")
        print(new_block(cfg, name).strip())
        print(f"\n(not written; `appzones new {name} -y` appends it to {CONFIG})", file=sys.stderr)
        return 0
    real = Path(os.path.realpath(CONFIG))  # a stowed link: write where it points
    real.parent.mkdir(parents=True, exist_ok=True)
    # one writer at a time; the file is replaced whole (never truncated in place)
    with open(real.parent / f".{real.name}.lock", "a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        old = real.read_text() if real.exists() else NEW_FILE
        cfg = zonecfg.load_text(old, real) if old.strip() else zonecfg.Config({}, {}, [])
        if cfg.errors:
            die(f"{CONFIG} is invalid:\n  " + "\n  ".join(cfg.errors))
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
    colour = next((c for c in PALETTE if c not in used), "#" + secrets.token_hex(3))
    return NEW_BLOCK.format(name=name, colour=colour)


def check(probe: str | None) -> int:
    cfg = zonecfg.load(CONFIG)
    for e in cfg.errors:
        print(f"error: {e}", file=sys.stderr)
    for w in cfg.warnings:
        print(f"warning: {w}", file=sys.stderr)
    if cfg.errors:
        return 1
    print(f"ok: {CONFIG}: {len(cfg.zones)} zones ({', '.join(cfg.zones)}), {len(cfg.apps)} apps "
          f"(this zone is release {zonecfg.RELEASE})")
    if not probe:
        return 0
    z = load_zone(probe)
    up(probe, quiet=True)
    tcp = "timeout 3 bash -c '</dev/tcp/{}/{}' 2>/dev/null"
    probes = [
        ("~/.ssh absent", 1, "ls -d ~/.ssh"),
        ("runtime dir: zone sockets only", 0,
         f"[ -z \"$(ls -A {ZRT} | grep -v -x -e bus -e zone-bus -e wayland-0 -e doc -e pipewire-0 -e pulse)\" ]"),
        ("no cgroupfs", 1, "ls /sys/fs/cgroup"),
        ("/nix/var absent", 1, "ls -d /nix/var"),
        ("capabilities empty", 0, "grep -q 'CapEff:.0000000000000000' /proc/self/status"),
        ("no_new_privs", 0, "grep -q 'NoNewPrivs:.1' /proc/self/status"),
        ("no host secrets in env", 1, "env | grep -E 'SSH_AUTH|HYPRLAND|TOKEN|SECRET|PASSWORD'"),
        *[(f"{p} absent", 1, f"ls -d {shlex.quote(str(p))}") for p in zonecfg.host_denied_paths()
          if p.exists() and not p.is_relative_to(HOME)],
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
        r = subprocess.run([sys.executable, str(HERE / "zone.py"), "run", "--wait", probe, "--",
                            "/run/current-system/sw/bin/bash", "-c", cmd],
                           stdin=subprocess.DEVNULL, capture_output=True, text=True)
        ok = (r.returncode == 0) == (expect == 0)
        fails += not ok
        print(f"{'PASS' if ok else 'FAIL'}  {title}")
    return 1 if fails else 0


def find_desktop(name: str) -> dict:
    dirs = [os.environ.get("XDG_DATA_HOME", str(HOME / ".local/share"))]
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
    LAUNCHER.mkdir(parents=True, exist_ok=True)
    for old in [*LAUNCHER.glob("appzones-*.desktop"), *LAUNCHER.glob("zone-*.desktop")]:  # zone-*: before 0.6
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
            (LAUNCHER / f"appzones-{aid}-{zn}.desktop").write_text("\n".join(lines) + "\n")
            n += 1
    print(f"{n} launcher entries in {LAUNCHER}")
    return 0


def autostart() -> int:
    cfg = load_cfg(quiet=True)
    write_colours(cfg)
    apps()
    for n, z in cfg.zones.items():
        if z.settings["autostart"]:
            print(f"autostart: zone {n}")
            systemctl("start", "--no-block", f"appzones@{n}.service", check=False)
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
    s.connect(str(HOST_RT / "hypr" / sig / ".socket2.sock"))
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
        os.execvp("tail", ["tail", "-n", "200", *(["-F"] if follow else []), str(RT / f"{name}.log")])
    os.execvp("journalctl", ["journalctl", "--user", "-u", f"appzones@{name}.service", "-n", "200", "--no-pager",
                             *(["-f"] if follow else [])])


# ── runtime mounts ─────────────────────────────────────────────────────────
def zone_request(name: str, req: dict) -> dict:
    """One request to zone-init; {} if it does not answer or know the op."""
    try:
        with request(name, req, timeout=10) as s:
            r = readline(s)[0]
        return r if r.get("ok") else {}
    except (OSError, ValueError):
        return {}


def mounts_file(name: str) -> Path:
    return RT / name / "mounts.json"  # sources, for `appzones mounts`; gone when the zone restarts


def read_mounts(name: str) -> dict:
    try:
        return json.loads(mounts_file(name).read_text())
    except (OSError, ValueError):
        return {}


def write_mounts(name: str, m: dict) -> None:
    tmp = mounts_file(name).with_suffix(".tmp")
    tmp.write_text(json.dumps(m, indent=1) + "\n")
    tmp.replace(mounts_file(name))


def helper_failed(r: subprocess.CompletedProcess, what: str):
    err = (r.stderr or r.stdout).strip()
    if "password is required" in err or "not allowed" in err or "may not run sudo" in err:
        die(f"{what}: sudo refused the mount helper ({err.splitlines()[-1]}). It needs the NixOS module's sudo "
            f"rule (programs.appzones.mountHelper = true, for programs.appzones.user) and a rebuilt system")
    die(f"{what}: {err.removeprefix('appzones-mount-helper: ') or r.returncode}")


def mount(name: str, path: str, ro: bool, as_name: str | None) -> int:
    load_zone(name)
    if not ping(name):
        die(f"zone {name} is not running; a mount lives only while the zone runs (`appzones up {name}` first)")
    err = zonecfg.mount_source_error(path)
    if err:
        die(f"refused: {err}")
    src = os.path.realpath(zonecfg.expand(path))
    nm = as_name or os.path.basename(src)
    if not zonecfg.MOUNT_NAME_RE.match(nm):
        die(f"{nm!r} cannot be a mount name (letters, digits, . _ -, not first; at most 64): give one with --as NAME")
    if nm in host_mounts(name):
        die(f"/mnt/host/{nm} is already mounted in zone {name} (appzones mounts {name})")
    target = STATE / name / "mnt" / nm
    target.mkdir(exist_ok=True)
    r = helper("mount", name, nm, src, *(["--ro"] if ro else []))
    if r.returncode != 0:
        try:
            target.rmdir()
        except OSError:
            pass
        helper_failed(r, "mount")
    m = read_mounts(name)
    m[nm] = {"source": src, "mode": "ro" if ro else "rw"}
    write_mounts(name, m)
    inside = zone_request(name, {"op": "mounts"})
    if inside and f"/mnt/host/{nm}" not in [p for p, _ in inside.get("mounts", [])]:
        warn(f"mounted on the host, but zone {name} does not see /mnt/host/{nm} (did the zone start before release "
             f"3? `appzones restart {name}`)")
    print(f"{src} is /mnt/host/{nm} in zone {name} ({'read-only' if ro else 'read-write'}), until "
          f"`appzones umount {name} {nm}` or the zone stops")
    return 0


def umount(name: str, nm: str, lazy: bool) -> int:
    load_zone(name)
    if nm not in host_mounts(name):
        die(f"nothing is mounted at /mnt/host/{nm} in zone {name} (appzones mounts {name})")
    r = helper("umount", name, nm, *(["--lazy"] if lazy else []))
    if r.returncode == 3:
        busy = zone_request(name, {"op": "busy", "path": f"/mnt/host/{nm}"}).get("procs", [])
        warn(f"/mnt/host/{nm} is busy in zone {name}" + (":" if busy else " (zone-init could not say by what)"))
        for pid, cmd, what in busy:
            print(f"  pid {pid} {cmd[:60]}: {what}", file=sys.stderr)
        die(f"close those (or stop the app), or detach it now: appzones umount --lazy {name} {nm}")
    if r.returncode != 0:
        helper_failed(r, "umount")
    try:
        (STATE / name / "mnt" / nm).rmdir()
    except OSError:
        pass
    m = read_mounts(name)
    m.pop(nm, None)
    if mounts_file(name).parent.is_dir():
        write_mounts(name, m)
    print(f"/mnt/host/{nm} removed from zone {name}" + (" (lazily: open files keep it until they close)" if lazy else ""))
    return 0


def mounts(name: str) -> int:
    load_zone(name)
    names = host_mounts(name)
    if not ping(name):
        if names:
            print(f"zone {name} is not running; left mounted on the host (cleared at its next start): "
                  + ", ".join(names))
        return 0
    m = read_mounts(name)
    inside = zone_request(name, {"op": "mounts"})
    modes = {p[len("/mnt/host/"):]: mode for p, mode in inside.get("mounts", [])} if inside else {}
    rows = [("NAME", "MODE", "SOURCE")]
    for nm in names:
        rows.append((nm, modes.get(nm, m.get(nm, {}).get("mode", "?")), m.get(nm, {}).get("source", "?")))
    if len(rows) == 1:
        print(f"nothing mounted in zone {name} (appzones mount {name} DIR)")
        return 0
    w = [max(len(r[i]) for r in rows) for i in range(3)]
    for row in rows:
        print("  ".join(c.ljust(w[i]) for i, c in enumerate(row)).rstrip())
    return 0


# ── upgrading from before 0.6 ──────────────────────────────────────────────
def supervisors() -> list[str]:
    """Command lines of this user's zone supervisors (any appzones version),
    other than this process."""
    out = []
    for d in Path("/proc").iterdir():
        if not d.name.isdigit() or int(d.name) == os.getpid():
            continue
        try:
            if d.stat().st_uid != UID:
                continue
            argv = (d / "cmdline").read_bytes().split(b"\0")
        except OSError:
            continue
        if b"_supervise" in argv and any(a.endswith(b"zone.py") for a in argv):
            out.append(b" ".join(argv).decode(errors="replace"))
    return out


def mounted_below(path: Path) -> bool:
    try:
        points = [line.split()[4] for line in open("/proc/self/mountinfo")]
    except OSError:
        return True  # cannot tell: do not move
    p = str(path)
    return any(m == p or m.startswith(p + "/") for m in points)


def migrate_state() -> None:
    """Before 0.6 the state (zone homes, launcher entries, tmp = "disk") was
    ~/.local/share/zones. It is moved, once, by a rename, to
    ~/.local/share/appzones when no zone runs and nothing is mounted below
    it; until then appzones (and the mount helper) keep using it. The config
    is never moved (often a dotfiles manager's link): a warning says how."""
    global STATE, LAUNCHER
    if LEGACY_CONFIG:
        warn(f"reading {LEGACY_CONFIG}: since 0.6 it belongs in ~/.config/appzones/zones.toml "
             "(mkdir -p ~/.config/appzones && mv ~/.config/zones/zones.toml ~/.config/appzones/)")
    running = supervisors()
    if any("/libexec/zones/" in r for r in running):
        warn("zones started by appzones before 0.6 are running and this version does not see them: "
             "`appzones stop --all`, then start them again")
    if zonecfg.env("STATE"):
        return
    new = HOME / ".local/share/appzones"
    if STATE != LEGACY_STATE:
        if os.path.lexists(LEGACY_STATE) and os.path.lexists(new):
            warn(f"{LEGACY_STATE} is no longer used (since 0.6 {new} is): move what you still need from it, "
                 "then remove it")
        return
    if running or mounted_below(LEGACY_STATE):
        warn(f"{LEGACY_STATE} moves to {new} once no zone runs (appzones stop --all)")
        return
    try:
        os.rename(LEGACY_STATE, new)
    except OSError as e:
        if not os.path.lexists(new):  # another appzones may have moved it just now
            warn(f"could not move {LEGACY_STATE} to {new}: {e.strerror or e}; using it where it is")
            return
    else:
        warn(f"moved {LEGACY_STATE} to {new} (appzones 0.6)")
    STATE, LAUNCHER = new, new / "launcher/share/applications"


def main(argv: list[str]) -> int:
    if not argv or argv[0] in ("-h", "--help", "help"):
        print(__doc__)
        return 0 if argv else 2
    cmd, rest = argv[0], argv[1:]
    if cmd == "_autostart" or not (cmd.startswith("_") or cmd == "active"):
        migrate_state()

    def one() -> str:
        if not rest:
            die(f"usage: appzones {cmd} ZONE")
        return rest[0]

    def each(fn) -> int:
        names = list(load_cfg(quiet=True).zones) if rest[:1] == ["--all"] else [one()]
        for n in names:
            if cmd != "stop" or rest[:1] != ["--all"] or ping(n):
                load_zone(n)
                fn(n)
        return 0

    if cmd == "_supervise":
        return supervise(one())
    if cmd == "_netsetup":
        i = rest.index("--")
        netsetup(rest[0], rest[1], rest[i + 1:])
    if cmd == "_autostart":
        return autostart()
    if cmd == "list":
        return zone_list()
    if cmd == "which":
        return which(one())
    if cmd == "up":
        return each(up)
    if cmd == "stop":
        return each(stop_zone)
    if cmd == "restart":
        return each(restart_zone)
    if cmd == "ping":
        r = ping(one()); print(json.dumps(r)); return 0 if r else 1
    if cmd == "ps":
        for p, c in zone_procs(one()):
            print(p, c)
        return 0
    if cmd == "logs":
        follow = any(a in ("-f", "--follow") for a in rest)
        rest[:] = [a for a in rest if a not in ("-f", "--follow")]
        return logs(one(), follow)
    if cmd == "shell":
        return shell(one())
    if cmd == "run":
        flags: dict = {"--wait": False, "--restart": False, "--bus": None}

        def take(ws):
            while ws and (ws[0] in flags or ws[0].startswith("--bus=")):
                f = ws.pop(0)
                if f.startswith("--bus"):
                    v = f[6:] if "=" in f else (ws.pop(0) if ws else "")
                    if v not in zonecfg.BUSES:
                        die(f"--bus must be one of {', '.join(zonecfg.BUSES)}")
                    flags["--bus"] = v
                else:
                    flags[f] = True
        take(rest)
        if not rest:
            die("usage: appzones run [--restart] [--bus host|zone|none] [--wait] ZONE APP|-- CMD...")
        name, words = rest[0], rest[1:]
        take(words)
        if flags["--wait"] and words[:1] != ["--"]:
            die("--wait runs a command: appzones run --wait ZONE -- CMD...")
        return run(name, words, flags["--wait"], flags["--restart"], flags["--bus"])
    if cmd == "new":
        return new(one(), "-y" in rest[1:])
    if cmd == "check":
        if rest[:1] == ["--config-only"]:
            rest.pop(0)
        probe = rest[1] if rest[:1] == ["--probe"] and len(rest) > 1 else None
        return check(probe)
    if cmd == "apps":
        return apps()
    if cmd == "active":
        return active("--follow" in rest, "--json" in rest)
    if cmd in ("mount", "umount", "mounts"):
        opts = {"--ro": False, "--lazy": False, "--as": None}
        words = []
        it = iter(rest)
        for a in it:
            if a == "--as" or a.startswith("--as="):
                opts["--as"] = a[5:] if "=" in a else next(it, None)
                if not opts["--as"]:
                    die("--as needs a NAME")
            elif a in ("--ro", "--lazy"):
                opts[a] = True
            elif a.startswith("--"):
                die(f"zone {cmd}: unknown option {a}")
            else:
                words.append(a)
        if cmd == "mount" and len(words) == 2 and not opts["--lazy"]:
            return mount(words[0], words[1], opts["--ro"], opts["--as"])
        if cmd == "umount" and len(words) == 2 and not opts["--ro"] and opts["--as"] is None:
            return umount(words[0], words[1], opts["--lazy"])
        if cmd == "mounts" and len(words) == 1:
            return mounts(words[0])
        die("usage: appzones mount ZONE HOST-DIR [--ro] [--as NAME] | appzones umount ZONE NAME [--lazy] | appzones mounts ZONE")
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except KeyboardInterrupt:
        sys.exit(130)
