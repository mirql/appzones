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
