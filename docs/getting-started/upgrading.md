# Upgrading

## From 0.7

- **NixOS:** nothing to do. Running zones keep working; `appzones restart ZONE`
  gives them 0.8's DNS (the zone answers its own, on `127.0.0.53`).
- **Hyprland:** a window from a zone appzones cannot name (one started without
  systemd, or another sandbox such as Flatpak) gets a red border and the tag
  `zone:?` instead of looking like a host window. If you load `appzones.lua`
  from the package, reload Hyprland's config; if you copied it, copy the new
  one first (a copy of 0.7's still looks windows up the old way).
- **Ubuntu 24.04 and Debian 12** are new (preview): see [Install](install.md#ubuntu-and-debian-preview).

## From 0.5

0.6 keeps everything under the name appzones:

| before 0.6 | since 0.6 |
|---|---|
| `~/.config/zones/zones.toml` | `~/.config/appzones/zones.toml` |
| `~/.local/share/zones/` (zone homes) | `~/.local/share/appzones/` |
| `/etc/zones/denied-paths` | `/etc/appzones/denied-paths` (the module writes it) |
| `$ZONES_*` | `$APPZONES_*` |
| `contrib/hyprland/zones.lua` | `contrib/hyprland/appzones.lua` |

- **Zone homes move by themselves**: the first `appzones` command that runs
  while no zone runs renames `~/.local/share/zones` to
  `~/.local/share/appzones` and says so. Until then the old directory stays
  in use. If backups or an impermanence setup list the old path, update them.
- **The config does not move by itself** (it is often a link your dotfiles
  manage): `appzones` reads the old file and says to move it:
  `mkdir -p ~/.config/appzones && mv ~/.config/zones/zones.toml ~/.config/appzones/`.
- **After the switch**: `appzones stop --all`, then start your zones again
  (`appzones up NAME`, or log out and in). The first command after the stop
  moves the zone homes.
- The old variable names and `zones.lua` still work in 0.6 and will be
  removed later. The window tag (`zone:NAME`) and `zones.toml` keep their names.
