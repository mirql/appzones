# Zones without systemd

appzones 0.7 is about one change of direction: **a zone should not need systemd**.
This page says why, what is already there, what is coming, and what it means for
you. The work is tracked in the milestone
[0.7.0: zones without systemd](https://github.com/mirql/appzones/milestone/1)
(issue [#50](https://github.com/mirql/appzones/issues/50)).

## Why

Until 0.6, every zone was a systemd user unit (`appzones@NAME.service`), and the
unit did real work: it held the zone's processes in a cgroup, cleaned them all up
when the zone stopped, restarted it after a crash, and kept its logs. That worked,
but it tied appzones to two things it does not otherwise need:

- **systemd itself.** Distributions and setups without it (Void, Alpine, Gentoo
  with OpenRC, a plain terminal session) could not run zones.
- **The NixOS module.** The unit files have to be installed for anything to start,
  so appzones could not simply be run from a package or `nix run`.

It also hid problems. Outside the unit, a crashed or killed zone server left the
zone running, and two servers for the same zone could get in each other's way.
systemd's cleanup covered that up; appzones did not handle it itself.

The goal is the Unix way of doing it: **a zone is one plain process**, and
anything can run it, whether systemd, runit, s6, dinit or a terminal. appzones
itself makes sure there is exactly one per zone and that nothing outlives it.

## What you get

- **The same behaviour on NixOS, nothing to do.** With the module installed,
  systemd stays the default supervisor. Window borders on Hyprland still come from
  the unit's cgroup.
- **Zones that clean up after themselves**, however they end: a zone and all its
  helpers end with their server, even after `kill -9`.
- **One server per zone, always.** A second start is refused cleanly instead of
  breaking the running zone.
- **Your choice of process manager.** `appzones serve ZONE` runs a zone in the
  foreground with standard readiness signalling, so runit, s6, dinit or a script
  can manage zones the way they manage anything else.
- **Zones that start on demand** without any service manager, the way `tmux` or
  `gpg-agent` do: `appzones run` starts the zone if it is not running.
- **No setting needed:** appzones uses systemd when its unit is installed, and
  starts zones itself otherwise. `stop`, `restart` and `logs` follow whoever runs
  a zone.
- **Per-zone choices** in `zones.toml`: `supervisor`, `start_timeout`, and
  `linger` for a zone that should end by itself when no app runs in it.
- **Service files** for runit, s6 and dinit (and systemd outside NixOS), in
  `contrib/`.
- **Other distributions:** Ubuntu 24.04 and Debian 12 as a preview
  (command-line zones), with `make install`
  ([Install](getting-started/install.md#ubuntu-and-debian-preview)).

## What is done

| | What | Since |
|---|---|---|
| ✓ | `appzones serve ZONE [--notify-fd N]`: a zone in the foreground; readiness over sd_notify, or as a newline on fd N (s6 style); SIGTERM or Ctrl-C stops it | [#43](https://github.com/mirql/appzones/pull/43) |
| ✓ | One server per zone: a lock (`$XDG_RUNTIME_DIR/appzones/NAME.lock`); a second `serve` exits with status 75, "already served", and leaves the running zone alone | [#51](https://github.com/mirql/appzones/pull/51) |
| ✓ | The zone ends with its server: zone-init watches a pidfd of the server, zone-wayland and the D-Bus proxy die with it; the startup waits (zone-wayland's "ready", the bus, the pidfd hand-over, zone-init's answer) have a 30-second deadline, building `packages` none | [#53](https://github.com/mirql/appzones/pull/53) |
| ✓ | Start zones on demand without systemd: `appzones run`, `up` and `shell` start the zone when it does not answer (one server even with many clients at once), with logs in `~/.local/state/appzones/` | [#46](https://github.com/mirql/appzones/issues/46) |
| ✓ | Supervisor mode `auto` (`APPZONES_SUPERVISOR`, also `systemd` or `spawn`): systemd when the unit is installed and the user manager answers, otherwise on demand; never both for one zone; `stop`, `restart`, `logs` follow whoever runs it; a warning when systemd has no `WAYLAND_DISPLAY` | [#47](https://github.com/mirql/appzones/issues/47) |
| ✓ | Per-zone settings (zones release 5): `supervisor`, `start_timeout` (the limit for each startup step), `linger` (end a zone after some time with no app, only if you want that) | [#48](https://github.com/mirql/appzones/issues/48) |
| ✓ | Service files for systemd, runit, s6 and dinit in `contrib/` (also in the package, `share/appzones/contrib`); `serve` exits 0 when stopped, so a manager restarts only failures | [#49](https://github.com/mirql/appzones/issues/49) |
| ✓ | Other distributions: Ubuntu 24.04 and Debian 12 (preview), with `make install`; see [Install](getting-started/install.md#ubuntu-and-debian-preview) | [0.8.0](https://github.com/mirql/appzones/milestone/2) |

## Running a zone yourself today

Without the appzones systemd unit installed (or without systemd), appzones
starts zones on demand by itself:

```bash
appzones run daily firefox      # starts the zone if needed, then Firefox in it
appzones logs daily             # ~/.local/state/appzones/daily.log
appzones stop daily
```

`APPZONES_SUPERVISOR` chooses who starts a zone that is not running:

| Value | Starts zones through |
|---|---|
| `auto` (default) | systemd, when `appzones@.service` is installed and the user manager answers; otherwise appzones itself |
| `systemd` | always systemd |
| `spawn` | always appzones itself, also where the unit is installed |

A running zone stays with whoever started it: `stop`, `restart` and `logs` find
it there. The unit and appzones never both run a zone: the second one finds the
zone's lock taken and stops.

Zones that systemd starts get the environment of the systemd user manager. If
it has no `WAYLAND_DISPLAY` (a session without uwsm or `systemctl --user
import-environment`), appzones warns on start; import it, or use `spawn`.

Many clients at once still get exactly one server per zone: the zone's lock
decides who starts it, and the others wait for it to answer. A waiting client
gives up after about 60 seconds (a first start that builds `packages` can take
longer); run the command again once the zone answers.

Or run the zone itself: `appzones serve` is the zone. Run it in a terminal to
watch a zone start and stop (Ctrl-C ends it):

```bash
appzones serve daily
```

Or under a process manager that reads readiness from a pipe, such as s6:

```bash
appzones serve daily --notify-fd 3     # writes a newline on fd 3 once the zone answers
```

## A process manager of your own

`contrib/` (in the package: `share/appzones/contrib/`) has ready service files.
Each runs `appzones serve ZONE`. `serve` exits 0 when the zone ends for good
(`appzones stop`, Ctrl-C, `linger`), 143 when the manager stops or restarts it
(SIGTERM), and 75 when the zone already runs elsewhere; the files keep a zone
down after 0 and 75, restart it after a failure, and otherwise do what you tell
the manager.

| Manager | Files | Start a zone |
|---|---|---|
| runit | `runit/appzones-ZONE/`: copy it as `appzones-daily` into your service directory | `sv up appzones-daily` |
| s6 | `s6/appzones-ZONE/`: copy it as `appzones-daily`; readiness through `notification-fd` | `s6-svc -u appzones-daily`, then `s6-svwait -U appzones-daily` |
| dinit | `dinit/appzones`: one file for every zone | `dinitctl start appzones@daily` |
| systemd | `systemd/`: the units the NixOS module installs, for other distributions | `systemctl --user start appzones@daily` |

Start the process manager from your graphical session, after the compositor:
a zone needs `WAYLAND_DISPLAY` and the session bus from its environment, and it
stops when the compositor goes away. `appzones stop` works on these zones too
(it signals the server; the manager then leaves it down), and so do the
manager's own commands. `appzones run` on a zone that is not running starts
it as `supervisor` says, not through your manager: start zones through the
manager first, or set `supervisor = "systemd"` only where systemd is the manager.

`appzones mount` needs the host's mounts to be shared, which systemd sets up at
boot. Without systemd, run `sudo mount --make-rshared /` once per boot, before
zones start (from the init system or your session's start script).

Zones stay **long-lived**: a zone runs until you stop it or your session ends.
When the compositor goes away, the zone's Wayland helper exits and the zone stops
with it. To have a zone end by itself, give it a `linger` time:

```toml
[zones.untrusted]
linger = "10m"         # ends 10 minutes after its last app exits
supervisor = "spawn"   # this zone: always started by appzones, not systemd
start_timeout = "1m"   # a slow machine: a minute for each startup step
```

## What does not change

- `zones.toml`, the zone homes, the never-list and every isolation property stay
  as they are. This is about who starts and stops a zone, not about what a zone
  can reach.
- With the NixOS module, zones keep running as `appzones@NAME.service`, which now
  runs `appzones serve %i`.
- On Hyprland, a zone's border colour comes from its cgroup, which systemd
  provides. Without systemd, a zone's windows get the tag `zone:?` and a red
  warning border: appzones knows they come from a zone, not which one. Naming
  them needs window identity from the Wayland security context. Sway 1.10 and
  later expose it, and `contrib/sway/appzones-sway` uses it; Hyprland needs a
  small upstream change.

See [How it works](how-it-works.md) for how a zone is built, and the
[CHANGELOG](https://github.com/mirql/appzones/blob/main/CHANGELOG.md) for every
change.
