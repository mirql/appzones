"""zonecfg: zones.toml schema v1 and its validator.

Everything that is a choice is a per-zone TOML setting (dns, links,
keyring, netbird, devices, clipboard, ...). Used by `appzones` (every command
validates first) and on its own:

    python3 zonecfg.py check [FILE]        validate, exit 1 with every error
    python3 zonecfg.py which ZONE [FILE]   every key in effect and its origin

Unknown keys are errors. Binds are checked on their realpath against the
never-list: a bind that is, contains or is inside a denied path is refused.
A host adds its own denied paths in /etc/zones/denied-paths (one per line;
the NixOS module's `deniedPaths`).
"""
from __future__ import annotations

import difflib
import fnmatch
import os
import re
import json
import stat
import subprocess
import sys
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

HOME = Path(os.environ.get("ZONES_HOME_FOR_TESTS") or Path.home())
NAME_RE = re.compile(r"^[a-z][a-z0-9-]{1,20}$")
# Directories of ~/.local/share/zones that are not zones (launcher entries).
RESERVED = {"launcher", "share", "bin", "lib", "etc"}
COLOUR_RE = re.compile(r"^#[0-9a-fA-F]{6}$")

# key -> (default, check(value) -> error or None, help)
def _enum(*vals):
    return lambda v: None if v in vals else f"must be one of {', '.join(map(repr, vals))}"

def _bool(v):
    return None if isinstance(v, bool) else "must be true or false"

def _strlist(v):
    return None if isinstance(v, list) and all(isinstance(x, str) for x in v) else "must be a list of strings"

DEVICES = ("gpu", "audio", "mic", "camera", "screencast", "usb")

def _devices(v):
    e = _strlist(v)
    if e:
        return e
    bad = [d for d in v if d not in DEVICES]
    return f"unknown device(s) {bad}; known: {', '.join(DEVICES)}" if bad else None

# usb = ["abcd", "abcd:1234"]: USB vendor, or vendor:product, lowercase hex
# (as lsusb prints them). Only matching devices enter a zone, never all of
# /dev/bus/usb or /dev/hidraw* (a YubiKey sits next to them).
USB_ID_RE = re.compile(r"^[0-9a-f]{4}(:[0-9a-f]{4})?$")

def _usb(v):
    e = _strlist(v)
    if e:
        return e
    bad = [x for x in v if not USB_ID_RE.match(x)]
    return f"entries must be \"vvvv\" or \"vvvv:pppp\" (lowercase hex, as lsusb prints them): {bad}" if bad else None

def _links(v):
    if v in ("portal", "zone"):
        return None
    if isinstance(v, str) and v.startswith("zone:") and NAME_RE.match(v[5:]):
        return None
    return "must be 'portal' (host default browser), 'zone' (this zone's browser) or 'zone:NAME'"

def _str(v):
    return None if isinstance(v, str) else "must be a string"

def _env(v):
    if not isinstance(v, dict) or not all(isinstance(k, str) and isinstance(x, str) for k, x in v.items()):
        return "must be a table of strings"
    bad = [k for k in v if env_denied(k)]
    return f"never passed into a zone: {bad}" if bad else None

def _binds(v):
    if not isinstance(v, list):
        return "must be a list of { path, mode }"
    for b in v:
        if not isinstance(b, dict) or set(b) - {"path", "mode"} or "path" not in b:
            return f"bind {b!r}: keys are path and mode"
        if not isinstance(b["path"], str) or not b["path"]:
            return f"bind {b!r}: path must be a non-empty string"
        if b.get("mode", "rw") not in ("rw", "ro"):
            return f"bind {b['path']}: mode must be 'rw' or 'ro'"
    return None

def _dbus(v):
    if not isinstance(v, dict) or set(v) - {"talk", "own"}:
        return "must be { talk = [...], own = [...] }"
    for k in ("talk", "own"):
        e = _strlist(v.get(k, []))
        if e:
            return f"dbus.{k} {e}"
        bad = [n for n in v.get(k, []) if dbus_denied(n) or k == "own" and own_denied(n)]
        if bad:
            return f"dbus.{k}: never granted: {bad}"
        if k == "own":
            wide = [n for n in v.get("own", []) if n.endswith(".*") and n.count(".") < 3]
            if wide:
                return f"dbus.own: a wildcard needs at least three labels before .* (org.example.app.*): {wide}"
        # OWN implies TALK, and TALK lets the zone call StartServiceByName and
        # auto-start a name: a name a host .service file provides would start
        # that host program outside the zone, for the zone to drive (Thunar's
        # org.xfce.FileManager.Execute runs a command on the host).
        if v.get(k) and activatable_names() is None:
            return (f"dbus.{k}: cannot ask the session bus which names it can start (ListActivatableNames), "
                    f"so cannot check whether a call would start a host program; nothing extra is granted")
        act = [n for n in v.get(k, []) if host_activatable(n)]
        if act:
            return (f"dbus.{k}: {act} can be started on the host bus by a call from the zone (a host .service "
                    f"file provides it); a single-instance app that needs it: bus = \"zone\" in its [apps] entry")
    return None

