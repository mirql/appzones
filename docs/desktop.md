# Desktop

## Hyprland

`contrib/hyprland/appzones.lua` (Hyprland's Lua config) tags every window
whose process runs in a zone with `zone:NAME` and gives it the zone's
border colour. The zone comes from the window's host-side cgroup
(`appzones@NAME.service`), which the app cannot change, never from its title or
class. Load it from your config:

```lua
dofile("/path/to/appzones/contrib/hyprland/appzones.lua")
```

For a bar: `appzones active --follow --json` prints Waybar custom-module JSON
(`[work]` while a zone window is focused):

```json
"custom/zone": { "exec": "appzones active --follow --json", "return-type": "json" }
```
