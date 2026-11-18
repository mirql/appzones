"""Moving the state of appzones before 0.6."""
from __future__ import annotations

import os
from pathlib import Path

import zonecfg

from .common import warn
from . import common


# ── upgrading from before 0.6 ──────────────────────────────────────────────
def supervisors() -> list[str]:
    """Command lines of this user's zone supervisors (any appzones version),
    other than this process."""
    out = []
    for d in Path("/proc").iterdir():
        if not d.name.isdigit() or int(d.name) == os.getpid():
            continue
        try:
            if d.stat().st_uid != common.UID:
                continue
            argv = (d / "cmdline").read_bytes().split(b"\0")
        except OSError:
            continue
        if (b"serve" in argv or b"_supervise" in argv) and any(a.endswith(b"zone.py") for a in argv):
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
    if common.LEGACY_CONFIG:
        warn(f"reading {common.LEGACY_CONFIG}: since 0.6 it belongs in ~/.config/appzones/zones.toml "
             "(mkdir -p ~/.config/appzones && mv ~/.config/zones/zones.toml ~/.config/appzones/)")
    running = supervisors()
    if any("/libexec/zones/" in r for r in running):
        warn("zones started by appzones before 0.6 are running and this version does not see them: "
             "`appzones stop --all`, then start them again")
    if zonecfg.env("STATE"):
        return
    new = common.HOME / ".local/share/appzones"
    if common.STATE != common.LEGACY_STATE:
        if os.path.lexists(common.LEGACY_STATE) and os.path.lexists(new):
            warn(f"{common.LEGACY_STATE} is no longer used (since 0.6 {new} is): move what you still need from it, "
                 "then remove it")
        return
    if running or mounted_below(common.LEGACY_STATE):
        warn(f"{common.LEGACY_STATE} moves to {new} once no zone runs (appzones stop --all)")
        return
    try:
        os.rename(common.LEGACY_STATE, new)
    except OSError as e:
        if not os.path.lexists(new):  # another appzones may have moved it just now
            warn(f"could not move {common.LEGACY_STATE} to {new}: {e.strerror or e}; using it where it is")
            return
    else:
        warn(f"moved {common.LEGACY_STATE} to {new} (appzones 0.6)")
    common.STATE, common.LAUNCHER = new, new / "launcher/share/applications"
