# Configuration reference

File: `~/.config/appzones/zones.toml` (`$APPZONES_CONFIG` overrides it). Every
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
device and key; release 5 `supervisor`, `start_timeout` and `linger`; `appzones check` prints
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
| `devices` | list of `gpu` `audio` `mic` `camera` `screencast` `usb` (built-in `["gpu", "audio"]`) | `gpu`: the render node only (not the KMS node) and its sysfs. `audio`: the PipeWire and PulseAudio sockets. `mic` is the same as `audio` for now. `camera`: the Camera portal, or `/dev/video*` with `camera = "device"`. `screencast`: the ScreenCast and Screenshot portals (each use asks you). `usb`: the USB devices named in `usb`, see [Devices and USB](devices.md) |
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
| `dbus` | `{ talk = [], own = [] }` | names on the host bus proxy added to the safe default set (see [D-Bus](dbus.md) for what is refused) |
| `bus` | `"host"` · `"zone"` · `"none"` | the session bus for a command that is not in `[apps]` (`appzones run Z -- CMD`). See [D-Bus](dbus.md) |
| `nix_ld` | `false` · `true` | generic Linux binaries run: `/lib64/ld-linux-x86-64.so.2` in the zone is the host's nix-ld shim, and `NIX_LD`, `NIX_LD_LIBRARY_PATH` are set as on the host. Refused on a host without nix-ld (`programs.nix-ld.enable`). See [Generic binaries](nix-ld.md) |
| `tmp` | `"ram"` · `"disk"` | `ram`: `/tmp` is a fresh tmpfs at every start. `disk`: `/tmp` and `/var/tmp` are `~/.local/share/appzones/NAME/tmp` and `.../var-tmp` (0700): kept across restarts, readable from the host. Cleaned at each start, see [/tmp on disk](files.md#tmp-on-disk-tmp--disk) |
| `home` | `""` | the zone's home; empty: `~/.local/share/appzones/NAME/home` |
| `force_host` | `false` · `true` | allow `network = "host"` in `untrusted` |
| `supervisor` | `"auto"` · `"systemd"` · `"spawn"` | who starts the zone when it is not running. `auto`: `$APPZONES_SUPERVISOR` if set, else systemd when `appzones@.service` is installed, else appzones itself. `systemd`: always its unit. `spawn`: always appzones itself (logs in `~/.local/state/appzones/`). The zone's own value beats `$APPZONES_SUPERVISOR`. See [Zones without systemd](../without-systemd.md) |
| `start_timeout` | `"30s"` | the limit for each step of the start (Wayland helper, D-Bus proxy, network setup, zone-init's answer): `"45s"`, `"2m"` or a number of seconds, up to 1 h. Building `packages` has no limit |
| `linger` | `""` | `""`: the zone stays until you stop it or the session ends. A duration (`"10m"`, `"1h"`, at least 10 s): the zone ends after that long with no app running in it (its own D-Bus daemon does not count). Under systemd it ends as a success, so it is not restarted |

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
- the host's own additions: `/etc/appzones/denied-paths`, one path per line,
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
  `org.mozilla.firefox*` (see Firefox under [D-Bus](dbus.md)), and a wildcard needs at
  least three labels before `.*` (`org.example.player.*`, not `org.example.*`);
- neither `talk` nor `own` takes a name that a host `.service` file provides
  (the validator asks the running session bus which names it can start;
  if it cannot ask, it grants nothing extra): a call to it
  would start that program on the host, outside the zone, for the zone to
  drive;
- a name in `own` of two zones is an error: the app in the zone that starts
  second would hand its work to the first, and a zone never hands anything
  to another zone.
