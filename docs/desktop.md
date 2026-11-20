# Desktop

## Hyprland

`contrib/hyprland/appzones.lua` (Hyprland's Lua config) tags every window
whose process runs in a zone with `zone:NAME` and gives it the zone's
border colour. The zone comes from the window's host-side cgroup
(`appzones@NAME.service`), which the app cannot change, never from its title or
class.

It fails closed: a window that comes from a zone but cannot be named gets the
tag `zone:?` and a red warning border, never the host's look. That is a zone
appzones started itself (without systemd there is no unit to name it by), a
process under `appzones.slice` that is not a zone's unit, and any process in
another pid namespace, so also a window from another sandbox such as Flatpak.
`appzones active` shows `[?]` for those.

Load it from your config:

```lua
dofile("/path/to/appzones/contrib/hyprland/appzones.lua")
```

For a bar: `appzones active --follow --json` prints Waybar custom-module JSON
(`[work]` while a zone window is focused):

```json
"custom/zone": { "exec": "appzones active --follow --json", "return-type": "json" }
```

## Sway

`contrib/sway/appzones-sway` (Python, standard library only) watches sway's
windows over its IPC and gives every window whose process runs in a zone the
mark `zone:NAME:ID` and the title prefix `[NAME]`, and switches off its
keyboard-shortcuts inhibitor. Sway has no per-window border colour; with
`show_marks yes` the mark shows in the title bar too.

The zone comes from what an app in a zone cannot change, never from its title
or app id: the window's security context (sway 1.10 and later report the app
id `dev.mirql.zone.NAME` that appzones' zone-wayland sets on the host), or the
process's cgroup (`appzones@NAME.service`, with systemd). If both are there they
must agree. Like the Hyprland hook it fails closed: a window from a zone it
cannot name, from another sandbox or from a nested pid namespace gets
`zone:?`. On sway 1.10 and later this names zones started without systemd
too; on sway 1.9 (Ubuntu 24.04) those get `zone:?`.

Start it from your sway config (the path of the package or `make install`):

```
exec /usr/share/appzones/contrib/sway/appzones-sway
```
