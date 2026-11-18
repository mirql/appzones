"""Runtime mounts: `appzones mount` / `umount` / `mounts`."""
from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

import zonecfg

from .common import die, load_zone, warn
from . import agent, common, fs


def mounts_file(name: str) -> Path:
    return common.RT / name / "mounts.json"  # sources, for `appzones mounts`; gone when the zone restarts


def read_mounts(name: str) -> dict:
    try:
        return json.loads(mounts_file(name).read_text())
    except (OSError, ValueError):
        return {}


def write_mounts(name: str, m: dict) -> None:
    tmp = mounts_file(name).with_suffix(".tmp")
    tmp.write_text(json.dumps(m, indent=1) + "\n")  # NOSONAR: a file in the zone's own runtime directory
    tmp.replace(mounts_file(name))


def helper_failed(r: subprocess.CompletedProcess, what: str):
    err = (r.stderr or r.stdout).strip()
    if "password is required" in err or "not allowed" in err or "may not run sudo" in err:
        die(f"{what}: sudo refused the mount helper ({err.splitlines()[-1]}). It needs the NixOS module's sudo "
            f"rule (programs.appzones.mountHelper = true, for programs.appzones.user) and a rebuilt system")
    die(f"{what}: {err.removeprefix('appzones-mount-helper: ') or r.returncode}")


def mount(name: str, path: str, ro: bool, as_name: str | None) -> int:
    load_zone(name)
    if not agent.ping(name):
        die(f"zone {name} is not running; a mount lives only while the zone runs (`appzones up {name}` first)")
    err = zonecfg.mount_source_error(path)
    if err:
        die(f"refused: {err}")
    src = os.path.realpath(zonecfg.expand(path))
    nm = as_name or os.path.basename(src)
    if not zonecfg.MOUNT_NAME_RE.match(nm):
        die(f"{nm!r} cannot be a mount name (letters, digits, . _ -, not first; at most 64): give one with --as NAME")
    if nm in fs.host_mounts(name):
        die(f"/mnt/host/{nm} is already mounted in zone {name} (appzones mounts {name})")
    target = common.STATE / name / "mnt" / nm
    target.mkdir(exist_ok=True)
    r = fs.helper("mount", name, nm, src, *(["--ro"] if ro else []))
    if r.returncode != 0:
        try:
            target.rmdir()
        except OSError:
            pass
        helper_failed(r, "mount")
    m = read_mounts(name)
    m[nm] = {"source": src, "mode": "ro" if ro else "rw"}
    write_mounts(name, m)
    inside = agent.zone_request(name, {"op": "mounts"})
    if inside and f"/mnt/host/{nm}" not in [p for p, _ in inside.get("mounts", [])]:
        warn(f"mounted on the host, but zone {name} does not see /mnt/host/{nm} (did the zone start before release "
             f"3? `appzones restart {name}`)")
    print(f"{src} is /mnt/host/{nm} in zone {name} ({'read-only' if ro else 'read-write'}), until "
          f"`appzones umount {name} {nm}` or the zone stops")
    return 0


def umount(name: str, nm: str, lazy: bool) -> int:
    load_zone(name)
    if nm not in fs.host_mounts(name):
        die(f"nothing is mounted at /mnt/host/{nm} in zone {name} (appzones mounts {name})")
    r = fs.helper("umount", name, nm, *(["--lazy"] if lazy else []))
    if r.returncode == 3:
        busy = agent.zone_request(name, {"op": "busy", "path": f"/mnt/host/{nm}"}).get("procs", [])
        warn(f"/mnt/host/{nm} is busy in zone {name}" + (":" if busy else " (zone-init could not say by what)"))
        for pid, cmd, what in busy:
            print(f"  pid {pid} {cmd[:60]}: {what}", file=sys.stderr)
        die(f"close those (or stop the app), or detach it now: appzones umount --lazy {name} {nm}")
    if r.returncode != 0:
        helper_failed(r, "umount")
    try:
        (common.STATE / name / "mnt" / nm).rmdir()
    except OSError:
        pass
    m = read_mounts(name)
    m.pop(nm, None)
    if mounts_file(name).parent.is_dir():
        write_mounts(name, m)
    print(f"/mnt/host/{nm} removed from zone {name}" + (" (lazily: open files keep it until they close)" if lazy else ""))
    return 0


def mounts(name: str) -> int:  # NOSONAR: an exit status: 0 is the only one
    load_zone(name)
    names = fs.host_mounts(name)
    if not agent.ping(name):
        if names:
            print(f"zone {name} is not running; left mounted on the host (cleared at its next start): "
                  + ", ".join(names))
        return 0
    m = read_mounts(name)
    inside = agent.zone_request(name, {"op": "mounts"})
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