BUSES = ("host", "zone", "none")

KEYS: dict[str, tuple[object, object, str]] = {
    "network": ("internet", _enum("none", "internet", "host"), "none | internet | host"),
    "netbird": (False, _bool, "with network=internet: also reach the NetBird mesh (the network of the host's "
                               "NetBird interface, never the host itself)"),
    "dns": ("host", _enum("host", "public"), "host (the host's resolver) | public (1.1.1.1)"),
    "links": ("portal", _links, "where links open: portal (host browser) | zone | zone:NAME"),
    "keyring": ("basic", _enum("basic", "zone"), "basic (fixed-key store) | zone (own keyring daemon, not built yet)"),
    "devices": (["gpu", "audio"], _devices, "any of " + " ".join(DEVICES)),
    "camera": ("portal", _enum("portal", "device"), "portal | device"),
    "usb": ([], _usb, "with \"usb\" in devices: USB devices by \"vvvv\" or \"vvvv:pppp\" (their /dev/bus/usb node, "
                      "hidraw nodes and sysfs; bound at zone start)"),
    "x11": ("none", _enum("none", "own"), "none | own (xwayland-satellite in the zone)"),
    "clipboard": ("both", _enum("both"), "both (the only value for now)"),
    "autostart": (False, _bool, "start the zone at login"),
    "autostart_apps": ([], _strlist, "app ids started with the zone"),
    "userns": (True, _bool, "nested user namespaces (browser sandboxes)"),
    "machine_id": ("zone", _enum("zone", "host"), "zone (own) | host"),
    "theme": (True, _bool, "copy gtk/qt/font/cursor config in at start"),
    "packages": ([], _strlist, "extra nixpkgs attributes on PATH"),
    "env": ({}, _env, "extra environment"),
    "binds": ([], _binds, "{ path, mode = rw|ro }, same path inside"),
    "dbus": ({"talk": [], "own": []}, _dbus, "extra bus names on the host bus proxy (never-list refused)"),
    "bus": ("host", _enum(*BUSES), "bus for commands not in [apps]: host (filtered proxy, portals) | zone (the "
                                   "zone's own bus) | none"),
    "nix_ld": (False, _bool, "generic Linux binaries run (/lib64/ld-linux-x86-64.so.2 is the host's nix-ld shim)"),
    "tmp": ("ram", _enum("ram", "disk"), "ram (fresh tmpfs each start) | disk (~/.local/share/zones/<zone>/tmp, "
                                         "kept; files unused for 14 days removed at start)"),
    "home": ("", _str, "zone home; default ~/.local/share/zones/<zone>/home"),
    "colour": ("", _str, "#rrggbb, required, unique"),
    "force_host": (False, _bool, "allow network=host in a zone named untrusted"),
}
APP_KEYS = {"command", "desktop", "zones", "args", "bus"}

# zones.toml can change before the installed `appzones` tool does (a dotfiles
# repo moves faster than a system rebuild). A file names the release it
# needs (`requires = N`); an older tool then says "update appzones" instead
# of "unknown key". Bump RELEASE when a key or value is added, and record it here.
RELEASE = 4
SINCE = {"requires": 2, "bus": 2, "apps.bus": 2, "bus = \"zone\"": 2, "bus = \"none\"": 2,
         "nix_ld": 3, "tmp": 3,  # release 3 also added zone mount/umount/mounts (no key)
         "usb": 4}  # release 4: the "usb" device and its usb key

# nix-ld on the host (programs.nix-ld.enable): the shim at the FHS loader
# path, and the directory with the real loader (ld.so) and its libraries.
NIX_LD_SHIM = os.environ.get("ZONES_NIX_LD_SHIM") or "/lib64/ld-linux-x86-64.so.2"
NIX_LD_LIB = os.environ.get("ZONES_NIX_LD_LIB") or "/run/current-system/sw/share/nix-ld/lib"


def host_nix_ld() -> tuple[str, str] | None:
    """(shim store path, lib dir) when the host runs nix-ld, else None."""
    shim = os.path.realpath(NIX_LD_SHIM)
    if not os.path.exists(shim) or "nix-ld" not in shim or not os.path.exists(os.path.join(NIX_LD_LIB, "ld.so")):
        return None
    return shim, NIX_LD_LIB


