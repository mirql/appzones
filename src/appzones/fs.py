"""What a zone sees: theme copy, devices, packages, /tmp, mount points, the bwrap command line."""
from __future__ import annotations

import fnmatch
import glob
import os
import shutil
import stat
import subprocess
import sys
import time
from pathlib import Path

import zonecfg

from .common import die, tool, warn
from . import common, server


# ── filesystem ─────────────────────────────────────────────────────────────
# /etc on a usual (FHS) distribution: what NixOS keeps elsewhere (alternatives
# for /usr/bin links, the loader's cache, the names getaddrinfo reads).
FHS_ETC = ["alternatives", "ld.so.cache", "ld.so.conf", "ld.so.conf.d", "services", "protocols", "gai.conf",
           "ca-certificates", "bash.bashrc", "debian_version", "lsb-release", "environment"]


def system_binds(layout: str | None = None, root: str = "/") -> list[str]:
    """The host's programs, read-only, for a zone and the D-Bus proxy's sandbox:
    /nix/store on NixOS; elsewhere /usr, and /bin /sbin /lib* as the host has
    them (links into /usr on a merged-/usr system)."""
    if (layout or zonecfg.host_layout()) == "nixos":
        return ["--ro-bind", "/nix/store", "/nix/store"]
    a = ["--ro-bind", "/usr", "/usr"]
    for d in ("/bin", "/sbin", "/lib", "/lib64", "/lib32", "/libx32"):
        p = os.path.join(root, d.lstrip("/"))
        if os.path.islink(p):
            a += ["--symlink", os.readlink(p), d]
        elif os.path.isdir(p):
            a += ["--ro-bind", d, d]
    return a


def system_env(layout: str | None = None) -> dict[str, str]:
    """PATH (the system part), XDG_DATA_DIRS and TZDIR inside a zone."""
    if (layout or zonecfg.host_layout()) == "nixos":
        return {"PATH": f"/etc/profiles/per-user/{common.USER}/bin:/run/current-system/sw/bin",
                "XDG_DATA_DIRS": f"/etc/profiles/per-user/{common.USER}/share:/run/current-system/sw/share",
                "TZDIR": "/etc/zoneinfo"}
    return {"PATH": "/usr/local/bin:/usr/bin:/usr/local/sbin:/usr/sbin:/bin:/sbin",  # /bin, /sbin: unmerged hosts
            "XDG_DATA_DIRS": "/usr/local/share:/usr/share", "TZDIR": "/usr/share/zoneinfo"}


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
        src = common.HOME / rel
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
            out.append((dst, str(common.HOME / rel)))
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
    if not shutil.which("nix"):
        warn(f"packages: {' '.join(z.settings['packages'])} not built: packages need Nix (nix on PATH)")
        return []
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
    for sub, days in common.TMP_DAYS.items():
        d = zdir / sub
        d.mkdir(mode=0o700, exist_ok=True)
        os.chmod(d, 0o700)
        n = clean_old(d, days)
        if n:
            print(f"[zone] tmp: removed {n} file(s) unused for {days} days from {d}", flush=True)


def host_mounts(zone: str) -> list[str]:
    """Names mounted under ~/.local/share/appzones/ZONE/mnt (host view)."""
    pre = str(common.STATE / zone / "mnt") + "/"
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
    return subprocess.run([common.SUDO, "-n", common.MOUNT_HELPER, *args], capture_output=True, text=True, stdin=subprocess.DEVNULL)  # NOSONAR: argv list, no shell; the helper re-validates every argument


def prepare_mnt(zone: str, zdir: Path) -> bool:
    """At zone start and stop: no runtime mount outlives a zone run. False when some are
    still mounted (the server then does not start the zone with them)."""
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
    return not host_mounts(zone)


