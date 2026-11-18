# How it works

```
systemd --user: appzones@work.service   (appzones.slice, Type=notify)
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
         └─ zone-init     the agent inside: run, shell (pty), ps
            └─ apps
```

- `appzones serve ZONE` is the zone itself, in the foreground: the systemd unit
  runs it, and so can any process manager or a terminal. It signals readiness
  over sd_notify, and with `--notify-fd N` as a newline on fd N (s6 style).
  SIGTERM or SIGINT stops the zone and its helpers. One server per zone: it
  holds `$XDG_RUNTIME_DIR/appzones/NAME.lock` (released when it exits); a
  second `serve` exits with status 75, "already served", and leaves the running
  zone untouched. With `--lock-fd N` a caller hands over a lock it took: fd N is
  inherited across `exec` (not close-on-exec), and the lock lives as long as any
  copy of it, so the caller closes its own (or `exec`s into `serve`).
- The zone ends with its server, however the server ends (also `kill -9`):
  zone-init holds a pidfd of the server (handed over a unix socket, since
  pasta closes inherited fds) and ends the zone when it becomes readable;
  zone-wayland gets SIGTERM when the server dies (`PR_SET_PDEATHSIG`); the
  D-Bus proxy is tied the same way. Waiting for zone-wayland's "ready", the
  proxy's bus, the pidfd hand-over and zone-init's answer each have a 30-second
  deadline; building `packages` comes before them and has none.
- The `appzones` CLI talks to zone-init over a socket the host makes,
  `$XDG_RUNTIME_DIR/appzones/NAME/agent.sock`, outside the zone. zone-init
  gets it as an inherited fd; there is no socket file inside the zone. zone-init
  is not dumpable, so apps in the zone cannot trace it or take that fd.
- `/run/zone` inside is read-only: the `xdg-open` shim (`bin/`) and the
  host's `links.sock`, through which clicked links leave the zone.

- `/run/user/1000` inside is an empty tmpfs that holds only the zone's
  sockets: `wayland-0`, `bus`, `zone-bus/` once an app uses the zone's own
  bus, and with `audio`, `pipewire-0` and `pulse/native`. The real bus, Hyprland's IPC, systemd, pueue, gnupg and
  the secrets never exist there.
- `/nix/store` is read-only. A curated `/etc` is bound, with the zone's own
  `machine-id`, `resolv.conf` and, for `dns = "public"`, `hosts`.
- `/tmp` is a tmpfs, or with `tmp = "disk"` the zone's `tmp/` (and
  `/var/tmp` its `var-tmp/`). `/mnt/host` is the zone's `mnt/`, read-only,
  where runtime mounts appear. `/lib64` exists only with `nix_ld`.
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