def unknown_key(where: str, k: str, known) -> str:
    near = difflib.get_close_matches(k, list(known), n=1, cutoff=0.75)
    return f"{where}unknown key {k!r}" + (f" (did you mean {near[0]!r}?)" if near else "")

# ── never-lists ────────────────────────────────────────────────────────────
def denied_paths() -> list[Path]:
    h = HOME
    rel = [".ssh", ".gnupg", ".config/sops", ".password-store", ".local/share/keyrings",
           ".local/share/kwalletd", ".config/opencode",
           # anything the host runs or reads as code or config at login: a
           # writable bind of one of these is a way out of the zone
           ".config/zones", ".local/share/zones", ".config/hypr", ".config/systemd", ".config/autostart",
           ".config/environment.d", ".config/uwsm", ".local/share/applications", ".local/bin", ".bin",
           ".bashrc", ".bash_profile", ".profile", ".zshrc", ".zshenv", ".zprofile", ".config/fish",
           ".config/nix", ".config/git", ".gitconfig", ".config/kwalletrc"]
    out = [Path(p) for p in ("/run/secrets", "/run/user", "/var/lib/sops-nix", "/etc/ssh", "/nix/var")]
    out += [h / r for r in rel]
    out += host_denied_paths()
    # globbed ones: ~/.local/share/opencode*, ~/.claude*
    for parent, pat in ((h / ".local/share", "opencode*"), (h, ".claude*")):
        if parent.is_dir():
            out += [p for p in parent.iterdir() if fnmatch.fnmatch(p.name, pat)]
    return out

# The host's own additions (code checkouts, secret stores, mounts), one path
# per line, "#" comments, "~/" for the home. The mount helper (root) clears
# every ZONES_* variable, so it always reads the real file.
HOST_DENIED_FILE = "/etc/zones/denied-paths"


class DeniedPathsError(ValueError):
    """The host's denied-paths file exists but cannot be used: refuse, never ignore it."""


def host_denied_paths() -> list[Path]:
    """The host's additions. A missing default file means none; an unreadable
    file, a missing explicit override or a bad line is an error (fail closed)."""
    override = os.environ.get("ZONES_DENIED_PATHS")
    path = Path(override or HOST_DENIED_FILE)
    try:
        lines = path.read_text().splitlines()
    except FileNotFoundError:
        if override:
            raise DeniedPathsError(f"{path} (ZONES_DENIED_PATHS) does not exist")
        return []
    except (OSError, UnicodeDecodeError) as e:
        raise DeniedPathsError(f"{path}: cannot read the host's denied paths: {e}")
    out = []
    for n, line in enumerate(lines, 1):
        p = line.split("#", 1)[0].strip()
        if not p:
            continue
        if p == "~" or p.startswith("~/"):
            out.append(HOME / p[2:])
        elif p.startswith("/"):
            out.append(Path(p))
        else:
            raise DeniedPathsError(f"{path}:{n}: {p!r} must be absolute or start with ~/")
    return out

DBUS_NEVER = ["org.freedesktop.systemd1*", "org.freedesktop.secrets*", "org.kde.kwalletd*",
              "org.freedesktop.Flatpak*", "org.gnome.keyring*", "org.freedesktop.impl.portal.*",
              "ca.desrt.dconf*", "org.kde.kdeconnect*", "org.freedesktop.login1*",
              "org.freedesktop.portal.Flatpak*", "org.kde.klauncher*", "org.kde.KWin*",
              # host daemons that act on host files with host rights: gvfs mounts
              # with the host's credentials, tumbler reads any host path, xfconfd
              # writes host settings, a host file manager runs commands
              "org.gtk.vfs*", "org.freedesktop.thumbnails*", "org.xfce.Xfconf*", "org.freedesktop.FileManager1*",
              "org.xfce.FileManager*", "org.xfce.Thunar*", "org.xfce.SessionManager*",
              "org.gnome.SessionManager*", "org.gnome.Shell*", "org.kde.ksmserver*", "org.kde.plasmashell*"]
# Names a zone never owns on the host bus, on top of DBUS_NEVER: owning a
# name there makes the zone the answer to every host app (and every other
# zone) that calls it, and owning implies talking to whoever owns it first.
# org.mozilla.<app>.*: the host's Firefox and every zone's share it; a zone
# gets its own remote name automatically (zone_init.py MOZ_APPS).
OWN_NEVER = ["org.freedesktop.*", "org.kde.*", "org.gnome.*", "org.xfce.*", "org.a11y.*", "com.canonical.*",
             "org.mozilla.firefox*", "org.mozilla.thunderbird*", "org.mozilla.librewolf*",
             # every zone's own names (its portal app id, its Firefox remote)
             "dev.mirql.zone*", "org.mozilla.*_zone_*"]

