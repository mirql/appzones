# Zones without systemd

A zone is one plain process, `appzones serve ZONE`, and anything can run it: systemd,
runit, s6, dinit, a script or a terminal. appzones itself makes sure there is exactly one
server per zone and that nothing outlives it: a zone and all its helpers end with their
server, even after `kill -9`, and a second start is refused instead of breaking the
running zone.

With the NixOS module nothing changes: systemd is the supervisor, and zones run as
`appzones@NAME.service` (which runs `appzones serve %i`). Elsewhere appzones uses systemd
when its unit is installed, and starts zones itself otherwise; `stop`, `restart` and
`logs` follow whoever runs a zone.

## Starting zones without a service manager

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

## Limits

- This is about who starts and stops a zone, not what a zone can reach: `zones.toml`,
  the zone homes, the never-list and every isolation property are the same either way.
- On Hyprland, a zone's border colour comes from its cgroup, which systemd
  provides. Without systemd, a zone's windows get the tag `zone:?` and a red
  warning border: appzones knows they come from a zone, not which one. Naming
  them needs window identity from the Wayland security context. Sway 1.11 and
  later expose it, and `contrib/sway/appzones-sway` uses it; Hyprland needs a
  small upstream change.

See [How it works](how-it-works.md) for how a zone is built.
