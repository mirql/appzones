# Changelog

The config schema has its own release number (`requires = N` in
`zones.toml`, `RELEASE` in `src/zonecfg.py`), separate from the version.

## 0.8.0

**0.8: zones beyond NixOS.** appzones ran only on NixOS: a zone's file system was
built from NixOS's layout, its DNS needed a recent pasta, and it was installed only
through the flake. 0.8 runs command-line zones on **Ubuntu 24.04 and Debian 12** as a
preview: dev tools and AI agents, with or without a desktop, also over SSH, installed
with `make install` and tested on both for every change. Desktop apps there are not
tested yet. On NixOS nothing changes for you. How to install:
https://mirql.github.io/appzones/getting-started/install.html (milestone "0.8.0:
zones beyond NixOS", #68).

**Upgrading from 0.7:**

- On NixOS with the module, nothing to do. Running zones keep working; restart them
  (`appzones restart ZONE`) to get 0.8's DNS in the zone (zone-init on `127.0.0.53`).
- Hyprland: zone windows that appzones cannot name (a zone started without systemd,
  another sandbox such as Flatpak) now get a red border and the tag `zone:?` instead
  of the host's look. Reload Hyprland's config; if you copied `appzones.lua`,
  copy the new one first.
- A mistyped command prints one line and exits 2, no longer the whole usage text.

**What changed:**

- **A zone's file system on other distributions** (0.8 milestone "zones beyond
  NixOS", #62): without `/run/current-system`, a zone gets the host's `/usr`
  read-only and `/bin`, `/sbin`, `/lib*` as the host has them, the `/etc` files
  such systems need (`alternatives`, `ld.so.cache`, ...), and a matching `PATH`,
  `XDG_DATA_DIRS` and `TZDIR`; the D-Bus proxy's sandbox too. NixOS stays as it
  is (`APPZONES_LAYOUT` overrides the choice). The shell, the `xdg-open` shim and
  `appzones check` no longer name `/run/current-system`. `nix_ld` is refused
  outside NixOS, `packages` warns without Nix, and appzones run from inside the
  home says it must be installed outside it (a zone cannot see it there). Not
  enough on its own yet: an install outside Nix and DNS with older pasta follow.

- **A zone answers its own DNS** (#64): zone-init listens on `127.0.0.53` in
  the zone and hands each query over `/run/zone/dns.sock` to the zone's server,
  which asks the host's resolver (`dns = "host"`, also a loopback one like
  systemd-resolved's) or 1.1.1.1 and 1.0.0.1 (`dns = "public"`). Before, pasta forwarded
  DNS, which needs a 2024 pasta (`--dns-host`): Ubuntu 24.04's could not do
  `dns = "public"`, Debian 12's no DNS at all. Now any pasta works; tested on
  both. Only standard queries, at most 4 KB, 64 at a time and 100 a second,
  only to that resolver; UDP answers too big for the client come back
  truncated, then whole over TCP.

- **Host tools in `/usr/sbin` and `/sbin`** (#63): appzones looks for `nft`, `bwrap`,
  `pasta` and its other helpers on `PATH`, then in `/usr/sbin` and `/sbin`. On Debian
  `nft` lives in `/usr/sbin`, which a user's `PATH` lacks, so a zone with
  `network = "internet"` did not start ("nft not found on PATH"). `APPZONES_BIN_<NAME>`
  still overrides. Thanks to @emreocell (#72).

- **`make install` outside Nix** (#65): `make && sudo make install` installs
  appzones under `/usr/local` (must be under `/usr`, which zones see), builds
  `zone-wayland` when libwayland's development files are there (the protocol
  file is in the repo, for Debian 12's older wayland-protocols), and installs
  the systemd user units, a sudo rule for `appzones mount`, and on Ubuntu 23.10+
  an AppArmor profile so bwrap and pasta may create user namespaces
  (`contrib/apparmor/`). `sudo make uninstall` removes it all. appzones finds
  its own `zone-wayland` next to itself, uses `/usr/bin/sudo` outside NixOS, and
  says clearly when it is installed where a zone cannot see it. Tested on
  Ubuntu 24.04 and Debian 12; a Nix check runs the install into a staging
  directory.

- **Tests on Ubuntu 24.04 and Debian 12** (#66): `just test-distros` boots each in
  a VM from pinned packages and runs `make install`, then zones as an ordinary
  user without systemd: on-demand start, the home and never-list, a sandbox
  inside, `appzones shell`, `appzones mount` through the sudo rule, DNS from the
  host's loopback resolver, the host refused, stop, `make uninstall`. Found and
  fixed with them: `make install` failed where AppArmor is not active (the
  profile is now installed and loaded only where AppArmor runs; it needs
  AppArmor 4); and `appzones mount` on a host whose mounts are private (no
  systemd) now says why the zone does not see the mount and how to fix it
  (`mount --make-rshared /`), instead of a wrong hint.

- **Docs: installing on Ubuntu and Debian** (#67): the install page has a part
  for each, NixOS and Ubuntu 24.04 / Debian 12 (the packages, `make install` and
  its options, what it installs and why, a first command-line zone, what is
  different from NixOS, without systemd, update and remove, kernel modules
  outside NixOS). The README, the docs home page and the agent skill say where
  appzones runs.

- **Window borders fail closed** (Hyprland): a window that comes from a zone but
  cannot be named gets the tag `zone:?` and a red warning border, never the host's
  look. Before, it got none: a zone started without systemd, a process under
  `appzones.slice` that is not a zone's unit, an unreadable cgroup. A process in
  another pid namespace counts as such a window too (so does another sandbox,
  e.g. Flatpak). The zone's own name still comes from its unit's cgroup, now also
  from a cgroup below it. `appzones active` shows `[?]`.

- **A zone always has its own cgroup namespace**: bwrap's `--unshare-cgroup`
  instead of `--unshare-cgroup-try`, which went on without one when it could not
  make it.

- **`appzones --version`** (and `-V`) prints the package version and the config release,
  e.g. `appzones 0.7.0 (config release 5)`, for bug reports and `requires = N` mismatches.
  A source checkout prints `unknown` for the version. Thanks to @emreocell (#71).

- **An unknown command says so, with the nearest one** (#74): `appzones lsit`
  now prints `appzones: unknown command 'lsit' (did you mean 'list'?); see
  appzones --help` and exits 2, instead of the whole 60-line usage text. The
  suggestion is dropped when nothing is close. `appzones` with no arguments
  and `--help` still print the full usage. Thanks to @tyza66 (#81).

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
