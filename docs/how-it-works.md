# How it works

```
systemd --user: appzones@work.service   (appzones.slice, Type=notify)
└─ appzones _supervise work                       host side
   ├─ zone-wayland      → wp_security_context_v1 listener (Hyprland hides
   │                      screencopy, data-control, layer-shell, virtual
   │                      input, global shortcuts, the window list)
   ├─ bwrap ─ xdg-dbus-proxy → the zone's filtered bus; the proxy's bwrap
   │                      carries /.flatpak-info, so portals see
   │                      app id dev.mirql.zone.work
   └─ pasta (own netns) ─ appzones _netsetup (nft deny list, then exec)
      └─ bwrap (user, pid, ipc, uts, cgroup ns; no caps; no_new_privs)
         └─ zone-init     the agent inside: run, shell (pty), ps
            └─ apps
```

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
