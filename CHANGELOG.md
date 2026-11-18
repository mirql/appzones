# Changelog

The config schema has its own release number (`requires = N` in
`zones.toml`, `RELEASE` in `src/zonecfg.py`), separate from the version.

## 0.7.0

**0.7: zones without systemd.** Until now every zone was a systemd user unit, and
the unit did real work: it cleaned up the zone's processes, restarted it, kept its
logs. That tied appzones to systemd and to its NixOS module, and it hid problems:
outside the unit, a killed zone server left the zone running. 0.7 makes a zone one
plain process, `appzones serve`, that any process manager or a terminal can run, and
makes appzones itself keep one server per zone and leave nothing behind. On NixOS
with the module nothing changes for you. Why and how:
https://mirql.github.io/appzones/without-systemd.html (milestone "0.7.0: zones
without systemd", #50).

**Upgrading from 0.6:**

- On NixOS with the module, nothing to do: zones keep running as
  `appzones@NAME.service` and restart into 0.7 when you restart them.
- `zones.toml` needs `requires = 5` only if it uses the new `supervisor`,
  `start_timeout` or `linger`.
- `appzones stop` on a zone outside its unit now ends it (with SIGINT) instead
  of refusing.
- Without systemd, zone logs moved from `$XDG_RUNTIME_DIR/appzones/` to
  `~/.local/state/appzones/`.
- Zones still need NixOS (#22).

**What changed:**

- **`appzones serve ZONE [--notify-fd N]`**: the zone itself, in the foreground, as a
  public command (it was the internal `_supervise`). The systemd unit runs it, and so can
  any process manager or a terminal. Readiness over sd_notify, and with `--notify-fd N` a
  newline on fd N (s6 style); SIGTERM or SIGINT stops the zone. `_supervise` still works
  for units that are running during the upgrade.

- **One server per zone**: `appzones serve` holds a lock
  (`$XDG_RUNTIME_DIR/appzones/NAME.lock`) before it changes anything; a second one
  exits with status 75 ("already served") instead of replacing the first zone's
  socket. `--lock-fd N` takes a lock a caller already holds. The unit does not
  restart on 75.

- **The zone ends with its server**, however the server ends: outside systemd, a
  killed `appzones serve` used to leave zone-init, bwrap and pasta running.
  zone-init now holds a pidfd of the server and ends the zone when the server is
  gone; zone-wayland and the D-Bus proxy are tied to it as well. Waiting for
  zone-wayland's "ready" (which could block forever), the D-Bus proxy's bus, the
  pidfd hand-over and zone-init's answer each have a deadline (`start_timeout`,
  30 seconds by default); building `packages` comes before them and has none.

- **Zones start on demand without systemd** (`APPZONES_SUPERVISOR=spawn`):
  `appzones run`, `up` and `shell` start a zone that does not answer, like tmux or
  gpg-agent. The client takes the zone's lock and hands it to a detached
  `appzones serve`, so many clients at once still get one server; a client killed
  during the start leaves the zone starting. Logs in
  `$XDG_STATE_HOME/appzones/ZONE.log` (by default `~/.local/state`; moved to
  `ZONE.log.1` at a start when over 2 MB), on the never-list; `appzones logs` and
  autostart follow.

- **Who starts zones is chosen for you**: `APPZONES_SUPERVISOR=auto` (the new
  default) uses systemd when `appzones@.service` is installed and the user manager
  answers, otherwise appzones starts zones itself; `systemd` and `spawn` force
  one. A running zone stays with whoever started it: `stop`, `restart` and `logs`
  follow it, and `stop` now also ends a zone served outside its unit (a terminal,
  another process manager) instead of refusing. Starting through systemd warns
  when the user manager has no `WAYLAND_DISPLAY`.

- **Zone settings for the server** (zones release 5, `requires = 5`), per zone or in
  `[defaults]`: `supervisor` (`auto` · `systemd` · `spawn`; the zone's own value
  beats `$APPZONES_SUPERVISOR`), `start_timeout` (the limit for each startup
  step, default `"30s"`), and `linger` (default `""`: the zone stays; `"10m"`:
  the zone ends 10 minutes after its last app, as a success, so systemd does not
  restart it). Changing `supervisor` or `start_timeout` does not mark a running
  zone as stale.

- **Service files for other process managers** in `contrib/` (and the package's
  `share/appzones/contrib`): runit and s6 service directories, a dinit file
  (`appzones@ZONE`), and the systemd units for other distributions. `appzones
  serve`'s exit status now says how it ended: 0 for good (SIGINT, which
  `appzones stop` now sends, Ctrl-C, or `linger`), 143 stopped by SIGTERM (a
  manager's stop or restart; the unit counts it as success), 75 already served;
  a stop during the start is no longer a failure. Zones still need NixOS: the zone's file system uses
  its layout (#22).

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
