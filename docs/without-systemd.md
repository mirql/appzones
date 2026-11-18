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
- **Later in 0.7:** choosing the supervisor per zone, and a path to running
  appzones outside NixOS.

## What is done

| | What | Since |
|---|---|---|
| ✓ | `appzones serve ZONE [--notify-fd N]`: a zone in the foreground; readiness over sd_notify, or as a newline on fd N (s6 style); SIGTERM or Ctrl-C stops it | [#43](https://github.com/mirql/appzones/pull/43) |
| ✓ | One server per zone: a lock (`$XDG_RUNTIME_DIR/appzones/NAME.lock`); a second `serve` exits with status 75, "already served", and leaves the running zone alone | [#51](https://github.com/mirql/appzones/pull/51) |
| ✓ | The zone ends with its server: zone-init watches a pidfd of the server, zone-wayland and the D-Bus proxy die with it; the startup waits (zone-wayland's "ready", the bus, the pidfd hand-over, zone-init's answer) have a 30-second deadline, building `packages` none | [#53](https://github.com/mirql/appzones/pull/53) |
| ✓ | Start zones on demand without systemd: `appzones run`, `up` and `shell` start the zone when it does not answer (one server even with many clients at once), with logs in `~/.local/state/appzones/` | [#46](https://github.com/mirql/appzones/issues/46) |
| | Supervisor mode `auto`: systemd when the unit is installed, otherwise on demand; never both for one zone | [#47](https://github.com/mirql/appzones/issues/47) |
| | Per-zone settings: `supervisor`, `start_timeout`, `linger` (end a zone after some idle time, only if you want that) | [#48](https://github.com/mirql/appzones/issues/48) |
| | Service files for runit, s6 and dinit; a try outside NixOS | [#49](https://github.com/mirql/appzones/issues/49) |

## Running a zone yourself today

Let appzones start zones on demand, without systemd. Until the automatic mode
comes ([#47](https://github.com/mirql/appzones/issues/47)), you choose it with an
environment variable:

```bash
export APPZONES_SUPERVISOR=none
appzones run daily firefox      # starts the zone if needed, then Firefox in it
appzones logs daily             # ~/.local/state/appzones/daily.log
appzones stop daily
```

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

Zones stay **long-lived**: a zone runs until you stop it or your session ends.
When the compositor goes away, the zone's Wayland helper exits and the zone stops
with it.

## What does not change

- `zones.toml`, the zone homes, the never-list and every isolation property stay
  as they are. This is about who starts and stops a zone, not about what a zone
  can reach.
- With the NixOS module, zones keep running as `appzones@NAME.service`, which now
  runs `appzones serve %i`.
- On Hyprland, a zone's border colour comes from its cgroup, which systemd
  provides. Without systemd, windows keep working but get no zone border until
  window identity comes from the Wayland security context instead. Sway already
  exposes it; Hyprland needs a small upstream change.

See [How it works](how-it-works.md) for how a zone is built, and the
[CHANGELOG](https://github.com/mirql/appzones/blob/main/CHANGELOG.md) for every
change.
