"""Paths, settings from the environment, messages, the config, systemctl."""
from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import zonecfg


# the directory of zone.py, zone_init.py, zonecfg.py and the mount helper
HERE = Path(__file__).resolve().parent.parent
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
DNS_ADDR = "127.0.0.53"  # NOSONAR: zone-init's DNS, on the zone's own loopback (as systemd-resolved's on hosts)
ZRT = f"/run/user/{UID}"  # the zone's own XDG_RUNTIME_DIR (a fresh tmpfs)
MOUNT_HELPER = zonecfg.env("MOUNT_HELPER") or str(HERE / "appzones-mount-helper")
# NixOS: the setuid wrapper; elsewhere sudo itself
SUDO = zonecfg.env("SUDO") or ("/run/wrappers/bin/sudo" if os.path.exists("/run/wrappers/bin/sudo")
                                else shutil.which("sudo") or "/usr/bin/sudo")
# netbird = true: the mesh is the network of this interface (NetBird's
# default name is wt0; the NixOS module's netbirdInterface sets it).
NETBIRD_IFACE = zonecfg.env("NETBIRD_IFACE") or "wt0"
TMP_DAYS = {"tmp": 14, "var-tmp": 30}  # tmp = "disk": unused this long, removed at zone start
PALETTE = ["#a6e3a1", "#89b4fa", "#f38ba8", "#f9e2af", "#cba6f7", "#94e2d5", "#fab387", "#f5c2e7",
           "#74c7ec", "#eba0ac", "#b4befe", "#89dceb"]


# where a host keeps root's tools (nft on Debian) that a user's PATH lacks
SBIN_DIRS = ("/usr/sbin", "/sbin")


def find_tool(name: str) -> str | None:
    """A helper program: $APPZONES_BIN_<NAME>, else the first one on PATH, else appzones'
    own (zone-wayland next to zone.py, as `make install` puts it), else in /usr/sbin or
    /sbin; None when there is none."""
    return (zonecfg.env("BIN_" + name.upper().replace("-", "_")) or shutil.which(name)
            or shutil.which(name, path=os.pathsep.join((str(HERE), *SBIN_DIRS))))


# Which implementation of the helpers runs (design/rust.md): "rust" when appzones was
# installed with the Rust helper (the package's or make install's wrapper sets it), so
# it reaches the server also under systemd. Unset: the Python and C ones.
HELPER = zonecfg.env("HELPER") or ""
RUST_HELPERS = ("zone-wayland", "zone-init", "open-link")  # done in Rust so far


_implements: dict[tuple[str, str], bool] = {}


def implements(helper: str, command: str) -> bool:
    """Whether the Rust helper at HELPER implements COMMAND (`appzones-helper --commands`)."""
    key = (helper, command)
    if key not in _implements:
        try:
            r = subprocess.run([helper, "--commands"], capture_output=True, text=True, timeout=5)
            _implements[key] = r.returncode == 0 and command in r.stdout.split()
        except (OSError, subprocess.TimeoutExpired):
            _implements[key] = False
    return _implements[key]


def rust_helper(command: str) -> str | None:
    """The Rust helper for COMMAND: $APPZONES_BIN_APPZONES_HELPER when set, else the one
    installed with this appzones (next to zone.py), else one on PATH; only one that
    implements COMMAND (an older helper elsewhere may not). None when there is none."""
    override = zonecfg.env("BIN_APPZONES_HELPER")
    if override:
        candidates = [override]
    else:
        own = HERE / "appzones-helper"
        candidates = [str(own)] if own.is_file() else []
        candidates.append(shutil.which("appzones-helper") or "")
    return next((p for p in candidates if p and implements(p, command)), None)


def helper_argv(name: str) -> list[str] | None:
    """The command that runs helper NAME: $APPZONES_BIN_<NAME> when set (an explicit
    choice wins), else `appzones-helper NAME` with the Rust helper, else the program NAME
    (find_tool); None when it is not installed."""
    override = zonecfg.env("BIN_" + name.upper().replace("-", "_"))
    if override:
        return [override]
    if HELPER == "rust" and name in RUST_HELPERS:
        p = rust_helper(name)
        return [p, name] if p else None
    p = find_tool(name)
    return [p] if p else None


