-- zones.lua: the pre-0.6 name of appzones.lua. It loads appzones.lua from
-- this same directory, so a Hyprland config that loads zones.lua keeps
-- working; point it at appzones.lua instead. Removed in a later release.
local here = debug.getinfo(1, "S").source:match("^@(.*/)") or "./"
dofile(here .. "appzones.lua")
