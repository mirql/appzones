-- appzones.lua: mark every window by the app zone its process runs in
-- (appzones docs/desktop.md; tested in tests/vm.nix).
--
-- The zone comes from what the kernel says about the window's pid, never from
-- the window's class, title or xdg_toplevel_tag, which the app chooses:
-- - its cgroup, .../appzones.slice/appzones@NAME.service (also a cgroup below it),
--   which systemd sets and a zone process cannot leave: the zone has its own
--   cgroup namespace and no cgroupfs;
-- - else a nested pid namespace (more than one NSpid in /proc/PID/status): a zone
--   started without systemd, or another sandbox, which appzones cannot name.
-- Fails closed: under appzones.slice but not parsable, in a nested pid namespace,
-- or unreadable, a window gets "zone:?" and the warning colour, never host styling.
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

local function slurp(path)
	local f = io.open(path, "r")
	if not f then
		return nil
	end
	local s = f:read("*a")
	f:close()
	return s
end

-- the zone's name, "?" for a window from a zone (or another sandbox) that cannot
-- be named, nil for a host window
local function zoneOf(pid)
	if not pid or pid <= 0 then
		return nil
	end
	local cgroup = slurp("/proc/" .. pid .. "/cgroup")
	local status = slurp("/proc/" .. pid .. "/status")
	if not cgroup or not status then
		return "?" -- gone already, or unreadable: cannot tell
	end
	for line in cgroup:gmatch("[^\n]+") do
		if line:find("/appzones.slice/", 1, true) then
			return line:match("/appzones%.slice/appzones@([%w-]+)%.service$")
				or line:match("/appzones%.slice/appzones@([%w-]+)%.service/")
				or "?"
		end
	end
	local nspid = status:match("\nNSpid:([^\n]*)")
	if nspid and nspid:match("%d+%s+%d+") then
		return "?" -- a process in a nested pid namespace
	end
	return nil
end

hl.on("window.open_early", function(w)
	local z = zoneOf(w.pid)
	if not z then
		return
	end
	local c = (z ~= "?" and colours()[z]) or fallback
	hl.dispatch(hl.dsp.window.tag({ window = w, tag = "+zone:" .. z }))
	-- 0.56 has no "border_color" prop; active and inactive are separate.
	hl.dispatch(hl.dsp.window.set_prop({ window = w, prop = "active_border_color", value = c }))
	hl.dispatch(hl.dsp.window.set_prop({ window = w, prop = "inactive_border_color", value = c }))
	hl.dispatch(hl.dsp.window.set_prop({ window = w, prop = "no_shortcuts_inhibit", value = "1" }))
end)
