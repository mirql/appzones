# appzones guide

An app zone is a long-lived sandbox that desktop apps run in. Each
zone has its own home directory, its own network rules and its own
colour. Apps in one zone cannot see the host home, other zones, or the
host's sockets. Zones are declared in one TOML file and run as systemd
user units.

The example configuration (`examples/zones.toml`) declares three:

| zone | colour | for | network | sound |
|---|---|---|---|---|
| `daily` | green `#a6e3a1` | messengers, personal browsing, media | internet | yes |
| `work` | blue `#89b4fa` | work browser, Slack, VS Code | internet + NetBird mesh | yes |
| `untrusted` | red `#f38ba8` | links and files you do not trust | internet, public DNS | no |

Every zone starts with an empty home. No app data, profile or login is
copied in from the host: you sign in to each app again inside the zone.
Only your look is copied at each start (GTK/Qt settings, fonts, icons,
cursor; `theme = false` turns it off). Host apps, their profiles and their
launcher entries stay as they are.

## Install (NixOS)

```nix
# flake.nix
inputs.appzones.url = "github:mirql/appzones";
inputs.appzones.inputs.nixpkgs.follows = "nixpkgs";

# in your NixOS configuration
imports = [ inputs.appzones.nixosModules.default ];
programs.appzones = {
  enable = true;
  user = "alice";                  # only this user's manager runs zones (null: everyone)
  deniedPaths = [ "/srv/code" ];   # your own never-list additions
  netbirdInterface = "wt0";        # only matters for netbird = true
};
```

Then write `~/.config/zones/zones.toml`: copy `examples/minimal.toml`
(works on any host), or run `appzones new NAME -y` (creates the file if there
is none). `examples/zones.toml` shows every key; parts of it need more setup
(`nix_ld` needs `programs.nix-ld.enable`, binds need the folders to exist).
Run `appzones check`.

Requirements:

- x86_64 Linux, NixOS (the module; the tool expects `/run/current-system`).
- A Wayland compositor with `wp_security_context_v1` (Hyprland, sway,
  KWin and others): every zone gets its own security context, and a zone
  does not start without it. Window tags and border colours need Hyprland
  0.56 or later with its Lua config (see "Hyprland").
- xdg-desktop-portal, PipeWire for `audio`, unprivileged user namespaces,
  nftables kernel modules (see "Kernel modules").
- For `dns = "host"`: the resolver is the first nameserver in
  `/etc/resolv.conf` (systemd-resolved, dnsmasq, ...), or
  `programs.appzones.dnsServer`.

Zone apps get the host session's `XDG_CURRENT_DESKTOP`.

## Hyprland

