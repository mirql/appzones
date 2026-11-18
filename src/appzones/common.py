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
