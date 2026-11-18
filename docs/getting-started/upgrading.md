# Upgrading

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
