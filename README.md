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

### Set up with an AI agent

Paste this into your coding agent (Claude Code, Codex, opencode, ...) on
the machine where you want appzones:

```markdown
# Set up appzones

Set up **appzones** on this NixOS machine: <https://github.com/mirql/appzones>
(long-lived sandboxes for desktop apps). Read its `README.md` and
`docs/guide.md` first ([raw guide](https://raw.githubusercontent.com/mirql/appzones/main/docs/guide.md)).

1. **Check first**, and tell me what you find before changing anything:
   - x86_64 NixOS configured with flakes (find my flake and this host's config);
   - a Wayland compositor with `wp_security_context_v1` (Hyprland, sway, KWin, ...);
     window borders and tags per zone need Hyprland;
   - unprivileged user namespaces, `xdg-desktop-portal`, PipeWire;
   - an nftables firewall, or the kernel modules in the guide's "Kernel modules".
2. **Add the flake input** `appzones` (`github:mirql/appzones`, with
   `inputs.nixpkgs.follows = "nixpkgs"`), import `appzones.nixosModules.default`,
   and set `programs.appzones.enable = true` and `programs.appzones.user` to my user.
3. **Ask me which folders must never enter a zone** (code checkouts, secret
   stores, other disks) and put them in `programs.appzones.deniedPaths`
   (absolute, or `~/...`). If I use NetBird, set `netbirdInterface` (default `wt0`).
4. **Ask me which zones and apps I want** (for example `daily`, `work`, `untrusted`).
   Write `~/.config/zones/zones.toml` starting from `examples/minimal.toml` (or
   put it where my dotfiles manage `~/.config`). Add binds only for folders I
   name; never my whole home, `~/.ssh`, or a folder where programs keep sockets.
5. **Optional:** if I use Hyprland, load `contrib/hyprland/zones.lua` from my
   Hyprland config; if I use Waybar, add the custom module from the guide ("Hyprland").
6. **Build without switching** (`nixos-rebuild build --flake .#<host>`), then run
   `./result/sw/bin/appzones check`. Show me what you changed and what it reports.
7. **Do not switch yourself.** Give me the switch command, and tell me that a
   switch can stop zones that are running.
8. **After I switch:** `appzones list`, then `appzones run <zone> <app>`.
```

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

## Authors

- Askar Amirov ([@askaramirov](https://github.com/askaramirov))
- Saif H ([@Saifeldin-H](https://github.com/Saifeldin-H))

## License

MIT
