# appzones

Long-lived, easy to manage sandboxes for desktop apps on Linux.

An app zone is a sandbox your apps live in: `daily` for messengers and
personal browsing, `work` for the work browser and Slack, `untrusted` for
links and files you don't trust. Each zone has its own home, its own network
rules and its own window border colour. Apps in one zone can't see your
home, other zones or the host's sockets. You declare zones in one TOML file
and they run as systemd user units.

```bash
appzones run daily firefox          # start the zone if needed, launch Firefox in it
appzones run work -- chromium       # any command
appzones shell untrusted            # a shell inside
appzones mount work ~/src/thing     # share a folder with a running zone, until umount
appzones list                       # zones, state, network, devices, apps running
```

```toml
# ~/.config/zones/zones.toml
version = 1

[zones.work]
colour  = "#89b4fa"
devices = ["gpu", "audio", "screencast"]
binds   = [{ path = "~/Work", mode = "rw" }]

[apps.firefox]
command = ["firefox"]
zones   = ["work"]
```

## How it works

Built from parts that already exist, with no daemon and no setuid binary of its own:

- **bubblewrap**: user, pid, ipc, uts and cgroup namespaces; no
  capabilities; `no_new_privs`; a fresh home and a curated `/etc`.
- **pasta** + **nftables**: its own network namespace with a deny list
  loaded inside it. The host's own addresses, its LANs (IPv4 and IPv6),
  loopback, tailnets and link-local are refused; the internet is reachable.
- **xdg-dbus-proxy**: a filtered session bus. Portals (file chooser, links,
  notifications, screen sharing) work; systemd, secrets and the host's file
  daemons don't.
- **wp_security_context_v1**: the compositor hides screencopy, clipboard
  managers, virtual input and global shortcuts from zone clients.
- **Hyprland** (optional): zone windows get a border in the zone's colour
  and a `zone:NAME` tag, taken from the process's cgroup, which the app
  can't fake.

## Install

NixOS with flakes:

```nix
inputs.appzones.url = "github:mirql/appzones";

# configuration
imports = [ inputs.appzones.nixosModules.default ];
programs.appzones.enable = true;
```

Then copy `examples/minimal.toml` to `~/.config/zones/zones.toml` (or run
`appzones new daily -y`), and run `appzones check`. You need x86_64 NixOS and a
Wayland compositor with `wp_security_context_v1`; window tags and border
colours need Hyprland. Other distributions aren't packaged yet.

## Docs

- [docs/guide.md](docs/guide.md): usage, the full configuration reference,
  D-Bus, USB devices, runtime mounts, how a zone is built, and the honest
  limits.
- [examples/minimal.toml](examples/minimal.toml): a first config that works anywhere.
- [examples/zones.toml](examples/zones.toml): every key, commented.

## Limits

Zones share your uid and kernel: a kernel bug reachable from inside
escapes everything. A compromised app can still read everything in its own
zone, draw windows that look like anything (learn the border colours), and
use the internet as you. X11-only apps don't run yet. See "Honest limits"
in the guide.

## Development

```bash
nix develop
just test        # unit tests (fast, no Nix)
just build       # the package; runs the unit tests too
just test-vm     # NixOS VM test with Hyprland (~6 min, needs /dev/kvm)
```

See [CONTRIBUTING.md](CONTRIBUTING.md). Found a way out of a zone? Please
report it privately: [SECURITY.md](SECURITY.md).

## License

MIT
