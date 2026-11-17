# Changelog

The config schema has its own release number (`requires = N` in
`zones.toml`, `RELEASE` in `src/zonecfg.py`), separate from the version.

## Unreleased

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
