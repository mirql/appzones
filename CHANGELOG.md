# Changelog

The config schema has its own release number (`requires = N` in
`zones.toml`, `RELEASE` in `src/zonecfg.py`), separate from the version.

## Unreleased

**0.7: zones without systemd.** Until now every zone was a systemd user unit, and
the unit did real work: it cleaned up the zone's processes, restarted it, kept its
logs. That tied appzones to systemd and to its NixOS module, and it hid problems:
outside the unit, a killed zone server left the zone running. 0.7 makes a zone one
plain process, `appzones serve`, that any process manager or a terminal can run, and
makes appzones itself keep one server per zone and leave nothing behind. On NixOS
with the module nothing changes for you. Why, what is done and what is coming:
https://mirql.github.io/appzones/without-systemd.html (milestone "0.7.0: zones
without systemd", #50).

- **The zone ends with its server**, however the server ends: outside systemd, a
  killed `appzones serve` used to leave zone-init, bwrap and pasta running.
  zone-init now holds a pidfd of the server and ends the zone when the server is
  gone; zone-wayland and the D-Bus proxy are tied to it as well. Waiting for
  zone-wayland's "ready" (which could block forever), the D-Bus proxy's bus, the
  pidfd hand-over and zone-init's answer each have a 30-second deadline; building
  `packages` comes before them and has none.

- **One server per zone**: `appzones serve` holds a lock
  (`$XDG_RUNTIME_DIR/appzones/NAME.lock`) before it changes anything; a second one
  exits with status 75 ("already served") instead of replacing the first zone's
  socket. `--lock-fd N` takes a lock a caller already holds. The unit does not
  restart on 75.

- **`appzones serve ZONE [--notify-fd N]`**: the zone itself, in the foreground, as a
  public command (it was the internal `_supervise`). The systemd unit runs it, and so can
  any process manager or a terminal. Readiness over sd_notify, and with `--notify-fd N` a
  newline on fd N (s6 style); SIGTERM or SIGINT stops the zone. `_supervise` still works
  for units that are running during the upgrade.

## 0.6.2

- **Docs for AI tools**: https://mirql.github.io/appzones/llms.txt (an index of the docs,
  with raw Markdown links) and https://mirql.github.io/appzones/llms-full.txt (every guide
  page in one file), generated from the docs pages by the site build.

- **JSON output for `appzones list --json`**: prints a JSON array with one object per zone,
  for scripts, status bars and AI agents. Fields: `name`, `state`, `network`, `dns`, `devices`,
  `apps`, `colour`.

- `src/zone.py` is split into modules under `src/appzones/` (client, server, fs, net,
  portals, agent, mounts, upgrade, common); no change in behaviour.

## 0.6.1

- zone-init's control socket is created by the host, outside the zone
  (`$XDG_RUNTIME_DIR/appzones/NAME/agent.sock`), and handed to zone-init as an
  fd; `/run/zone` is read-only inside the zone, and zone-init cannot be traced
  from inside it. Restart running zones after upgrading.

- **The docs are split into pages** (https://mirql.github.io/appzones/): getting started, recipes,
  configuration (reference, files, devices, D-Bus, generic binaries), desktop,
  troubleshooting (new), how it works, security, contributing. `docs/guide.md`
  is now their contents page.

- **A docs site**: https://mirql.github.io/appzones/ (the guide, with search), built by
  GitHub Pages from `docs/`.

- **An agent skill** (`skills/appzones/SKILL.md`, `npx skills add
  mirql/appzones`): how a coding agent runs things in zones, changes the
  config and debugs a zone, with rules (ask before stopping a zone, never
  weaken isolation on its own).

## 0.6.0

- **Everything is named appzones now**, not only the command:
  `~/.config/appzones/zones.toml`, `~/.local/share/appzones/` (zone homes),
  `/etc/appzones/denied-paths`, `/run/user/UID/appzones/`, `$APPZONES_*`,
  `appzones-mount-helper`, `contrib/hyprland/appzones.lua`, launcher entries
  `appzones-APP-ZONE.desktop`. The window tag `zone:NAME`, `zones.toml` and
  `$ZONE` inside a zone keep their names: they name a zone, not the tool.
- **Upgrading** (docs/guide.md "Upgrading from 0.5"): `appzones stop --all`
  after the switch. The next `appzones` command moves `~/.local/share/zones`
  to `~/.local/share/appzones` (a rename, only when no zone runs and nothing
  is mounted there). The old config file is read, with a warning saying how to
  move it. Update backups or impermanence lists that name the old paths.
- Still read in 0.6, removed later: `$ZONES_*`, `~/.config/zones/zones.toml`,
  `/etc/zones/denied-paths`, `contrib/hyprland/zones.lua` (it loads
  appzones.lua). The never-list refuses binds of the old and the new paths.
- `just release VERSION`: tags a release and publishes its CHANGELOG section
  as the GitHub release notes.

## 0.5.0

- **Renamed the command** `zone` to `appzones` (`appzones run daily
  firefox`), the same name as the project, so it is not mixed up with other
  "zone" tools. The user units are now `appzones@NAME.service`,
  `appzones-autostart.service` and `appzones.slice`; update a bar module
  (`appzones active --follow --json`) and the Hyprland hook
  (`contrib/hyprland/zones.lua` matches the new cgroup path). Unchanged:
  `~/.config/zones/zones.toml`, `~/.local/share/zones/` (zone homes),
  `/etc/zones/denied-paths`, the `zone:NAME` window tag, `ZONES_*`
  variables, config release 4.

## 0.4.0

First public release.

- `zone` CLI and supervisor, `zone@.service` user units, NixOS module
  (`programs.appzones`).
- Config release 4: zones with their own home, network (`none`,
  `internet`, `host`), DNS, devices (`gpu`, `audio`, `mic`, `camera`,
  `screencast`, `usb`), D-Bus buses (`host`, `zone`, `none`), `nix_ld`,
  `tmp = "disk"`, runtime mounts (`zone mount`).
- Host-specific denied paths (`deniedPaths`, `/etc/zones/denied-paths`)
  and the NetBird interface (`netbirdInterface`) are settings rather
  than built in.
- Hyprland hook for zone window tags and border colours, and
  `zone active` for bars.

Hardening before the first release:

- Binds and a custom `home` are checked on the resolved path, also before
  the home exists; nothing from inside `/run`, `/proc`, `/sys`, `/dev`,
  `/tmp`, `/etc`, `/nix`, `/var`, `/boot` or `/root`; never a socket, FIFO
  or device. The supervisor pins each bind (`--bind-fd`) and checks the
  opened object, so a symlink swapped in after validation is refused.
- Theme files are checked where they really are before they are copied in.
- The network deny list refuses every host address and every connected
  network (IPv6 LANs with global addresses too); NetBird allows only its
  networks and never the host's own addresses.
- No default `org.mpris.MediaPlayer2.*` grant; overlapping `dbus.own`
  names across zones and zone-owned names are refused; activatable names
  come from the running bus (ListActivatableNames), failing closed.
- `bus` is documented as routing, not a boundary inside a zone; `bus = "none"`
  also hides zone-init's control socket, and the xdg-open shim directory is
  read-only. `--wait` passes only pipes, regular files and `/dev/null`.
- An unreadable or malformed `/etc/zones/denied-paths` refuses binds
  instead of being ignored.
- `dns = "host"` follows the host's `resolv.conf` (or `dnsServer`), also
  with `network = "host"`.
- `zone new -y` creates a missing config and writes atomically under a
  lock; `examples/minimal.toml`. Malformed values are errors, never a crash.
