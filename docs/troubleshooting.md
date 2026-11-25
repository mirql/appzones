# Troubleshooting

First steps for any problem:

```bash
appzones list                 # is the zone running? which network, dns, devices?
appzones which ZONE           # every setting in effect, and where it came from
appzones logs ZONE            # the zone's journal (-f to follow)
appzones check --probe ZONE   # validate the config and run isolation probes inside
appzones --version            # the version and the config release (for a bug report)
```

| Symptom | Why | What to do |
|---|---|---|
| A site or name does not resolve in a zone, but works elsewhere | `dns = "host"` uses the host's resolver, with its ad blocking and blocklists (including `/etc/hosts`) | Check on the host: `getent hosts NAME`. Allow the name in the host's DNS, or set `dns = "public"` for that zone (1.1.1.1 or 1.0.0.1, and a minimal `/etc/hosts`; internal names then do not resolve) |
| No name resolves in a zone with `dns = "public"` (timeouts), but works with `dns = "host"` | A VPN's or a firewall's DNS leak protection lets DNS go only to its own resolver (Mullvad's does): the zone's queries to 1.1.1.1 and 1.0.0.1 are refused on the host. `appzones logs ZONE` says `names do not resolve: dns = "public": 1.1.1.1:53 and 1.0.0.1:53 refused on this host`, and `appzones check --probe ZONE` fails with the same | `dns = "host"` for that zone (the VPN's resolver, through the host's) |
| The LAN, the router, `localhost`, the host's own services or a tailnet are unreachable | By design: `network = "internet"` refuses all of them ([reference](configuration/reference.md)) | For a NetBird mesh: `netbird = true`. `network = "host"` gives the zone the host's whole network; think twice |
| `zone X cannot see FILE (not inside its binds)` | The file is outside the zone | [`appzones mount`](configuration/files.md#runtime-mounts), a bind, or the app's file chooser |
| `refused: ... never enters a zone` | The never-list (`~/.ssh`, shell config, `/run/secrets`, your `deniedPaths`, ...) | Pick a smaller folder that holds only what the zone needs |
| An app exits at once and `appzones run` prints its status and last output | Often a single-instance GTK or Electron app that cannot own its name on the host bus | Follow the hint: `bus = "zone"` for that app in `[apps]`, or `appzones run --bus zone` once ([D-Bus](configuration/dbus.md)) |
| A warning that `zones.toml` changed for a running zone | The zone still runs with the old settings | `appzones restart ZONE` (closes its apps), or `appzones run --restart` when no app runs in it |
| `required file not found` for a downloaded binary | Generic glibc binaries need a loader | `nix_ld = true`, with `programs.nix-ld.enable` on the host ([Generic binaries](configuration/nix-ld.md)) |
| Chromium or Electron apps refuse to start | Their sandbox needs nested user namespaces | `userns = true` (the default) |
| No sound, microphone, camera or screen sharing | Not in the zone's `devices` | Add `audio`, `mic`, `camera` or `screencast`, then restart the zone |
| A USB device is missing | Devices are bound at zone start ([Devices and USB](configuration/devices.md)) | Check `usb = [...]` matches `lsusb`, plug it in, restart the zone |
| An X11-only app, a Flatpak app, an AppImage | X11-only apps do not run yet; Flatpak apps need the host's Flatpak; AppImages need FUSE | Run Flatpaks on the host; AppImages with `--appimage-extract-and-run` |
| Desktop apps open no window; the log says "no security-context protocol" | The compositor has no `wp_security_context_v1` (GNOME's Mutter 46, Weston, sway before 1.9), so the zone has no Wayland socket | `wayland = "host"` for that zone (the host's socket, no security context: see the [reference](configuration/reference.md)), or a compositor with it: Hyprland, sway 1.9+, others on wlroots 0.17+, KDE Plasma 6 |
| A window has no zone border colour | Borders and tags need Hyprland and the appzones hook | See [Desktop](desktop.md) |
| A zone does not start at all | Often missing nftables modules, or an invalid config | `appzones check`, `appzones logs ZONE`; see [Install](getting-started/install.md) |
| "nft not found" (or bwrap, pasta, ...) | appzones looks for its helpers on `PATH`, then in `/usr/sbin` and `/sbin` | Install the package that has it, or point appzones at it: `export APPZONES_BIN_NFT=/path/to/nft` (`APPZONES_BIN_<NAME>` for any helper; for zones that systemd starts, also in the user manager: `systemctl --user set-environment`) |

Still stuck? Ask in [Discussions](https://github.com/mirql/appzones/discussions/categories/q-a), or open an [issue](https://github.com/mirql/appzones/issues/new/choose) with `appzones --version`, `appzones check` output and the relevant part of your `zones.toml`.