def tool(name: str) -> str:
    """A helper program a zone cannot do without (find_tool, or exit)."""
    p = find_tool(name)
    if not p:
        die(f"{name} not found on PATH, {', '.join(SBIN_DIRS)}")
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


# read only when a zone starts, so changing them needs no restart; linger counts once set
SERVER_ONLY = ("supervisor", "start_timeout")


def settings_hash(z: zonecfg.Zone) -> str:
    """What a running zone was built from: `stale` compares it with the config."""
    s = {k: v for k, v in z.settings.items() if k not in SERVER_ONLY and not (k == "linger" and v == "")}
    return hashlib.sha256(json.dumps(s, sort_keys=True).encode()).hexdigest()


SUPERVISORS = ("auto", "systemd", "spawn")


def supervisor(z: zonecfg.Zone | None = None) -> str:
    """Who starts zone Z: its `supervisor` setting, unless that is auto; then
    $APPZONES_SUPERVISOR: auto (the default), systemd, or spawn (appzones starts zones
    itself; `none` means the same)."""
    if z is not None and z.settings.get("supervisor", "auto") != "auto":
        return z.settings["supervisor"]
    v = (zonecfg.env("SUPERVISOR") or "auto").strip().lower()
    v = "spawn" if v == "none" else v
    if v not in SUPERVISORS:
        die(f"APPZONES_SUPERVISOR={v!r}: use one of {', '.join(SUPERVISORS)}")
    return v


_unit: list[bool] = []


def unit_installed() -> bool:
    """The appzones@.service template is installed and the user's systemd answers."""
    if not _unit:
        try:
            r = subprocess.run(["systemctl", "--user", "show", "-p", "LoadState", "--value",
                                "appzones@probe.service"], capture_output=True, text=True, timeout=10)
            _unit.append(r.returncode == 0 and r.stdout.strip() == "loaded")
        except (OSError, subprocess.TimeoutExpired):
            _unit.append(False)
    return _unit[0]


def use_systemd(z: zonecfg.Zone | None = None) -> bool:
    """Zone Z, when not running, is started through systemd (else: by appzones). Zones
    that run stay with whoever started them: see client.unit_runs."""
    s = supervisor(z)
    return s == "systemd" or (s == "auto" and unit_installed())


def systemctl(*args: str, check: bool = True) -> subprocess.CompletedProcess:
    r = subprocess.run(["systemctl", "--user", *args], capture_output=True, text=True)
    if check and r.returncode != 0:
        die(f"systemctl --user {' '.join(args)}: {r.stderr.strip() or r.returncode}")
    return r


def server_command(argv: list[str], cwd: str) -> tuple[str, str] | None:
    """(subcommand, zone) when ARGV is an appzones zone server: the interpreter, its
    options, then appzones' zone.py (this one, or another installed appzones' or zones'
    from the Nix store: a server from before an upgrade), then `serve ZONE` or
    `_supervise ZONE`. Only the process's own script counts, never later arguments."""
    i = 1
    while i < len(argv) and argv[i].startswith("-"):
        if argv[i] in ("-c", "-m"):  # code or a module, not a script
            return None
        i += 2 if argv[i] in ("-X", "-W", "-Q") else 1  # options that take a value
    if i >= len(argv):
        return None
    script, rest = argv[i], argv[i + 1:i + 3]
    if len(rest) < 2 or rest[0] not in ("serve", "_supervise") or not script.endswith("zone.py"):
        return None
    path = (Path(cwd) / script).resolve()
    installed = path.parent.name in ("appzones", "zones") and path.parent.parent.name == "libexec" \
        and str(path).startswith("/nix/store/")
    if path == (HERE / "zone.py").resolve() or installed:
        return rest[0], rest[1]
    return None


_state = os.environ.get("XDG_STATE_HOME", "")
# a relative XDG_STATE_HOME is ignored, as the XDG spec says (and the never-list does)
LOG_DIR = (Path(_state) if os.path.isabs(_state) else HOME / ".local/state") / "appzones"
LOG_MAX = 2 * 1024 * 1024  # a log is rotated (to NAME.log.1) at a start when it is bigger


def log_path(name: str) -> Path:
    """Zone NAME's log when appzones starts it itself (without systemd)."""
    return LOG_DIR / f"{name}.log"
