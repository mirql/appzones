# contrib

- `hyprland/`: zone borders and labels on Hyprland, see
  [Desktop integration](../docs/desktop.md).
- Service files that run a zone as `appzones serve ZONE`, for when a process
  manager should run zones instead of appzones starting them on demand. See
  [Zones without systemd](../docs/without-systemd.md#a-process-manager-of-your-own).
  - `systemd/`: the user units the NixOS module installs, for other
    distributions.
  - `runit/appzones-ZONE/`, `s6/appzones-ZONE/`: a service directory; copy it
    once per zone as `appzones-NAME` (the zone's name comes from the directory's).
  - `dinit/appzones`: one file for every zone, started as `appzones@NAME`.

`appzones serve` exits 0 when it ends for good (SIGINT: `appzones stop`,
Ctrl-C; or `linger`), 143 when stopped by SIGTERM (the manager's own stop or
restart), 75 when the zone already runs elsewhere, and something else on a
failure. The files keep a zone down after 0 and 75 and let the manager decide
otherwise.

Start the process manager from your graphical session (after the compositor),
so zones get `WAYLAND_DISPLAY` and the session bus: a zone needs both, and it
stops when the compositor goes away.
