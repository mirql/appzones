"""appzones: app zones on this host (appzones, https://github.com/mirql/appzones).

    appzones list [--json]                 zones: state, network, dns, devices, apps running, colour
                                           (--json: one object per zone, for scripts and bars)
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
    appzones serve ZONE [--notify-fd N]    the zone itself, in the foreground: builds it, holds it,
                                           exits when it ends (SIGTERM/SIGINT: stop). For a process
                                           manager or a terminal; appzones@ZONE.service runs it.
                                           Ready: sd_notify READY=1 ($NOTIFY_SOCKET), and with
                                           --notify-fd N a newline on fd N (s6 style)

Internal (systemd and the supervisor):
    appzones _netsetup RULES INFO ZONE -- CMD
                                           load nft RULES in this netns, open INFO as fd 3, make
                                           ZONE's agent socket (for zone-init: $ZONE_CTL_FD), exec CMD
    appzones _autostart                    write launcher entries, start zones with autostart = true

Config: ~/.config/appzones/zones.toml ($APPZONES_CONFIG). State: ~/.local/share/appzones.
Before 0.6: ~/.config/zones and ~/.local/share/zones (read, and moved; see migrate_state).
Docs: https://mirql.github.io/appzones/ (docs/ in the repo).
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import zonecfg  # noqa: E402
from appzones.common import die  # noqa: E402
from appzones import agent, client, mounts, server, upgrade  # noqa: E402


def main(argv: list[str]) -> int:
    if not argv or argv[0] in ("-h", "--help", "help"):
        print(__doc__)
        return 0 if argv else 2
    cmd, rest = argv[0], argv[1:]
    if cmd == "_autostart" or not (cmd.startswith("_") or cmd in ("active", "serve")):
        upgrade.migrate_state()

    def one() -> str:
        if not rest:
            die(f"usage: appzones {cmd} ZONE")
        return rest[0]

    if cmd in ("serve", "_supervise"):  # _supervise: the name before 0.7, for units still running
        name, fd = one(), None
        opts = rest[1:]
        while opts:
            o = opts.pop(0)
            v = o.split("=", 1)[1] if o.startswith("--notify-fd=") else opts.pop(0) if o == "--notify-fd" and opts else None
            if not o.startswith("--notify-fd") or v is None or not (v.isascii() and v.isdigit() and len(v) <= 9):
                die(f"usage: appzones serve ZONE [--notify-fd N] (not {o!r})")
            fd = int(v)
        return server.supervise(name, notify_fd=fd)
    if cmd == "_netsetup":
        i = rest.index("--")
        server.netsetup(rest[0], rest[1], rest[2], rest[i + 1:])
    if cmd == "_autostart":
        return client.autostart()
    if cmd == "list":
        return client.zone_list("--json" in rest)
    if cmd == "which":
        return client.which(one())
    if cmd == "up":
        return client.each_zone(cmd, rest, client.up)
    if cmd == "stop":
        return client.each_zone(cmd, rest, client.stop_zone)
    if cmd == "restart":
        return client.each_zone(cmd, rest, client.restart_zone)
    if cmd == "ping":
        r = agent.ping(one()); print(json.dumps(r)); return 0 if r else 1
    if cmd == "ps":
        for p, c in agent.zone_procs(one()):
            print(p, c)
        return 0
    if cmd == "logs":
        follow = any(a in ("-f", "--follow") for a in rest)
        rest[:] = [a for a in rest if a not in ("-f", "--follow")]
        return client.logs(one(), follow)
    if cmd == "shell":
        return client.shell(one())
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
        return client.run(name, words, flags["--wait"], flags["--restart"], flags["--bus"])
    if cmd == "new":
        return client.new(one(), "-y" in rest[1:])
    if cmd == "check":
        if rest[:1] == ["--config-only"]:
            rest.pop(0)
        probe = rest[1] if rest[:1] == ["--probe"] and len(rest) > 1 else None
        return client.check(probe)
    if cmd == "apps":
        return client.apps()
    if cmd == "active":
        return client.active("--follow" in rest, "--json" in rest)
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
            return mounts.mount(words[0], words[1], opts["--ro"], opts["--as"])
        if cmd == "umount" and len(words) == 2 and not opts["--ro"] and opts["--as"] is None:
            return mounts.umount(words[0], words[1], opts["--lazy"])
        if cmd == "mounts" and len(words) == 1:
            return mounts.mounts(words[0])
        die("usage: appzones mount ZONE HOST-DIR [--ro] [--as NAME] | appzones umount ZONE NAME [--lazy] | appzones mounts ZONE")
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except KeyboardInterrupt:
        sys.exit(130)
