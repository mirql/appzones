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

`appzones serve` exits 0 when it is stopped (SIGTERM, SIGINT) or ends by
itself (`linger`), 75 when the zone already runs elsewhere, and something
else on a failure. The files restart a zone only on a failure.

Start the process manager from your graphical session (after the compositor),
so zones get `WAYLAND_DISPLAY` and the session bus: a zone needs both, and it
stops when the compositor goes away.