`contrib/hyprland/zones.lua` (Hyprland's Lua config) tags every window
whose process runs in a zone with `zone:NAME` and gives it the zone's
border colour. The zone comes from the window's host-side cgroup
(`appzones@NAME.service`), which the app cannot change, never from its title or
class. Load it from your config:

```lua
dofile("/path/to/appzones/contrib/hyprland/zones.lua")
```

For a bar: `appzones active --follow --json` prints Waybar custom-module JSON
(`[work]` while a zone window is focused):

```json
"custom/zone": { "exec": "appzones active --follow --json", "return-type": "json" }
```

## Kernel modules

A zone loads its nft deny list inside an unprivileged user namespace,
where the kernel does not autoload modules. A host with an nftables
firewall has them loaded already; otherwise load them yourself:

```nix
boot.kernelModules = [ "nf_tables" "nft_reject" "nft_reject_inet" "nf_reject_ipv4" "nf_reject_ipv6" ];
```

## Using it

```bash
appzones list                      # zones, state, network, dns, devices, apps running, colour
appzones run daily firefox         # start the zone if needed, launch the app declared in [apps]
appzones run work -- chromium --incognito   # any command, not only declared apps
appzones run --restart work slack  # apply a changed zones.toml first, if no app runs in work
appzones run daily thunar          # a single-instance app: [apps.thunar] has bus = "zone"
appzones run --bus none daily -- foot   # once, with no D-Bus at all (see "D-Bus" below)
appzones shell untrusted           # an interactive shell inside (the pty is in the zone)
appzones ps daily                  # processes in the zone
appzones logs daily                # its journal (app output is there, prefixed "[app pid]")
appzones logs -f daily             # follow it
appzones stop daily                # SIGTERM to every app in it (they save state), then the zone
appzones restart daily             # stop + start: applies zones.toml changes
appzones stop --all
appzones which work                # every setting in effect and where it came from
appzones check                     # validate zones.toml
appzones check --probe untrusted   # also run isolation probes inside the zone
appzones new games                 # print a safe zone block; `-y` appends it to zones.toml
appzones apps                      # rewrite the launcher entries (also at every login)
appzones mount work ~/src/thing    # ~/src/thing is /mnt/host/thing in the running zone, now
appzones mount work ~/Music --ro --as music   # read-only, at /mnt/host/music
appzones mounts work               # what is mounted, from where, ro or rw
appzones umount work thing         # gone again (busy: says by what; --lazy detaches anyway)
```

- **Launcher.** Every app in `[apps]` gets a launcher entry per zone, named
  `Firefox (work)`. The entries are written to
  `~/.local/share/zones/launcher/share/applications`, which is on
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
  For a while, without a restart: `appzones mount` (see "Runtime mounts").
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

### Changing a zone

Edit `zones.toml`, then:

```bash
appzones check                 # what is wrong, if anything
appzones restart work          # a running zone keeps its old settings until then
```

`appzones run` warns when a running zone is older than the file.

## NVIDIA

On a laptop with an Intel iGPU and an NVIDIA dGPU (PRIME offload,
proprietary driver), `gpu` binds every `/dev/dri/renderD*`, but
never `/dev/nvidiactl`, `/dev/nvidia0`, `/dev/nvidia-modeset` or
`/dev/nvidia-uvm`, which the proprietary driver needs next to its render
node. A zone also starts with a clean environment, so the host's
`__NV_PRIME_RENDER_OFFLOAD` / `__GLX_VENDOR_LIBRARY_NAME=nvidia` do not
reach it. Zone apps therefore render on the Intel iGPU through Mesa
(glvnd's NVIDIA EGL vendor and NVIDIA's Vulkan ICD fail to open their
device and the loader moves on to Mesa). `prime-run`, CUDA and NVENC do not
work in a zone. Giving a zone the NVIDIA nodes would expose the proprietary
driver's whole ioctl surface (and CUDA's UVM) to it, so it is not done.

## Configuration reference

File: `~/.config/zones/zones.toml` (`$ZONES_CONFIG` overrides it). Every
key below can sit in `[defaults]` or in a `[zones.NAME]` table; a zone
inherits from `[defaults]`, and `[defaults]` from the built-in values.
Unknown keys are errors.

**`requires = N`** (top level) names the zones release the file needs. A
`zones.toml` kept in a dotfiles repo can change before the installed `appzones`
tool does. An `appzones` older than the file then says "needs zones release N,
this `appzones` is release M: update appzones (and rebuild)" and lists what it does not know, instead of
failing on an unknown key. A file that uses a newer setting without raising
`requires` is an error, so the field stays true. Release 2 added `bus` and
`requires`; release 3 `nix_ld`, `tmp` and `appzones mount`; release 4 the `usb`
device and key; `appzones check` prints
the tool's own release. (A `zone` (the old name) from
before release 2 does not know `requires` either: it still says "unknown
key" until it is updated once.) A real typo stays an error, with the nearest
known key suggested.