def dbus_denied(name: str) -> bool:
    if name in ("*", "") or name.endswith(".*") and name.count(".") < 2:
        return True  # talk=* or org.* is far too wide
    return any(fnmatch.fnmatchcase(name, p) or fnmatch.fnmatchcase(p.rstrip("*"), name.rstrip("*") + "*")
               for p in DBUS_NEVER)

def own_denied(name: str) -> bool:
    return any(fnmatch.fnmatchcase(name, p) or name + ".x" == p.replace("*", "x") for p in OWN_NEVER)

def bus_activatable_names() -> set[str] | None:
    """Every name the running session bus can start (ListActivatableNames):
    the bus's own answer, whatever its configuration includes. None when the
    bus cannot be asked (no session bus, no busctl)."""
    try:
        r = subprocess.run(["busctl", "--user", "--json=short", "call", "org.freedesktop.DBus",
                            "/org/freedesktop/DBus", "org.freedesktop.DBus", "ListActivatableNames"],
                           capture_output=True, text=True, timeout=5)
        if r.returncode != 0:
            return None
        data = json.loads(r.stdout)["data"][0]
        return {n for n in data if isinstance(n, str)}
    except (OSError, ValueError, KeyError, IndexError, TypeError, subprocess.TimeoutExpired):
        return None


_activatable: set[str] | None = None
_activatable_known = False

def activatable_names() -> set[str] | None:
    """Every name a host .service file provides; None if that cannot be known
    (then extra talk/own grants are refused: fail closed). Tests (and hosts
    without a session bus) name the service directories in
    $ZONES_DBUS_SERVICE_DIRS instead."""
    global _activatable, _activatable_known
    if _activatable_known:
        return _activatable
    _activatable_known = True
    if "ZONES_DBUS_SERVICE_DIRS" not in os.environ:
        _activatable = bus_activatable_names()
        return _activatable
    names: set[str] = set()
    for d in [Path(x) for x in os.environ["ZONES_DBUS_SERVICE_DIRS"].split(":") if x]:
        try:  # scandir, not glob: glob hides a missing or unreadable directory
            with os.scandir(d) as it:
                files = [Path(e.path) for e in it if e.name.endswith(".service")]
        except OSError:
            _activatable = None
            return None
        for f in files:
            try:
                for line in f.read_text(errors="replace").splitlines():
                    if line.startswith("Name="):
                        names.add(line[5:].strip())
            except OSError:
                _activatable = None
                return None
    _activatable = names
    return names

def host_activatable(name: str) -> bool:
    """True if NAME (or, for "a.b.*", any name under a.b) has a host .service file
    (also True when that cannot be known: fail closed)."""
    names = activatable_names()
    if names is None:
        return True
    if name.endswith(".*"):
        pre = name[:-2]
        return any(n == pre or n.startswith(pre + ".") for n in names)
    return name in names

ENV_NEVER = ["SSH_AUTH_SOCK", "GPG_*", "HYPRLAND_INSTANCE_SIGNATURE", "OPENCODE*",
             "SOPS_*", "*TOKEN*", "*KEY*", "*SECRET*", "*PASSWORD*", "DBUS_SESSION_BUS_ADDRESS",
             "WAYLAND_DISPLAY", "XDG_RUNTIME_DIR", "LD_PRELOAD", "LD_LIBRARY_PATH", "DISPLAY", "XAUTHORITY"]

def env_denied(name: str) -> bool:
    return any(fnmatch.fnmatchcase(name.upper(), p) for p in ENV_NEVER)

def expand(p: str) -> Path:
    return Path(os.path.expanduser(p.replace("~", str(HOME), 1) if p.startswith("~") else p))

def policy_error(real: Path, raw: str) -> str | None:
    """Why the already-resolved path REAL may never enter a zone, or None. The
    whole policy for binds and the custom home: never a system root whole,
    never on the never-list (is, contains or is inside), never from inside
    the host's system trees. Callers resolve first (bind_error), or pass the
    path of a descriptor they already hold (the supervisor pins binds)."""
    via = f" (resolves to {real})" if str(real) != str(expand(raw)) else ""
    for forbidden in (Path("/"), Path("/home"), Path(os.path.realpath(HOME)), Path("/nix"), Path("/etc"),
                      Path("/proc"), Path("/sys"), Path("/dev"), Path("/run"), Path("/tmp"), Path("/var")):
        if real == forbidden:
            return f"{raw}{via}: binding {forbidden} whole is never allowed"
    try:
        denied = denied_paths()
    except DeniedPathsError as e:
        return f"{raw}: {e}"
    for d in denied:
        dr = Path(os.path.realpath(d))
        if real == dr or dr.is_relative_to(real) or real.is_relative_to(dr):
            how = "is" if real == dr else ("contains" if dr.is_relative_to(real) else "is inside")
            return f"{raw}{via}: {how} {d}, which never enters a zone"
    # the host's system trees (sockets in /run and /tmp, devices, other users'
    # and root's files); the home itself is exempt (a /home that lives in /var)
    if not real.is_relative_to(Path(os.path.realpath(HOME))):
        for d in MOUNT_NEVER_INSIDE:
            if real.is_relative_to(d):
                return f"{raw}{via}: is inside {d}, which never enters a zone"
    return None


