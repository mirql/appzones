# First zones

```bash
appzones list                      # zones, state, network, dns, devices, apps running, colour
appzones list --json               # JSON array, one object per zone (for scripts)
appzones run daily firefox         # start the zone if needed, launch the app declared in [apps]
appzones run work -- chromium --incognito   # any command, not only declared apps
appzones run --restart work slack  # apply a changed zones.toml first, if no app runs in work
appzones run daily thunar          # a single-instance app: [apps.thunar] has bus = "zone"
appzones run --bus none daily -- foot   # once, with no D-Bus at all (see [D-Bus](../configuration/dbus.md))
appzones shell untrusted           # an interactive shell inside (the pty is in the zone)
appzones ps daily                  # processes in the zone
appzones logs daily                # its journal (app output is there, prefixed "[app pid]")
appzones logs -f daily             # follow it
appzones stop daily                # SIGTERM to every app in it (they save state), then the zone
appzones restart daily             # stop + start: applies zones.toml changes
appzones stop --all
appzones which work                # every setting in effect and where it came from
appzones which work --json         # the same as JSON (one object, for scripts)
appzones check                     # validate zones.toml
appzones check --probe untrusted   # also run isolation probes inside the zone
appzones new games                 # print a safe zone block; `-y` appends it to zones.toml
appzones apps                      # rewrite the launcher entries (also at every login)
appzones mount work ~/src/thing    # ~/src/thing is /mnt/host/thing in the running zone, now
appzones mount work ~/Music --ro --as music   # read-only, at /mnt/host/music
appzones mounts work               # what is mounted, from where, ro or rw
appzones umount work thing         # gone again (busy: says by what; --lazy detaches anyway)
```

- **Processes.** If the zone is stopped, `appzones ps ZONE` prints a
  start hint on stderr and exits with status 1.
- **The starting folder.** Run from a folder inside one of the zone's binds (also one reached
  through a symlink, as `~/src` linked elsewhere), `appzones run` starts the command in that
  folder in the zone; from anywhere else, in the zone's home.
- **Launcher.** Every app in `[apps]` gets a launcher entry per zone, named
  `Firefox (work)`. The entries are written to
  `~/.local/share/appzones/launcher/share/applications`, which is on
  `XDG_DATA_DIRS`. The host's own entries are never hidden.
- **Windows.** A zone window has the zone's colour as its border, and the
  Hyprland tag `zone:NAME`. Waybar shows `[work]` next to the workspaces
  when the active window belongs to a zone. The tag comes from the cgroup
  of the window's process, which the app cannot change. The app's title,
  class or own tags are never used.
- **Files.** A zone sees only its own home and the paths in its `binds`.
  Any other file reaches it through the file chooser: the dialog runs on
  the host, and the zone gets only the file you pick (document portal).
  `appzones run` refuses a path argument that is outside the zone's binds.
  For a while, without a restart: `appzones mount` (see [Runtime mounts](../configuration/files.md#runtime-mounts)).
- **Links.** With `links = "zone"` (`work`, `untrusted`), a link clicked in
  the zone opens in the same zone's browser. With `links = "portal"`
  (`daily`) it opens in the host's default browser.
- **Autostart.** A zone with `autostart = true` starts at login and then
  starts its `autostart_apps`.
- **When an app does not start.** `appzones run` watches the app for 2.5 s. If
  it exits with an error in that time (or with 0 after its D-Bus name was
  refused, as Thunar does), `appzones run` prints its exit status and its last
  lines of output, exits non-zero, and, when the app
  could not own its D-Bus name, says which `bus` setting fixes it. An app
  that hands over to an instance already running exits 0 and stays quiet.
  Everything an app prints is in `appzones logs ZONE`.

## Changing a zone

Edit `zones.toml`, then:

```bash
appzones check                 # what is wrong, if anything
appzones restart work          # a running zone keeps its old settings until then
```

`appzones run` warns when a running zone is older than the file.
