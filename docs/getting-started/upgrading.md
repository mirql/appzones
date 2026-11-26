# Upgrading

Each release's notes in the
[CHANGELOG](https://github.com/mirql/appzones/blob/main/CHANGELOG.md) start with what it
means for you. After updating, `appzones stop --all` (or `appzones restart ZONE` for
each running zone) gives running zones the new version; until then they keep the old one.
`appzones --version` says which version and config release you run.

What needs more than that:

## From 0.11 or earlier

- appzones is one Rust binary since 0.12: **restart running zones** after the update.
  Building from a checkout needs Rust 1.85 or newer, and `make install` puts the
  binaries in `PREFIX/libexec/appzones`.
- `wayland = "host"` (0.11.3) gives the host's socket only where the compositor has no
  security context (GNOME, Weston, sway before 1.9); on Hyprland, sway 1.9 or later or KDE
  the zone gets its own.

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

0.6 renamed everything to appzones. Since 0.15 the old names are not read any more:
appzones warns about each one it finds, with the command that moves it, and moves
nothing itself.

| before 0.6 | since 0.6 | what to do |
|---|---|---|
| `~/.config/zones/zones.toml` | `~/.config/appzones/zones.toml` | `mkdir -p ~/.config/appzones && mv ~/.config/zones/zones.toml ~/.config/appzones/` |
| `~/.local/share/zones/` (zone homes) | `~/.local/share/appzones/` | `appzones stop --all && mv ~/.local/share/zones ~/.local/share/appzones` |
| `/etc/zones/denied-paths` | `/etc/appzones/denied-paths` | NixOS: the module writes the new one. Elsewhere: `sudo mv` it. Until then no zone starts, as its paths would enter zones |
| `$ZONES_*` | `$APPZONES_*` | rename the variables |
| `contrib/hyprland/zones.lua` | `contrib/hyprland/appzones.lua` | load `appzones.lua` in your Hyprland config |

If backups or an impermanence setup list the old paths, update them. The window
tag (`zone:NAME`) and `zones.toml` keep their names.