def bind_error(raw: str) -> str | None:
    """Why this bind (or custom home) is refused, or None. Compares resolved
    paths, never strings: symlinks anywhere on the path are resolved, also
    when the last parts do not exist yet, so a home that is created at start
    is checked where it would land."""
    p = expand(raw)
    if not p.is_absolute():
        return f"{raw}: must be absolute or start with ~"
    real = Path(os.path.realpath(p))
    e = policy_error(real, raw)
    if e:
        return e
    if not p.exists():
        return f"{raw}: does not exist"
    return kind_error(os.stat(p).st_mode, raw)


# Theme files are copied (dereferenced) into every zone: never from here, nor
# from the never-list (the store and /etc are fine: fonts and icons live there).
THEME_NEVER_INSIDE = [Path(p) for p in ("/proc", "/sys", "/dev", "/run", "/tmp", "/var", "/root", "/boot")]


def theme_source_error(real: Path) -> str | None:
    """Why the resolved path REAL may not be copied into a zone as a theme file, or None."""
    for d in THEME_NEVER_INSIDE if not real.is_relative_to(Path(os.path.realpath(HOME))) else []:
        if real.is_relative_to(d):
            return f"{real} is inside {d}"
    try:
        denied = denied_paths()
    except DeniedPathsError as e:
        return str(e)
    for d in denied:
        dr = Path(os.path.realpath(d))
        if real == dr or real.is_relative_to(dr):
            return f"{real} is inside {d}, which never enters a zone"
    return None


def fd_path(fd: int) -> Path | None:
    """The path of the object an open descriptor refers to, or None when it has
    none a policy can be checked on: unlinked (" (deleted)": the name was
    replaced, the old object is still readable), or not an absolute path."""
    try:
        target = os.readlink(f"/proc/self/fd/{fd}")
    except OSError:
        return None
    if target.endswith(" (deleted)") or not target.startswith("/"):
        return None
    return Path(target)


def kind_error(mode: int, raw: str) -> str | None:
    """Only directories and regular files are bound: never a socket (a read-only
    bind of a socket still connects), a FIFO or a device."""
    if stat.S_ISDIR(mode) or stat.S_ISREG(mode):
        return None
    what = ("a socket" if stat.S_ISSOCK(mode) else "a FIFO" if stat.S_ISFIFO(mode) else
            "a device" if stat.S_ISCHR(mode) or stat.S_ISBLK(mode) else "not a file or directory")
    return f"{raw}: is {what}; only directories and files are bound"


# ── runtime mounts (appzones mount) ────────────────────────────────────────────
MOUNT_NAME_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$")
# On top of the binds' never-list: a runtime mount never comes from inside
# these. The host's system trees, other users' and root's files, and /tmp
# (its X11 and ICE sockets are a way to the host session).
MOUNT_NEVER_INSIDE = [Path(p) for p in ("/proc", "/sys", "/dev", "/run", "/etc", "/nix", "/boot", "/root",
                                        "/var", "/tmp")]


def mount_source_error(raw: str) -> str | None:
    """Why a folder may not be mounted into a zone at run time, or None.
    The binds' never-list (is, contains or is inside), compared on the
    resolved path, plus MOUNT_NEVER_INSIDE. The root helper runs this on the
    path of the directory it has already opened, as the user."""
    p = expand(raw)
    if not p.is_absolute():
        return f"{raw}: must be absolute or start with ~"
    # refusals first, lexically for a path that does not exist (as bind_error)
    real = Path(os.path.realpath(p))
    for d in MOUNT_NEVER_INSIDE if not real.is_relative_to(Path(os.path.realpath(HOME))) else []:
        if real == d or real.is_relative_to(d):
            via = f" (resolves to {real})" if str(real) != str(p) else ""
            return f"{raw}{via}: is inside {d}, which is never mounted into a zone"
    err = bind_error(raw)
    if err:
        return err
    return None if p.is_dir() else f"{raw}: not a directory"

# ── loading ────────────────────────────────────────────────────────────────
@dataclass
class Zone:
    name: str
    settings: dict
    origin: dict = field(default_factory=dict)  # key -> "builtin" | "defaults" | "zone"

