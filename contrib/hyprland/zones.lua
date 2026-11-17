-- zones.lua: the pre-0.6 name of appzones.lua, kept so a Hyprland config
-- that loads it keeps working; point it at appzones.lua instead. A full copy,
-- not a loader: Nix may copy this file into the store on its own. Removed in
-- a later release; test_zonecfg checks the copy matches appzones.lua.
-- (appzones docs/guide.md "Hyprland"; tested in tests/vm.nix).
--
-- The zone comes from the host-side cgroup of the window's pid
-- (.../appzones.slice/appzones@NAME.service), which systemd sets and a zone process
-- cannot change: no cgroupfs is mounted inside a zone. Never from the
-- window's class, title or xdg_toplevel_tag, which the app chooses.
-- A zone window gets the tag "zone:NAME" (bars read it: `appzones active`), the
-- zone's border colour, and no_shortcuts_inhibit (zone clients can see
-- zwp_keyboard_shortcuts_inhibit_manager_v1; they never switch off the
-- compositor's shortcuts).
-- Colours: $XDG_RUNTIME_DIR/appzones/colours, "NAME #rrggbb" lines written by
-- `appzones` at every zone start and login.
local rt = os.getenv("XDG_RUNTIME_DIR") or "/run/user/1000"
local fallback = "rgb(f38ba8)"

local function colours()
	local t = {}
	local f = io.open(rt .. "/appzones/colours", "r")
	if not f then
		return t
	end
	for line in f:lines() do
		local z, c = line:match("^([%w-]+)%s+#(%x%x%x%x%x%x)$")
		if z then
			t[z] = "rgb(" .. c .. ")"
		end
	end
	f:close()
	return t
end

local function zoneOf(pid)
	if not pid or pid <= 0 then
		return nil
	end
	local f = io.open("/proc/" .. pid .. "/cgroup", "r")
	if not f then
		return nil
	end
	local line = f:read("*l")
	f:close()
	return line and line:match("/appzones%.slice/appzones@([%w-]+)%.service$")
end

hl.on("window.open_early", function(w)
	local z = zoneOf(w.pid)
	if not z then
		return
	end
	local c = colours()[z] or fallback
	hl.dispatch(hl.dsp.window.tag({ window = w, tag = "+zone:" .. z }))
	-- 0.56 has no "border_color" prop; active and inactive are separate.
	hl.dispatch(hl.dsp.window.set_prop({ window = w, prop = "active_border_color", value = c }))
	hl.dispatch(hl.dsp.window.set_prop({ window = w, prop = "inactive_border_color", value = c }))
	hl.dispatch(hl.dsp.window.set_prop({ window = w, prop = "no_shortcuts_inhibit", value = "1" }))
end)