def bwrap_argv(z: zonecfg.Zone, zdir: Path, zrt: Path, network: str, extra_path: list[str],
               host_wayland: bool = False) -> list[str]:
    """bwrap's command line for zone Z. HOST_WAYLAND: wayland = "host" and the compositor
    has no security context (the server tried it): the host's own socket."""
    s = z.settings
    home = Path(os.path.expanduser(s["home"])) if s["home"] else zdir / "home"
    if s["home"]:
        # checked where it lands (also before it exists) by the validator;
        # pinned and checked again on the descriptor by netsetup
        err = zonecfg.bind_error(s["home"])
        if err and not err.endswith(": does not exist"):
            die(f"home {err}")
    home.mkdir(parents=True, exist_ok=True)
    a = [tool("bwrap"), "--unshare-user", "--unshare-ipc", "--unshare-pid", "--unshare-uts", "--unshare-cgroup",
         "--uid", str(common.UID), "--gid", str(common.GID), "--hostname", f"{z.name}-zone",
         "--die-with-parent", "--new-session", "--cap-drop", "ALL", "--clearenv"]
    if network == "none":
        a += ["--unshare-net"]
    if not s["userns"]:
        a += ["--disable-userns", "--assert-userns-disabled"]
    a += ["--proc", "/proc", "--dev", "/dev"]
    if s["tmp"] == "disk":
        # kept across restarts, reachable from the host (prepare_tmp: 0700, cleaned at start)
        a += ["--bind", str(zdir / "tmp"), "/tmp", "--bind", str(zdir / "var-tmp"), "/var/tmp"]  # NOSONAR: the zone's own /tmp, bound from its private directory
    else:
        a += ["--tmpfs", "/tmp"]  # NOSONAR: a fresh tmpfs inside the zone
    layout = zonecfg.host_layout()
    a += ["--tmpfs", "/run", "--perms", "0700", "--tmpfs", common.ZRT, "--dir", "/run/zone"] + system_binds(layout)
    if layout == "fhs" and extra_path:
        # packages built with the host's Nix: their store paths, read-only
        a += ["--ro-bind", "/nix/store", "/nix/store"]
    # runtime mounts (appzones mount): read-only here, so the zone cannot plant
    # anything in it; a host mount below .../mnt propagates in (bwrap makes
    # its mounts slaves of the host's, and /home is shared on the host)
    a += ["--ro-bind", str(zdir / "mnt"), "/mnt/host"]
    if s["nix_ld"]:
        shim, lib = zonecfg.host_nix_ld() or die("nix_ld = true, but this host has no nix-ld")
        a += ["--dir", "/lib64", "--symlink", shim, "/lib64/ld-linux-x86-64.so.2"]
    for link in ("/run/current-system", f"/etc/profiles/per-user/{common.USER}", "/run/opengl-driver",
                 "/run/opengl-driver-32"):
        if os.path.lexists(link):
            a += ["--symlink", os.path.realpath(link), link]
    if layout == "nixos":
        a += ["--symlink", "/run/current-system/sw/bin", "/bin"]
        a += ["--symlink", "/run/current-system/sw/bin/env", "/usr/bin/env"]
    for e in ETC + (FHS_ETC if layout == "fhs" else []):
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
    a += ([server.PIN, "rw", s["home"], str(common.HOME)] if s["home"] else ["--bind", str(home), str(common.HOME)])
    if s["theme"]:
        for src, dst in copy_theme(zdir):
            a += ["--ro-bind", str(src), dst]
    for b in s["binds"]:
        # never a path here: netsetup opens it, checks what it opened, and
        # bwrap mounts that descriptor (no window to swap in a symlink)
        a += [server.PIN, b.get("mode", "rw"), b["path"], str(zonecfg.expand(b["path"]))]
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
    # read-only: nothing in the zone may put a file here (the agent's socket is
    # an inherited fd, see netsetup; links.sock is the host's and stays reachable)
    a += ["--ro-bind", str(zrt / "ctl"), "/run/zone"]
    if (zrt / "ctl/bin").is_dir():
        # the xdg-open shim: first on PATH, so never writable from inside
        a += ["--ro-bind", str(zrt / "ctl/bin"), "/run/zone/bin"]
    sysenv = system_env(layout)
    env = {
        "HOME": str(common.HOME), "USER": common.USER, "LOGNAME": common.USER,
        "PATH": ":".join(extra_path + ["/run/zone/bin", sysenv["PATH"]]),
        "LANG": os.environ.get("LANG", "C.UTF-8"), "TZ": os.environ.get("TZ", ":/etc/localtime"),
        "XDG_RUNTIME_DIR": common.ZRT, "ZONE": z.name, "TERM": "xterm-256color",
        **({"ZONE_DNS": common.DNS_ADDR} if network == "internet" else {}),
        "XDG_DATA_DIRS": sysenv["XDG_DATA_DIRS"],
        "XDG_CONFIG_DIRS": "/etc/xdg", "XDG_SESSION_TYPE": "wayland",
        "NIXOS_OZONE_WL": "1", "MOZ_ENABLE_WAYLAND": "1", "QT_QPA_PLATFORM": "wayland;xcb",
        "SDL_VIDEODRIVER": "wayland", "ELECTRON_OZONE_PLATFORM_HINT": "wayland",
        "TZDIR": sysenv["TZDIR"],
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
    host_wl = common.host_wayland_socket() if host_wayland and s["wayland"] == "host" else None
    if host_wl:
        # wayland = "host": the compositor's own socket, no security context (warned by check)
        a += ["--bind", host_wl, f"{common.ZRT}/wayland-0"]
        env["WAYLAND_DISPLAY"] = "wayland-0"
    elif s["wayland"] != "none" and (zrt / "wayland-0").exists():
        a += ["--bind", str(zrt / "wayland-0"), f"{common.ZRT}/wayland-0"]
        env["WAYLAND_DISPLAY"] = "wayland-0"
    if (zrt / "bus").exists():
        a += ["--bind", str(zrt / "bus"), f"{common.ZRT}/bus"]
        env["DBUS_SESSION_BUS_ADDRESS"] = f"unix:path={common.ZRT}/bus"
    # for zone-init: the zone's own bus (bus = "zone") and the nested bwrap
    # that hides every bus (bus = "none"); store paths, visible inside
    daemon = common.find_tool("dbus-daemon")  # only for bus = "zone"; zone-init says when it is missing
    if daemon:
        env["ZONE_DBUS_DAEMON"] = os.path.realpath(daemon)
    env["ZONE_BWRAP"] = os.path.realpath(tool("bwrap"))
    if "audio" in devices or "mic" in devices:
        # "mic" is an alias of "audio" for now (the whole PipeWire graph);
        # a real mic toggle needs a restricted per-zone socket.
        if (common.HOST_RT / "pipewire-0").exists():
            a += ["--bind", str(common.HOST_RT / "pipewire-0"), f"{common.ZRT}/pipewire-0"]
        if (common.HOST_RT / "pulse/native").exists():
            a += ["--bind", str(common.HOST_RT / "pulse/native"), f"{common.ZRT}/pulse/native"]
            env["PULSE_SERVER"] = f"unix:{common.ZRT}/pulse/native"
    doc = common.HOST_RT / "doc/by-app" / f"dev.mirql.zone.{z.name}"
    try:
        if doc.is_dir():
            a += ["--bind", str(doc), f"{common.ZRT}/doc"]
    except OSError:
        pass
    a += ["--ro-bind", str(zrt / "flatpak-info"), "/.flatpak-info"]
    a += ["--info-fd", "3", "--lock-file", "/.flatpak-info"]
    env.update(s["env"])
    for k, v in env.items():
        a += ["--setenv", k, v]
    return a + zone_init_argv()


# where the Rust helper appears in a zone: bwrap's /run is a fresh tmpfs, so the bind
# never lands on anything of the zone's
ZONE_HELPER = "/run/appzones-helper"


def zone_init_argv() -> list[str]:
    """The end of bwrap's command line: zone-init. With the Rust helper, the binary bound
    read-only into the zone and run from there (it needs nothing else of appzones in the
    zone); else the host's Python running zone_init.py, which the zone sees in /usr or
    the Nix store."""
    if common.HELPER == "rust" and "zone-init" in common.RUST_HELPERS:
        helper = common.zone_helper()
        if not helper:
            die("appzones was installed with the Rust helper, but there is no appzones-helper that implements "
                "zone-init (`appzones-helper --commands`); reinstall it")
        return ["--ro-bind", os.path.realpath(helper), ZONE_HELPER, "--", ZONE_HELPER, "zone-init"]
    return ["--", os.path.realpath(sys.executable), str(common.HERE / "zone_init.py")]