@dataclass
class Config:
    zones: dict[str, Zone]
    apps: dict[str, dict]
    errors: list[str]
    warnings: list[str] = field(default_factory=list)

def load(path: str | Path) -> Config:
    try:
        text = Path(path).read_text()
    except (OSError, UnicodeDecodeError) as e:
        return Config({}, {}, [f"{path}: {e}"])
    return load_text(text, path)


def load_text(text: str, path: str | Path = "zones.toml") -> Config:
    """Validate TEXT as a zones.toml (PATH only names it in messages)."""
    errors: list[str] = []
    try:
        raw = tomllib.loads(text)
    except tomllib.TOMLDecodeError as e:
        return Config({}, {}, [f"{path}: {e}"])
    for k in raw:
        if k not in ("version", "requires", "defaults", "zones", "apps"):
            errors.append(unknown_key("top level: ", k, ("version", "requires", "defaults", "zones", "apps")))
    shapes = [f"{k} must be a table" for k in ("defaults", "zones", "apps") if not isinstance(raw.get(k, {}), dict)]
    if shapes:
        return Config({}, {}, errors + shapes)
    if raw.get("version") != 1:
        errors.append("version must be 1")
    defaults = raw.get("defaults", {})
    for k in defaults:
        if k not in KEYS:
            errors.append(unknown_key("[defaults] ", k, KEYS))
    zones: dict[str, Zone] = {}
    for zname, zraw in raw.get("zones", {}).items():
        if not NAME_RE.match(zname):
            errors.append(f"zone name {zname!r} must match {NAME_RE.pattern}")
        elif zname in RESERVED:
            errors.append(f"zone name {zname!r} is reserved ({', '.join(sorted(RESERVED))})")
        if not isinstance(zraw, dict):
            errors.append(f"[zones.{zname}] must be a table")
            continue
        s, o = {}, {}
        for k, (dflt, _, _) in KEYS.items():
            if k in zraw:
                s[k], o[k] = zraw[k], "zone"
            elif k in defaults:
                s[k], o[k] = defaults[k], "defaults"
            else:
                s[k], o[k] = dflt, "builtin"
        for k in zraw:
            if k not in KEYS:
                errors.append(unknown_key(f"[zones.{zname}] ", k, KEYS))
        zones[zname] = Zone(zname, s, o)
    apps = raw.get("apps", {})
    errors += validate(zones, apps)
    req = raw.get("requires", 1)
    if not isinstance(req, int) or isinstance(req, bool) or req < 1:
        errors.append("requires must be a release number (an integer, 1 or more)")
    elif req > RELEASE:
        # every other error is most likely a setting this tool does not know yet
        return Config(zones, apps, [
            f"{path} needs zones release {req}, and this `appzones` is release {RELEASE}: update appzones "
            f"(and rebuild) to get the newer one. Until then, what this zone does not know:"]
            + [f"  {e}" for e in errors], [])
    used = [n for n, since in SINCE.items() if isinstance(req, int) and since > req and uses(raw, n)]
    if used and not errors:
        errors.append(f"requires = {req}, but the file uses {', '.join(used)} (zones release "
                      f"{max(SINCE[n] for n in used)}): set requires = {max(SINCE[n] for n in used)}, so an "
                      f"older `appzones` says to update instead of failing on an unknown key")
    warnings = [f"[zones.{z.name}] userns = false: Chromium/Electron refuse to start, Firefox runs with a weaker "
                f"sandbox without saying so" for z in zones.values() if z.settings["userns"] is False]
    for z in zones.values():
        s = z.settings
        if s["keyring"] == "zone":
            warnings.append(f"[zones.{z.name}] keyring = \"zone\" is not built yet; the zone runs with \"basic\" for now")
        if s["x11"] == "own":
            warnings.append(f"[zones.{z.name}] x11 = \"own\" is not built yet; the zone has no X11 for now")
        if s["network"] == "host":
            warnings.append(f"[zones.{z.name}] network = \"host\": the zone reaches everything the host reaches, "
                            f"localhost and the tailnet included")
        if s["machine_id"] == "host":
            warnings.append(f"[zones.{z.name}] machine_id = \"host\": apps can link this zone to the host and to "
                            f"other zones by it")
    # The host bus is one bus: a name two zones may own is owned by whichever
    # starts first, and the other zone's app talks to it (OWN implies TALK).
    grants: list[tuple[str, str]] = []
    for z in zones.values():
        own = z.settings["dbus"].get("own", []) if isinstance(z.settings["dbus"], dict) else []
        grants += [(n, z.name) for n in (own if isinstance(own, list) else []) if isinstance(n, str)]
    clashes: dict[str, set[str]] = {}
    for i, (a, za) in enumerate(grants):
        for b, zb in grants[i + 1:]:
            if za != zb and names_overlap(a, b):
                key = a if a == b else f"{a} / {b}"
                clashes.setdefault(key, set()).update((za, zb))
    for n, zs in clashes.items():
        errors.append(f"dbus.own {n!r} in zones {', '.join(sorted(zs))}: on the shared host bus the first zone to "
                      f"start the app owns it, and the app in the other zone would hand its work (a URL, a "
                      f"file) to that one; a zone never hands anything to another zone")
    return Config(zones, apps, errors, warnings)