| key | values (built-in default first) | what it does |
|---|---|---|
| `colour` | `"#rrggbb"` (required, unique) | border colour and bar label |
| `network` | `"internet"` · `"none"` · `"host"` | `internet`: own network namespace (pasta) plus a deny list loaded inside it. The deny list blocks every address of the host, every network the host is directly connected to (a LAN with global IPv6 or public IPv4 addresses too), private ranges, loopback, CGNAT (tailnet, NetBird), link-local and multicast. It is computed when the zone starts: after the host joins another network, restart the zone. `none`: loopback only. `host`: the host's whole network, including localhost (warned; refused for a zone named `untrusted` without `force_host`) |
| `netbird` | `false` · `true` | with `internet`: also reach the NetBird mesh (the networks of the host's NetBird interface, `programs.appzones.netbirdInterface`), except the host's own addresses. Services see the traffic as coming from the host |
| `dns` | `"host"` · `"public"` | `host`: the host's resolver (the first nameserver in `/etc/resolv.conf`, or `programs.appzones.dnsServer`; its ad blocking and internal names). `public`: 1.1.1.1 and a minimal `/etc/hosts` |
| `links` | `"portal"` · `"zone"` · `"zone:NAME"` | where clicked http(s) links open: the host's browser, this zone's browser, or zone NAME's browser (the first browser in `[apps]` for that zone) |
| `keyring` | `"basic"` · `"zone"` | `basic`: browsers use their fixed-key store, protected only by disk encryption. `zone` is not built yet (warned, runs as `basic`) |
| `devices` | list of `gpu` `audio` `mic` `camera` `screencast` `usb` (built-in `["gpu", "audio"]`) | `gpu`: the render node only (not the KMS node) and its sysfs. `audio`: the PipeWire and PulseAudio sockets. `mic` is the same as `audio` for now. `camera`: the Camera portal, or `/dev/video*` with `camera = "device"`. `screencast`: the ScreenCast and Screenshot portals (each use asks you). `usb`: the USB devices named in `usb`, see "USB devices" |
| `camera` | `"portal"` · `"device"` | how `camera` is given |
| `usb` | `[]` | with `usb` in `devices`: `["vvvv"]` (a vendor) or `["vvvv:pppp"]` (vendor and product), lowercase hex as `lsusb` prints them. Required (non-empty) with the device; set in a zone without the device it is an error |
| `x11` | `"none"` · `"own"` | `own` (xwayland-satellite) is not built yet. The host's Xwayland is never shared |
| `clipboard` | `"both"` | copy and paste both ways, through the focused window |
| `autostart` | `false` · `true` | start the zone at login |
| `autostart_apps` | `[]` | app ids from `[apps]` started with the zone |
| `userns` | `true` · `false` | nested user namespaces, which browser and Electron sandboxes need. `false`: Chromium and Electron refuse to start, and Firefox runs a weaker sandbox without saying so (warned) |
| `machine_id` | `"zone"` · `"host"` | `zone`: a random id, kept, so apps cannot link the zone to the host or to other zones |
| `theme` | `true` · `false` | copy GTK, Qt, font, icon and cursor settings in at each start. Copied, not bound, because they are often links into a dotfiles checkout |
| `packages` | `[]` | extra nixpkgs attributes, built at zone start and put first on `PATH` |
| `env` | `{}` | extra environment. Names on the never-list are refused (`*TOKEN*`, `*KEY*`, `SSH_AUTH_SOCK`, `HYPRLAND_*`, `LD_PRELOAD`, …) |
| `binds` | `[]` | `{ path = "~/Downloads", mode = "rw" \| "ro" }`, at the same path inside |
| `dbus` | `{ talk = [], own = [] }` | names on the host bus proxy added to the safe default set (see "D-Bus" below for what is refused) |
| `bus` | `"host"` · `"zone"` · `"none"` | the session bus for a command that is not in `[apps]` (`appzones run Z -- CMD`). See "D-Bus" below |
| `nix_ld` | `false` · `true` | generic Linux binaries run: `/lib64/ld-linux-x86-64.so.2` in the zone is the host's nix-ld shim, and `NIX_LD`, `NIX_LD_LIBRARY_PATH` are set as on the host. Refused on a host without nix-ld (`programs.nix-ld.enable`). See "Generic Linux binaries" |
| `tmp` | `"ram"` · `"disk"` | `ram`: `/tmp` is a fresh tmpfs at every start. `disk`: `/tmp` and `/var/tmp` are `~/.local/share/zones/NAME/tmp` and `.../var-tmp` (0700): kept across restarts, readable from the host. Cleaned at each start, see "/tmp on disk" |
| `home` | `""` | the zone's home; empty: `~/.local/share/zones/NAME/home` |
| `force_host` | `false` · `true` | allow `network = "host"` in `untrusted` |

`[apps.ID]` (ID: lowercase, as zone names):

| key | what |
|---|---|
| `command` | argv, e.g. `["mpv", "--player-operation-mode=pseudo-gui"]` |
| `zones` | the zones it gets a launcher entry in |
| `args` | extra arguments per zone, `{ untrusted = ["--private-window"] }` |
| `desktop` | a host `.desktop` file to take the name, icon and MIME types from |
| `bus` | `"host"` · `"zone"` · `"none"`: the session bus this app gets; default: the zone's `bus`. `appzones run Z -- CMD` uses it too when `CMD` is this app's command (by name) |

**Never inside a zone, whatever the TOML says.** The validator compares
resolved paths (symlinks anywhere on the path, also for a `home` that does
not exist yet), so a symlink cannot get around it. When a zone starts, the
supervisor opens each bind, checks what it actually opened, and bwrap
mounts that same descriptor: swapping in a symlink after the check changes
nothing. Only directories and regular files are bound, never a socket, a
FIFO or a device. It refuses a bind or `home` that is, contains or is
inside any of these:

- `/`, `/home`, `~`, `/nix`, `/etc`, `/var`, `/run`, `/tmp`, `/proc`,
  `/sys`, `/dev` whole, and anything inside `/proc`, `/sys`, `/dev`,
  `/run`, `/tmp`, `/etc`, `/nix`, `/boot`, `/root` or `/var` (sockets such
  as the system bus, devices, other users' files);
- `/run/user`, `/run/secrets`, `/var/lib/sops-nix`, `/etc/ssh`,
  `/nix/var`;
- the host's own additions: `/etc/zones/denied-paths`, one path per line,
  `~/` for the home (`programs.appzones.deniedPaths`);
- `~/.ssh`, `~/.gnupg`, `~/.config/sops`, `~/.password-store`, the keyrings
  and kwallet;
- opencode and claude state;
- everything the host runs or reads at login: `~/.config/{zones,hypr,
  systemd,autostart,environment.d,uwsm,nix,git,fish}`,
  `~/.local/share/{zones,applications}`, `~/.local/bin`, `~/.bin`, and the
  shell rc files.

A bound directory brings everything in it, also what appears there later:
a socket an app on the host creates inside a shared folder (an agent, a
control socket) is reachable from the zone, read-only bind or not. Share
folders with documents, not folders where programs keep their sockets.

On the host bus, these names are never granted (`talk` or `own`):
`systemd1`, `secrets`, `kwalletd*`, `Flatpak` and `portal.Flatpak`,
`impl.portal.*`, `dconf`, `kdeconnect*`, `login1`, `klauncher`, `KWin`, the
session managers and shells, the host's file and settings daemons
(`org.gtk.vfs.*`, `org.freedesktop.thumbnails.*`, `org.xfce.Xfconf`,
`org.freedesktop.FileManager1`, `org.xfce.FileManager`, `org.xfce.Thunar`),
`*` and `org.*`. On top of that:

- `own` never takes `org.freedesktop.*`, `org.kde.*`, `org.gnome.*`,
  `org.xfce.*`, `org.a11y.*`, `com.canonical.*` or the Mozilla apps'
  `org.mozilla.firefox*` (see "Firefox" below), and a wildcard needs at
  least three labels before `.*` (`org.example.player.*`, not `org.example.*`);
- neither `talk` nor `own` takes a name that a host `.service` file provides
  (the validator asks the running session bus which names it can start;
  if it cannot ask, it grants nothing extra): a call to it
  would start that program on the host, outside the zone, for the zone to
  drive;
- a name in `own` of two zones is an error: the app in the zone that starts
  second would hand its work to the first, and a zone never hands anything
  to another zone.

## D-Bus

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

## Generic Linux binaries (`nix_ld`)

A binary built for other distributions (a release download, an agent
CLI) asks for the loader `/lib64/ld-linux-x86-64.so.2`. On a NixOS host
with nix-ld that path is nix-ld's shim, which runs the binary with the loader and libraries
from `NIX_LD` and `NIX_LD_LIBRARY_PATH`. A zone has no `/lib64` and starts
with a clean environment, so the same binary fails there with "cannot
execute: required file not found". With `nix_ld = true` the zone gets
`/lib64/ld-linux-x86-64.so.2` (a link to the host shim in `/nix/store`) and
the two variables, pointing at `/run/current-system/sw/share/nix-ld/lib`.
The libraries are the host's `programs.nix-ld.libraries`. Nothing else is
shared: no `LD_LIBRARY_PATH`, which stays on the env never-list. The
example configuration turns it on in `work` and keeps it off in
`untrusted`.

## USB devices (`usb`)

```toml
[zones.gadgets]
devices = ["gpu", "audio", "usb"]
usb     = ["abcd"]          # every device of vendor abcd; "abcd:1234" for one product
```

At each zone start, `appzones` reads `/sys/bus/usb/devices` and, for every
device whose `idVendor` (and `idProduct`, if given) matches, binds:

- its node `/dev/bus/usb/BBB/DDD` (from `busnum`/`devnum`, checked against
  the device's `dev` MAJ:MIN), for libusb;
- every `/dev/hidrawN` whose `/sys/class/hidraw/hidrawN/device` lies below
  that device, for hidapi and hidraw readers;
- the device's own sysfs directory (its realpath, read-only), plus
  `/sys/bus/usb` and, with a hidraw node, `/sys/class/hidraw` (listings
  for enumeration: their links to other devices dangle in the zone), and
  `/sys/dev/char`;
- udev's records of those nodes, `/run/udev/data/c189:N` and
  `c243:N` (read-only), which libusb and hidapi built with libudev read.

Never all of `/dev/bus/usb` or `/dev/hidraw*` (a YubiKey sits next to
them), and never `/dev/uinput`: it would let the zone type into the host
session. A zone without `usb` has no `/dev/bus/usb`, no hidraw node and no
`/sys/bus/usb`.

- **Bound at start.** A device plugged in later, or replugged (its device
  number changes), is not in the zone: `appzones restart ZONE`. With no
  matching device at start the zone warns (`appzones logs ZONE`) and starts
  without one.
- **Permissions are the host's.** The zone opens the nodes as you. A USB
  node is `root 0664` and a hidraw node `root 0600` unless a udev rule
  gives the seat user access, e.g. on NixOS:
  `services.udev.extraRules = ''SUBSYSTEMS=="usb", ATTRS{idVendor}=="abcd", TAG+="uaccess"'';`
- **No hotplug.** A device that arrives later has no node in the zone,
  whatever libusb's hotplug callback says (with `network` `internet` or
  `none` udev's netlink events do not even reach the zone's own network
  namespace).

## /tmp on disk (`tmp = "disk"`)

`/tmp` in the zone is `~/.local/share/zones/NAME/tmp`, and `/var/tmp` is
`~/.local/share/zones/NAME/var-tmp`. Both are 0700 and survive `zone
restart` and reboots, and you can reach them from the host. The zone wrote
everything in them, so do not open links or run files from them on the
host without care.

Cleanup at every zone start: files not read or written for 14 days
(`/tmp`) or 30 days (`/var/tmp`) are removed, then directories that are
that old and empty. Times are atime and mtime; on a `/home` mounted
`noatime` that means "not written". Symlinks are removed
as links and never followed. `ram` (the default) keeps the old behaviour:
a fresh tmpfs at each start, and no `/var/tmp`.

## Runtime mounts

```bash
appzones mount ZONE HOST-DIR [--ro] [--as NAME]   # → /mnt/host/NAME (NAME: HOST-DIR's name)
appzones umount ZONE NAME [--lazy]
appzones mounts ZONE
```

A host folder appears at `/mnt/host/NAME` in a **running** zone without a
restart, and `appzones umount` takes it away again. A mount lasts until
`umount` or until the zone stops or restarts: the zone removes every mount
left at its start and stop. To keep a folder for good, add it to `binds`.

- **Same never-list as `binds`**, on the resolved path, so a symlink cannot
  get around it: a folder that is, contains or is inside a denied path is
  refused (`~/.ssh`, `~`, the keys, agent state, your `deniedPaths`, …). A
  runtime mount also never comes from inside `/proc`, `/sys`, `/dev`,
  `/run` (sockets, `/run/user/1000`), `/etc`, `/nix`, `/boot`, `/root`,
  `/var` or `/tmp`. Only directories.
- **Busy.** If an app in the zone has its working directory, a file or a
  mapping under it, `appzones umount` refuses and lists the processes. `--lazy`
  detaches it at once; files still open keep working until they close.
- Always `nosuid` and `nodev`. With `--ro` the mount is read-only from the
  moment the zone sees it, and the zone cannot remount it writable, not
  even from a nested user namespace.

**How.** Each zone binds `~/.local/share/zones/NAME/mnt` read-only at
`/mnt/host`. bwrap makes the zone's mounts slaves of the host's, and
`/home` is normally a shared mount (systemd makes `/` shared), so a mount the host makes under that
directory appears in the zone at once, and an unmount disappears. The zone
cannot create anything in `/mnt/host` itself. A bind mount on the host
needs root, so `appzones` calls one helper, `zones-mount-helper`, through
`sudo -n`. The NixOS module adds a sudo rule for exactly that file, for
`programs.appzones.user`, without a password (`mountHelper = true`).

**Why sudo, and why it cannot be abused.** The other choice was a
root system service with a socket. That would be one more daemon, and
it would have to find out who is calling (`SO_PEERCRED`) and which mount
namespace they are in. A sudo rule for one file is less code, sudo logs
every call, and the helper always runs in the caller's own namespace.
What the helper accepts:

- **Who.** The caller is `SUDO_UID`, set by sudo. Root itself is refused,
  and so is running it without sudo. A zone cannot run sudo at all:
  `no_new_privs` turns setuid off, and `/run/wrappers` is not in a zone.
- **What.** It opens the source **as the caller** (its uid, gid and
  groups), so the source must be a directory the caller can already reach
  and read. A root-only directory, or one inside a 0700 directory, is
  refused. The never-list is checked on the path of that open directory,
  and the mount is made from the same descriptor (`open_tree`), so nothing
  can be swapped in between. Mounts below the source do not come along.
- **Where.** Only `<caller's home>/.local/share/zones/ZONE/mnt/NAME`. ZONE
  and NAME are plain names (no `/`, no `..`). The target is opened with
  `openat2(RESOLVE_NO_SYMLINKS)`, so a symlink anywhere on the path is
  refused. It must be a directory the caller owns and not already a mount
  point. The mount is attached to that descriptor (`move_mount`), and an
  unmount goes through the descriptor of the caller's own `mnt`
  directory, so no path is walked twice.
- `--ro` is set on the detached mount (`mount_setattr`) before it is
  attached, so there is no moment when the zone sees it writable.

So the most a caller can do with the helper is put a directory it can
already read onto a directory it owns. It can do that with no helper at
all by copying, and the never-list still applies. The narrow rule matters
for the zone (which can never use sudo), for the user unit (which calls the
helper at a zone's start and stop), and for a `wheel` that needs a
password.

## How a zone is built

```
systemd --user: appzones@work.service   (appzones.slice, Type=notify)
└─ appzones _supervise work                       host side
   ├─ zone-wayland      → wp_security_context_v1 listener (Hyprland hides
   │                      screencopy, data-control, layer-shell, virtual
   │                      input, global shortcuts, the window list)
   ├─ bwrap ─ xdg-dbus-proxy → the zone's filtered bus; the proxy's bwrap
   │                      carries /.flatpak-info, so portals see
   │                      app id dev.mirql.zone.work
   └─ pasta (own netns) ─ appzones _netsetup (nft deny list, then exec)
      └─ bwrap (user, pid, ipc, uts, cgroup ns; no caps; no_new_privs)
         └─ zone-init     the agent inside: run, shell (pty), ps
            └─ apps
```

- `/run/user/1000` inside is an empty tmpfs that holds only the zone's
  sockets: `wayland-0`, `bus`, `zone-bus/` once an app uses the zone's own
  bus, and with `audio`, `pipewire-0` and `pulse/native`. The real bus, Hyprland's IPC, systemd, pueue, gnupg and
  the secrets never exist there.
- `/nix/store` is read-only. A curated `/etc` is bound, with the zone's own
  `machine-id`, `resolv.conf` and, for `dns = "public"`, `hosts`.
- `/tmp` is a tmpfs, or with `tmp = "disk"` the zone's `tmp/` (and
  `/var/tmp` its `var-tmp/`). `/mnt/host` is the zone's `mnt/`, read-only,
  where runtime mounts appear. `/lib64` exists only with `nix_ld`.
- `/dev` is bwrap's minimal one, plus the render node with `gpu`,
  `/dev/video*` with `camera = "device"`, and the named USB devices with
  `usb`. `/sys` holds only what those need and `/sys/devices/system/cpu`.
- The zone runs on `/run/current-system`, so apps are the host's versions.
- Portal calls are limited by method. Allowed: FileChooser, OpenURI,
  Notification, Settings, Print, Email, Trash, Inhibit, the monitors,
  GameMode, Realtime, Secret and GlobalShortcuts, plus ScreenCast and
  Screenshot with `screencast` and Camera with `camera`. RemoteDesktop,
  InputCapture, Background, DynamicLauncher and the Flatpak portal are
  never reachable.

## Honest limits

1. **Same uid, same kernel.** A kernel bug reachable from inside escapes
   everything, and an escape lands as your user, with your files, keys and
   sudo. Namespaces, nested user namespaces, the GPU driver and nft in the
   namespace all widen what the kernel exposes. A seccomp filter is
   planned; a separate uid or a microVM back end is later.
2. **A compromised app in a zone can still:**
   - read and write everything in its zone: its home, its binds
     (`~/Downloads` in `daily`), and the data of every other app in that
     zone;
   - read the clipboard by mapping a window (Hyprland focuses new
     windows), and set it at any time;
   - draw windows that look like anything. The border colour is the
     defence, so learn the colours;
   - send notifications that look like system ones, and see when you are
     idle;
   - with `audio` (and `mic`): record the microphone and other apps' sound,
     for now;
   - with `gpu`: talk to the GPU driver;
   - use the internet as the host (same public IP), and with `netbird` the
     mesh as the host;
   - reach a network the host joined after the zone started, until the
     zone restarts (the deny list is computed at start);
   - resolve internal DNS names (with `dns = "host"`);
   - ask for a screencast or a file. That is your dialog, every time.
3. **Logins inside zones use the basic store.** Browsers and Electron apps
   keep cookies and passwords under a fixed key, protected only by disk
   encryption.
4. **X11-only apps do not start** until `x11 = "own"` is built.
5. **Flatpak apps do not run in a zone** (they need the host's Flatpak
   service). Run them on the host: they have their own sandbox.
6. **AppImages need `--appimage-extract-and-run`** (FUSE needs setuid,
   which `no_new_privs` disables).
7. **Telegram's tray icon, Slack's screen share and the KDE file
   dialog** are not covered by the VM test.
8. **An app on the zone's own bus has no portals** (see "D-Bus"): no
   notifications, tray or screen sharing from it.

## Testing

Nothing is ever tested on the live desktop.

```bash
just test           # validator unit tests (no Nix)
just check FILE     # validate a zones.toml
just build          # the package (runs the unit tests too)
just test-vm        # NixOS VM: Hyprland, zone units, isolation, window tags (~6 min, /dev/kvm)
```

The VM test (`tests/vm.nix`, `checks.x86_64-linux.vm`) runs Hyprland on a
virtual GPU. It covers:

- the units: start, stop, restart, crash (the unit restarts) and
  autostart;
- every CLI command;
- isolation probes: the zone home, `~/.ssh`, `/srv/code` (a `deniedPaths` entry), the
  runtime dir, capabilities, the environment and the machine-id;
- the network: host loopback, a tailnet-like host address, the host's own
  IPv4 and IPv6 addresses, its connected network, a private LAN and a LAN
  with global IPv6 addresses are refused, the routed "internet" peer is
  reachable, and nft cannot be removed from inside;
- D-Bus: systemd is refused, the Settings portal answers, RemoteDesktop is
  denied, owning `org.mpris.MediaPlayer2.*` is refused;
- single-instance apps on the zone's own bus: Thunar shows a window and a
  second launch reuses it, `org.xfce.Thunar` exists on the zone bus and
  never on the host's, gnome-text-editor and eog start; on the host bus
  Thunar exits and `appzones run` prints its status, output and the bus hint;
  `bus = "none"` leaves no bus socket and no zone-init control socket;
  `appzones run --wait` refuses a socket or directory as stdin; the validator refuses owning
  `org.freedesktop.Notifications` and `org.xfce.Thunar`; `appzones run` reports
  an app that exits early; `appzones logs -f`;
- Wayland: privileged globals are hidden from zone clients; zone windows
  get the tag and colour, from the CLI and from a launcher entry; a host
  window gets neither;
- Firefox in a zone with its content processes under seccomp in their own
  user namespace; Firefox in two zones at once, each owning only its zone's
  remote name, and a second launch in one zone opening in that zone's
  Firefox, never the other's;
- audio sockets only with `audio`;
- an FHS-wrapped app (`buildFHSEnv`) running inside a zone;
- `nix_ld`: a generic glibc binary (loader `/lib64/ld-linux-x86-64.so.2`,
  no RPATH) runs with `nix_ld = true` and fails with "required file not
  found" without it; the validator refuses `nix_ld` on a host without
  nix-ld;
- `tmp = "disk"`: files in `/tmp` and `/var/tmp` survive `appzones restart`,
  are on the host in 0700 directories, and old files and links are
  removed (never followed);
- runtime mounts: mount, `mounts`, umount; read-only stays read-only (also
  against a remount from a nested user namespace); a busy umount names the
  process; `--lazy`; the never-list is refused, also through a symlink; the
  helper refuses a target outside `zones/*/mnt` (a symlink there, or for
  `mnt` itself), bad names, a directory the user cannot reach, running
  without sudo or as root; sudo allows the user nothing else; a zone cannot
  use sudo; a zone restart leaves no mount behind.
