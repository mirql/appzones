# D-Bus

A zone app gets one of three session buses. The default is `host`.

| `bus` | what the app talks to | for |
|---|---|---|
| `host` | the filtered proxy to the host bus (`$XDG_RUNTIME_DIR/bus`): the portals (file chooser, links, notifications, screencast), the tray, plus the zone's `dbus` lists | most apps: browsers, Electron, players |
| `zone` | the zone's own bus (`$XDG_RUNTIME_DIR/zone-bus/bus`), a `dbus-daemon` that zone-init starts inside the zone on first use. Every name on it exists only in this zone, and any app on it may own any name | single-instance apps: Thunar, gnome-text-editor, eog, most GTK and KDE apps that are "already running" the second time |
| `none` | nothing: the app runs in its own mount namespace (a nested bwrap) whose runtime dir has the Wayland and audio sockets but neither bus, without `DBUS_SESSION_BUS_ADDRESS`, and without zone-init's control socket | apps that work without a bus (a GApplication then runs as a plain, non-unique app). Needs `userns = true` |

**`bus` is routing, not a wall between apps of one zone.** It chooses
which bus an app talks to so that it works; it does not keep one app of a
zone from another. An app on `zone` could still open the zone's host-bus
socket; every app in a zone can ask zone-init to start another process in
it, and an app on `none` shares the zone's home, files and link socket with
apps that do have a bus. The boundary is the zone: put an app you do not
trust in its own zone.

**Media players (MPRIS).** A zone does not own `org.mpris.MediaPlayer2.*`
by default: owning a name means talking to it too, and that prefix is
shared by the host's players and every other zone's. To let the host's
media keys reach one player in a zone, grant its exact name, for example
`dbus = { own = ["org.mpris.MediaPlayer2.spotify"] }`. Two zones cannot own
overlapping names.

**Why single-instance apps fail on `host`.** Thunar, like every
GApplication, asks the bus for its own name (`org.xfce.Thunar`) when it
starts; the next launch finds the name taken and hands its arguments over.
The host proxy grants no such name, so the app prints `Failed to register`
and exits. Granting it there (`dbus = { own = [...] }`, as Flatpak does for
an app id) is not safe here, because the zone runs the host's own programs:

- owning a name lets the zone call that name too, and a call to a name the
  host can start (Thunar ships `org.xfce.Thunar.service`) starts the
  **host** Thunar, outside the zone, which the zone then drives:
  `org.xfce.FileManager.Execute` runs a program on the host;
- the host bus is one bus for all zones: Thunar in `untrusted` would hand
  its arguments to Thunar in `daily`, and the host's own Thunar to a zone's.

On the zone's own bus none of that exists. It is how the example
configuration runs Thunar:

```toml
[apps.thunar]
command = ["thunar"]
desktop = "thunar.desktop"
zones   = ["daily", "work", "untrusted"]
bus     = "zone"
```

`appzones run daily -- thunar` uses it too: the command matches the app.

**Firefox.** A Firefox launch first looks for a running Firefox on the
host bus, under `org.mozilla.<app>.<profile path>`, and hands its URL over.
Owning `org.mozilla.firefox.*` would let a zone reach the host's Firefox and
every other zone's that shares a profile path. So zone-init starts Firefox
(and `firefox-esr`, `-beta`, `-devedition`, `-nightly`, `librewolf`) with
`MOZ_DBUS_APP_NAME=firefox_zone_<zone>`, and the proxy lets each zone own
only `org.mozilla.firefox_zone_<zone>.*`. A second launch in a zone opens in
that zone's Firefox and never sees another zone's or the host's. The window
class stays `firefox`, and Firefox keeps `bus = "host"` (portals, screen
sharing). `org.mozilla.firefox*` is refused in `own`. A Firefox that another
app inside the zone starts directly (not through zone-init) gets the default
name, which the zone may not own: it runs without the remote, and a second
one hits the profile lock.

**What an app on `zone` gives up.** It does not use the host bus: no
portals (GTK falls back to its own file chooser, which shows the zone's
files, the same files the portal would let through), no notifications, no
tray icon, no media keys, and a link it opens through
GLib (the OpenURI portal) goes nowhere; `xdg-open` in the zone still
follows `links`. The zone bus starts services from the host's D-Bus service files when an
app calls them, inside the zone and as the zone: xfconfd keeps Thunar's
settings in the zone's `~/.config/xfce4`, dconf keeps GSettings in the
zone's `~/.config/dconf`, tumbler (if installed) makes thumbnails of the
zone's files. The portals, notification daemons, the tray watcher, keyrings
and the accessibility bus are never started there.

**Thunar in a zone.** Trash: deleting moves files to the zone's own
`~/.local/share/Trash`; the Trash view (`trash:///`) and network places need
gvfs, which zone apps do not load (`GIO_EXTRA_MODULES` is not passed in).
Thumbnails: only with tumbler installed on the host, made
inside the zone. Removable drives and the "open terminal here" action do
not reach outside the zone. Never `talk` to the host's gvfs, tumbler,
xfconfd or file manager: they act on host files with host rights (the
validator refuses them).