def names_overlap(a: str, b: str) -> bool:
    """Whether bus names (or "prefix.*" patterns) A and B can name the same name."""
    def pre(n: str) -> str | None:
        return n[:-2] if n.endswith(".*") else None
    pa, pb = pre(a), pre(b)
    if pa is None and pb is None:
        return a == b
    if pa is not None and pb is not None:
        return pa == pb or pa.startswith(pb + ".") or pb.startswith(pa + ".")
    p, n = (pa, b) if pa is not None else (pb, a)
    return n == p or n.startswith(p + ".")


def uses(raw: dict, what: str) -> bool:
    """Whether the raw TOML uses a setting from SINCE ("bus", "apps.bus", 'bus = "zone"')."""
    tables = [raw.get("defaults", {})] + [z for z in raw.get("zones", {}).values() if isinstance(z, dict)]
    apps = [a for a in raw.get("apps", {}).values() if isinstance(a, dict)]
    if what == "requires":
        return "requires" in raw
    if what.startswith("apps."):
        return any(what[5:] in a for a in apps)
    if " = " in what:
        k, v = what.split(" = ", 1)
        v = v.strip('"')
        return any(t.get(k) == v for t in tables + apps)
    return any(what in t for t in tables)


def resolve_app(cfg: Config, zone: str, app: str) -> list[str] | None:
    """The argv for app APP in ZONE (command + args[zone]), or None if APP is not declared."""
    a = cfg.apps.get(app)
    if a is None:
        return None
    if zone not in a.get("zones", []):
        raise ValueError(f"app {app!r} is not declared for zone {zone!r} (its zones: {', '.join(a.get('zones', []))})")
    return list(a["command"]) + list(a.get("args", {}).get(zone, []))

