# How it works

```
systemd --user: appzones@work.service   (appzones.slice, Type=notify;
│                                         or started by appzones itself)
└─ appzones serve work                           host side
   ├─ zone-wayland      → wp_security_context_v1 listener (Hyprland hides
   │                      screencopy, data-control, layer-shell, virtual
   │                      input, global shortcuts, the window list)
   ├─ bwrap ─ xdg-dbus-proxy → the zone's filtered bus; the proxy's bwrap
   │                      carries /.flatpak-info, so portals see
   │                      app id dev.mirql.zone.work
   └─ pasta (own netns) ─ appzones _netsetup (nft deny list, the agent's
      │                   socket, then exec)
      └─ bwrap (user, pid, ipc, uts, cgroup ns; no caps; no_new_privs)
         └─ zone-init     the agent inside: run, shell (pty), ps (its protocol: the
                          top of src/zone_init.py, tested by AgentProtocolTest; with
                          the Rust helper, appzones-helper zone-init, bound read-only
                          at /run/appzones-helper)
            └─ apps
```

- `appzones serve ZONE` is the zone itself, in the foreground (why: [Zones
  without systemd](without-systemd.md)): the systemd unit
  runs it, and so can any process manager or a terminal. It signals readiness
  over sd_notify, and with `--notify-fd N` as a newline on fd N (s6 style).
  SIGTERM or SIGINT stops the zone and its helpers. Exit status: 0 when the zone
  ended for good (SIGINT: `appzones stop`, Ctrl-C; or `linger`), 143 when
  stopped by SIGTERM (a process manager's stop or restart), 75 when the zone is
  already served, anything else a failure. A stop during the start ends it the
  same way, not as a failure. One server per zone: it
  holds `$XDG_RUNTIME_DIR/appzones/NAME.lock` (released when it exits); a
  second `serve` exits with status 75, "already served", and leaves the running
  zone untouched. With `--lock-fd N` a caller hands over a lock it took: fd N is
  inherited across `exec` (not close-on-exec), and the lock lives as long as any
  copy of it, so the caller closes its own (or `exec`s into `serve`). The lock
  is also how appzones finds a zone's server: `stop`, `restart`, `logs` and a
  waiting client look for the process that has the lock file open (only the
  server keeps it: the descriptor is close-on-exec). A server from before 0.7,
  which has no lock, is found by its command line. zone-init names itself
  `zone-init` (`pgrep -x zone-init`).
- Who starts a zone that does not answer: `APPZONES_SUPERVISOR=auto` (the
  default) uses the unit when it is installed and the user manager answers
  (`systemctl --user show -p LoadState appzones@probe.service` is `loaded`),
  else appzones; `systemd` and `spawn` force one. A running zone stays with
  whoever runs it: `stop`, `restart` and `logs` check the unit first, then
  servers outside it (found by the zone's lock, see above).
- Without systemd (`spawn`), `appzones run`, `up` and `shell`
  start the zone themselves when it does not answer: the client takes the
  zone's lock and starts `appzones serve ZONE --lock-fd L --notify-fd W`
  detached (its own session, cwd `/`, output to
  `$XDG_STATE_HOME/appzones/ZONE.log`, by default `~/.local/state/appzones/`;
  at a start, a log over 2 MB is moved to `ZONE.log.1`), then waits for
  readiness. Other clients wait for the zone to answer, and give up after about
  60 seconds (try again once it answers). The log directory is on the
  never-list: no zone can bind it.
- The zone ends with its server, however the server ends (also `kill -9`):
  zone-init holds a pidfd of the server (handed over a unix socket, since
  pasta closes inherited fds) and ends the zone when it becomes readable;
  zone-wayland (the C helper, or `appzones-helper zone-wayland` when appzones
  was installed with the Rust helper: the `appzones-rust` package or `make
  install HELPER=rust`, which set `APPZONES_HELPER=rust`) gets SIGTERM when the server dies (`PR_SET_PDEATHSIG`); the
  D-Bus proxy is tied the same way. Waiting for zone-wayland's "ready", the
  proxy's bus, the pidfd hand-over and zone-init's answer each have a deadline
  (`start_timeout`, 30 seconds by default); building `packages` comes before
  them and has none.
- With `linger`, the server asks zone-init every few seconds what runs in the
  zone (its own D-Bus daemon does not count). After `linger` with no app it
  stops the zone and exits with status 0, so systemd does not restart it.
- The `appzones` CLI talks to zone-init over a socket the host makes,
  `$XDG_RUNTIME_DIR/appzones/NAME/agent.sock`, outside the zone. zone-init
  gets it as an inherited fd; there is no socket file inside the zone. zone-init
  is not dumpable, so apps in the zone cannot trace it or take that fd.
- `/run/zone` inside is read-only: the `xdg-open` shim (`bin/`; its link part
  is a Python one-liner, or `appzones-helper open-link` with the Rust helper) and the
  host's `links.sock`, through which clicked links leave the zone, and
  `dns.sock`, through which DNS queries do.
- DNS (`network = "internet"`): the zone's `resolv.conf` names `127.0.0.53`,
  where zone-init answers (UDP and TCP; `_netsetup` allows port 53 in the
  zone's network). Each query goes over `dns.sock` to the zone's server on the
  host, which asks the host's resolver (`dns = "host"`: the first nameserver in
  the host's `/etc/resolv.conf`, also a loopback one such as systemd-resolved's
  `127.0.0.53`) or 1.1.1.1, then 1.0.0.1 (`dns = "public"`). Only standard
  queries, at most 4 KB, 64 at a time and 100 a second on average, only to
  that resolver. A resolver on the host or the LAN stays unreachable from the
  zone directly (the deny list refuses both); a public one like 1.1.1.1 is on
  the internet, which the zone reaches anyway. pasta does no DNS, so any pasta
  version works.

- `/run/user/1000` inside is an empty tmpfs that holds only the zone's
  sockets: `wayland-0`, `bus`, `zone-bus/` once an app uses the zone's own
  bus, and with `audio`, `pipewire-0` and `pulse/native`. The real bus, Hyprland's IPC, systemd, pueue, gnupg and
  the secrets never exist there.
- The host's programs are read-only: on NixOS `/nix/store` (and the
  system's links); on other distributions `/usr`, with `/bin`, `/sbin` and
  `/lib*` as the host has them (links into `/usr` on most). Which one is
  decided at start: NixOS when `/run/current-system` exists
  (`APPZONES_LAYOUT=nixos|fhs` overrides it). A curated `/etc` is bound, with
  the zone's own `machine-id`, `resolv.conf` and, for `dns = "public"`,
  `hosts`.
- `/tmp` is a tmpfs, or with `tmp = "disk"` the zone's `tmp/` (and
  `/var/tmp` its `var-tmp/`). `/mnt/host` is the zone's `mnt/`, read-only,
  where runtime mounts appear. On NixOS, `/lib64` exists only with `nix_ld`.
- `/dev` is bwrap's minimal one, plus the render node with `gpu`,
  `/dev/video*` with `camera = "device"`, and the named USB devices with
  `usb`. `/sys` holds only what those need and `/sys/devices/system/cpu`.
- The zone runs on `/run/current-system`, so apps are the host's versions.
- Portal calls are limited by method. Allowed: FileChooser, OpenURI,
  Notification, Settings, Print, Email, Trash, Inhibit, the monitors,
  GameMode, Realtime, Secret and GlobalShortcuts, plus ScreenCast and
  Screenshot with `screencast` and Camera with `camera`. RemoteDesktop,
  InputCapture, Background, DynamicLauncher and the Flatpak portal are
  never reachable.