def validate(zones: dict[str, Zone], apps: dict[str, dict]) -> list[str]:
    errs: list[str] = []
    seen_colour: dict[str, str] = {}
    for z in zones.values():
        s, p = z.settings, f"[zones.{z.name}]"
        for k, (_, check, _) in KEYS.items():
            e = check(s[k])
            if e:
                errs.append(f"{p} {k} {e}")
        c = s["colour"]
        if not isinstance(c, str) or not COLOUR_RE.match(c):
            errs.append(f"{p} colour is required, as #rrggbb")
        elif c.lower() in seen_colour:
            errs.append(f"{p} colour {c} already used by zone {seen_colour[c.lower()]}")
        else:
            seen_colour[c.lower()] = z.name
        if s["netbird"] is True and s["network"] != "internet":
            errs.append(f"{p} netbird = true needs network = \"internet\"")
        if s["network"] == "host" and z.name == "untrusted" and not s["force_host"]:
            errs.append(f"{p} network = \"host\" in untrusted needs force_host = true")
        if isinstance(s["links"], str) and s["links"].startswith("zone:") and s["links"][5:] not in zones:
            errs.append(f"{p} links names unknown zone {s['links'][5:]!r}")
        if s["nix_ld"] is True and host_nix_ld() is None:
            errs.append(f"{p} nix_ld = true, but this host has no nix-ld ({NIX_LD_SHIM} is not its shim, or "
                        f"{NIX_LD_LIB}/ld.so is missing): programs.nix-ld.enable = true on the host first")
        if isinstance(s["devices"], list) and "camera" not in s["devices"] and z.origin["camera"] == "zone":
            errs.append(f"{p} camera is set but \"camera\" is not in devices")
        if isinstance(s["devices"], list) and isinstance(s["usb"], list):
            if s["usb"] and "usb" not in s["devices"] and z.origin["usb"] == "zone":
                errs.append(f"{p} usb is set but \"usb\" is not in devices")
            if "usb" in s["devices"] and not s["usb"]:
                errs.append(f"{p} \"usb\" is in devices but usb is empty: name the devices, "
                            f"usb = [\"vvvv\"] or [\"vvvv:pppp\"] (never all of them)")
        if isinstance(s["binds"], list):
            for b in s["binds"]:
                if isinstance(b, dict) and isinstance(b.get("path"), str):
                    e = bind_error(b["path"])
                    if e:
                        errs.append(f"{p} bind {e}")
        if isinstance(s["home"], str) and s["home"]:
            # checked where it would land, also before it exists (the zone creates it)
            e = bind_error(s["home"])
            if e and not e.endswith(": does not exist"):
                errs.append(f"{p} home {e}")
            elif not e and not expand(s["home"]).is_dir():
                errs.append(f"{p} home {s['home']}: not a directory")
        for a in s["autostart_apps"] if isinstance(s["autostart_apps"], list) else []:
            if not isinstance(a, str):
                continue  # reported by the type check
            if a not in apps:
                errs.append(f"{p} autostart_apps names undeclared app {a!r}")
            elif not isinstance(apps[a], dict) or not isinstance(apps[a].get("zones", []), list):
                continue  # reported with the app
            elif z.name not in apps[a].get("zones", []):
                errs.append(f"{p} autostart_apps: app {a!r} is not declared for this zone")
    for aname, a in apps.items():
        p = f"[apps.{aname}]"
        if not isinstance(a, dict) or not isinstance(a.get("zones", []), list) or not isinstance(a.get("args", {}), dict):
            errs.append(f"{p} must be a table with command = [...], zones = [...], args = {{ ZONE = [...] }}")
            continue
        if not NAME_RE.match(aname):
            errs.append(f"{p} app id must match {NAME_RE.pattern}")
        for k in a:
            if k not in APP_KEYS:
                errs.append(unknown_key(f"{p} ", k, APP_KEYS))
        if _strlist(a.get("command")) or not a.get("command"):
            errs.append(f"{p} command must be a non-empty list of strings")
        if _strlist(a.get("zones", [])):
            errs.append(f"{p} zones must be a list of zone names")
            continue
        for zn in a.get("zones", []):
            if zn not in zones:
                errs.append(f"{p} zones names unknown zone {zn!r}")
        if "desktop" in a and not isinstance(a["desktop"], str):
            errs.append(f"{p} desktop must be a string")
        for zn, args in a.get("args", {}).items():
            if zn not in a.get("zones", []):
                errs.append(f"{p} args for {zn!r}, which is not in its zones")
            if _strlist(args):
                errs.append(f"{p} args.{zn} must be a list of strings")
        if "bus" in a and a["bus"] not in BUSES:
            errs.append(f"{p} bus must be one of {', '.join(map(repr, BUSES))}")
        if a.get("bus") == "none" and isinstance(a.get("zones", []), list):
            for zn in a.get("zones", []):
                if zn in zones and zones[zn].settings["userns"] is False:
                    errs.append(f"{p} bus = \"none\" needs userns = true in zone {zn!r} (the app gets its own "
                                f"mount namespace without the bus socket)")
    for z in zones.values():
        if z.settings["bus"] == "none" and z.settings["userns"] is False:
            errs.append(f"[zones.{z.name}] bus = \"none\" needs userns = true")
    return errs


def app_bus(cfg: Config, zone: str, app: str | None) -> str:
    """The bus an app (or, with app None, an undeclared command) gets in ZONE."""
    a = cfg.apps.get(app) if app else None
    if a and a.get("bus"):
        return a["bus"]
    return cfg.zones[zone].settings["bus"]


def app_for_command(cfg: Config, argv0: str) -> str | None:
    """The declared app whose command is ARGV0 (by basename), for `appzones run Z -- thunar`."""
    base = os.path.basename(argv0)
    for aid, a in cfg.apps.items():
        cmd = a.get("command") or [""]
        if isinstance(cmd, list) and cmd and os.path.basename(str(cmd[0])) == base:
            return aid
    return None

def main(argv: list[str]) -> int:
    default = Path(os.environ.get("ZONES_CONFIG", HOME / ".config/zones/zones.toml"))
    if len(argv) >= 1 and argv[0] == "check":
        cfg = load(argv[1] if len(argv) > 1 else default)
        for e in cfg.errors:
            print(f"error: {e}", file=sys.stderr)
        for w in cfg.warnings:
            print(f"warning: {w}", file=sys.stderr)
        if not cfg.errors:
            print(f"ok: {len(cfg.zones)} zones, {len(cfg.apps)} apps (zones release {RELEASE})")
        return 1 if cfg.errors else 0
    if len(argv) >= 2 and argv[0] == "which":
        cfg = load(argv[2] if len(argv) > 2 else default)
        z = cfg.zones.get(argv[1])
        if not z:
            print(f"no zone {argv[1]!r}", file=sys.stderr)
            return 1
        for k in KEYS:
            print(f"{k:15} {z.settings[k]!r:45} ({z.origin[k]})")
        return 1 if cfg.errors else 0
    print(__doc__, file=sys.stderr)
    return 2

if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
