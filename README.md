<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo/appzones-dark.svg">
    <img alt="appzones." src="assets/logo/appzones-light.svg" width="420">
  </picture>
</p>

<p align="center"><b>Long-lived sandboxes for your desktop apps, dev tools and AI agents on Linux.</b></p>

<p align="center">
  <a href="https://github.com/mirql/appzones/releases"><img alt="Release" src="https://img.shields.io/github/v/release/mirql/appzones"></a>
  <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-blue"></a>
  <a href="#install"><img alt="NixOS flake and module" src="https://img.shields.io/badge/NixOS-flake%20%2B%20module-5277C3?logo=nixos&logoColor=white"></a>
  <a href="docs/getting-started/install.md"><img alt="Wayland" src="https://img.shields.io/badge/Wayland-wp__security__context__v1-FFBC00"></a>
  <br>
  <a href="https://github.com/mirql/appzones/stargazers"><img alt="GitHub stars" src="https://img.shields.io/github/stars/mirql/appzones"></a>
  <a href="https://github.com/mirql/appzones/commits/main"><img alt="Last commit" src="https://img.shields.io/github/last-commit/mirql/appzones"></a>
  <a href="https://github.com/mirql/appzones/graphs/commit-activity"><img alt="Commit activity" src="https://img.shields.io/github/commit-activity/m/mirql/appzones"></a>
  <a href="https://coderabbit.ai"><img alt="CodeRabbit reviews" src="https://img.shields.io/coderabbit/prs/github/mirql/appzones?labelColor=171717&color=FF570A&label=CodeRabbit%20reviews"></a>
  <a href="CONTRIBUTING.md"><img alt="One Rust binary, dependencies pinned" src="https://img.shields.io/badge/Rust-one%20binary-brightgreen?logo=rust&logoColor=white"></a>
  <!-- ALL-CONTRIBUTORS-BADGE:START - Do not remove or modify this section -->
<a href="#contributors"><img alt="All contributors" src="https://img.shields.io/badge/all_contributors-6-orange"></a>
<!-- ALL-CONTRIBUTORS-BADGE:END -->
</p>

> *I wanted my desktop split into **separate, sealed-off spaces**: a browser
> for each kind of work, and a clean home for every AI tool with its own
> configs. A VM or a container for each felt like **far too much**, so I built
> something **lighter**.*
>
> — **Askar**, author of appzones

An app zone is a place your apps live in: `daily` for messengers, `work`
for Slack and the work browser, `untrusted` for links you don't trust,
`dev` for `npm install`. Each zone has its own home, its own network rules
and its own window border colour. You declare them in one TOML file; they
run as systemd user units, or without systemd, started on demand.

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/concept/concept-dark.svg">
    <img alt="A link opens in the untrusted zone. Its attempts to read ~/.ssh and ~/Documents, reach the host's dev server, the other zones and the LAN are refused; the internet is reachable." src="assets/concept/concept-light.svg" width="760">
  </picture>
</p>

```bash
appzones run daily firefox        # start the zone if needed, launch Firefox in it
appzones run dev -- kitty         # any command
appzones shell untrusted          # a shell inside
appzones mount dev ~/src/thing    # share a folder with a running zone, until umount
appzones list                     # zones, state, network, devices, apps running
```

> [!NOTE]
> appzones is young (0.14). It runs on **x86_64 NixOS**, and as a preview on
> **Ubuntu 24.04, Debian 12, Arch, Fedora and openSUSE**: command-line zones, and desktop apps
> under sway 1.9+ (tested) or GNOME (`wayland = "host"`, tried by hand).
> Window borders and tags per zone need Hyprland. On **macOS** (Apple silicon, in
> development): command-line zones ([install](docs/getting-started/install.md#macos-in-development)).

## Why

Everything you run on Linux runs as you. A browser exploit, a malicious
`postinstall` script, or an AI agent that runs the wrong command can read
`~/.ssh`, your cloud credentials, your other projects and your password
store, and reach every device on your LAN.

appzones puts a wall between those things, without a VM per app:

- **Your host stays clean.** Package managers, agent CLIs, prebuilt binaries
  and their caches live in a zone's home, not yours.
- **Zones, not per-app profiles.** Group apps the way you work. One TOML
  file, validated before anything starts.
- **Your apps, as they are.** A zone runs the host's own apps from the Nix
  store: no second runtime, no repackaging.
- **You can see where a window lives.** Each zone's windows carry its
  border colour, set from the process's cgroup, which the app can't fake.

![A terminal: the host has an SSH key; inside the untrusted zone the home is empty, the key is not there, a LAN address is refused and a public address works](docs/demo-untrusted.gif)

<sub>Recorded in a NixOS VM by <code>just demo</code> (<a href="tests/demo.nix">tests/demo.nix</a>): the commands and their output are real, the yellow lines are narration. The LAN and "the internet" are a second VM.</sub>

## Recipes

### An isolated browser

```toml
[zones.untrusted]
colour  = "#f38ba8"
dns     = "public"   # no internal names, no LAN
links   = "zone"     # links clicked here open in this zone's browser
devices = ["gpu"]    # no sound, no mic, no camera, no screen sharing

[apps.firefox]
command = ["firefox"]
zones   = ["untrusted"]
args    = { untrusted = ["--private-window"] }
```

`appzones run untrusted firefox`, or "Firefox (untrusted)" in your launcher.

### Package managers off the host

Keep `npm`, `pip`, `cargo` and their install scripts away from your real
home:

```toml
[zones.dev]
colour   = "#f9e2af"
binds    = [{ path = "~/src/my-app", mode = "rw" }]
packages = ["nodejs", "python3", "uv"]  # nixpkgs attributes, on PATH in the zone
nix_ld   = true                         # prebuilt binaries from npm and pip run
tmp      = "disk"                       # /tmp survives restarts
```

On Ubuntu or Debian, leave out `packages` and `nix_ld` (NixOS's): the zone
uses the host's programs from `/usr`, so install them on the host first
(`sudo apt install nodejs npm python3-venv`), and prebuilt binaries run
there anyway. Debian's and Ubuntu's `pip` installs only into a virtual
environment: `python3 -m venv .venv && . .venv/bin/activate` inside the zone.

```bash
appzones run dev -- kitty -e tmux new -As dev   # a terminal + tmux session inside
appzones shell dev                              # or, without a desktop (SSH): a shell inside
# then, inside: cd ~/src/my-app, then npm install, npm run dev, pip install ...
```

The tmux server, `~/.npm`, `~/.cache` and every install script stay inside
`dev`. The host never runs `npm`.

### AI agents in a zone

Give an agent one project and nothing else:

```toml
[zones.agents]
colour   = "#cba6f7"
binds    = [{ path = "~/src/my-app", mode = "rw" }]
packages = ["nodejs", "git"]
nix_ld   = true                         # NixOS; on Ubuntu or Debian leave out these two lines
```

```bash
appzones run agents -- kitty     # install and log in to your agent CLI in there
appzones shell agents            # the same without a desktop (SSH)
```

The agent's login, its tokens and its history stay in the zone's home. It
can't read `~/.ssh` or your other projects, has no `SSH_AUTH_SOCK`, and
can't reach your LAN or the host's own services.

To let it talk only to the services it needs, give it an allow list: it then reaches
those names and nothing else, through appzones' own proxy (`HTTPS_PROXY`, set for its
apps), and every refused name is in `appzones logs agents`. The key is release 7: set
`requires = 7` at the top of the file (an older file says 6 or less):

```toml
allow = ["api.anthropic.com", "github.com", "*.githubusercontent.com", "registry.npmjs.org"]
```

![A terminal: the host has three projects and a dev server on localhost:3000; inside the agents zone only ~/src/my-app exists, ssh-add finds no agent, and localhost:3000 is refused](docs/demo-agents.gif)

> [!CAUTION]
> A **writable** bind is writable from the zone: it can change anything in
> that folder, including files your host runs later, such as `.git/hooks`,
> `.envrc`, `Makefile` and `package.json` scripts. Review what comes out of
> a zone before you run it on the host, or bind read-only and copy results
> out.

### Everyday apps, kept apart

```toml
[zones.work]
colour  = "#89b4fa"
devices = ["gpu", "audio", "mic", "screencast"]

[apps.slack]
command = ["slack"]
zones   = ["work"]
```

Every app in `[apps]` gets a launcher entry per zone ("Slack (work)"). Logins
and data of one zone never reach another.

### Opening a file you don't trust

```bash
appzones up untrusted                              # a mount needs a running zone
appzones mount untrusted ~/Downloads --ro --as dl
appzones run untrusted -- evince /mnt/host/dl/invoice.pdf
# ... read it; once Evince is closed:
appzones umount untrusted dl                       # detaches the mount; ~/Downloads stays as it is
```

![A terminal: ~/Downloads is mounted read-only into the untrusted zone; inside, writing to invoice.pdf fails with Read-only file system; then it is unmounted](docs/demo-file.gif)

> [!TIP]
> `appzones new NAME` prints a safe zone block, and `-y` appends it to your
> config. `appzones check --probe ZONE` runs isolation probes inside a zone.

<details>
<summary><b>How it works</b></summary>

Built from parts that already exist, with no daemon and no setuid binary of
its own:

- **bubblewrap**: user, pid, ipc, uts and cgroup namespaces; no
  capabilities; `no_new_privs`; a fresh home and a curated `/etc`.
- **pasta + nftables**: its own network namespace with a deny list loaded
  inside it. The host's own addresses, its LANs (IPv4 and IPv6), loopback,
  tailnets and link-local are refused; the internet is reachable.
- **xdg-dbus-proxy**: a filtered session bus. Portals (file chooser, links,
  notifications, screen sharing) work; systemd, secrets and the host's file
  daemons don't.
- **wp_security_context_v1**: the compositor hides screencopy, clipboard
  managers, virtual input and global shortcuts from zone clients.
- **Hyprland** (optional): zone windows get the zone's border colour and a
  `zone:NAME` tag, from the process's cgroup.

</details>

<details>
<summary><b>How it compares</b>: Flatpak, Firejail, Qubes OS, Distrobox</summary>

| | appzones | Flatpak | Firejail | Qubes OS | Distrobox / Toolbx |
|---|---|---|---|---|---|
| You group apps by | trust (a zone per area of your life) | app | app | VM | container (not a boundary) |
| Apps come from | the host's own programs (Nix store or `/usr`) | Flathub, packaged for it | your system | each VM's OS | the container's distro |
| Network | internet yes; LAN, host and tailnet no | on or off | on, off or a filter | per-VM firewall | the host's |
| Which zone is this window? | border colour, the app can't fake it | no | no | border colour | no |
| Isolation | namespaces, same kernel | namespaces, same kernel | namespaces, same kernel, setuid binary | separate VMs (much stronger) | almost none |
| Runs on | NixOS; Ubuntu, Debian, Arch, Fedora, openSUSE (preview) | any distro | any distro | Qubes only | any distro |

appzones is for when Flatpak is too fine-grained, Qubes is too much, and
you want your messenger, your work browser, your agents and that PDF in
separate, labelled boxes on the desktop you already have.

</details>

## Install

NixOS with flakes:

```nix
# flake.nix
inputs.appzones.url = "github:mirql/appzones";
inputs.appzones.inputs.nixpkgs.follows = "nixpkgs";

# configuration.nix
imports = [ inputs.appzones.nixosModules.default ];
programs.appzones = {
  enable = true;
  user = "alice";
  deniedPaths = [ "~/src/secret-project" ];  # never in any zone, on top of ~/.ssh & co.
};
```

Then copy [`examples/minimal.toml`](examples/minimal.toml) to
`~/.config/appzones/zones.toml` (or run `appzones new daily -y`) and run
`appzones check`.

Ubuntu 24.04 and Debian 12 (preview): the `.deb` from the
[latest release](https://github.com/mirql/appzones/releases/latest), with
`sudo apt install ./appzones_*_amd64.deb`. Or from a checkout:

```bash
sudo apt install bubblewrap passt nftables iproute2 git make gcc   # a desktop also: xdg-dbus-proxy dbus
# and Rust 1.85 or newer (rustup.rs: Debian 12 and Ubuntu 24.04 ship older ones)
git clone https://github.com/mirql/appzones && cd appzones && make && sudo make install
```

Arch: `paru -S appzones` from the [AUR](https://aur.archlinux.org/packages/appzones) ([details](https://mirql.github.io/appzones/getting-started/install.html#arch-linux-preview)).
Fedora: the RPM from the [latest release](https://github.com/mirql/appzones/releases/latest), or `make install` ([details](https://mirql.github.io/appzones/getting-started/install.html#fedora-preview)).
The [install page](https://mirql.github.io/appzones/getting-started/install.html#ubuntu-and-debian-preview)
says what it installs (an AppArmor profile on Ubuntu, a sudo rule for
`appzones mount`) and how to start your first zone.

> [!IMPORTANT]
> A system switch can stop zones that are running. Close what runs in your
> zones before you switch.

<details>
<summary><b>Set up with an AI agent</b> (NixOS): a prompt to paste into Claude Code, Codex, opencode, ...</summary>

```markdown
# Set up appzones

Set up **appzones** on this NixOS machine: <https://github.com/mirql/appzones>
(long-lived sandboxes for desktop apps). Read its `README.md` and the docs
first: [install](https://raw.githubusercontent.com/mirql/appzones/main/docs/getting-started/install.md),
[configuration reference](https://raw.githubusercontent.com/mirql/appzones/main/docs/configuration/reference.md),
[desktop](https://raw.githubusercontent.com/mirql/appzones/main/docs/desktop.md) (raw markdown; the site is https://mirql.github.io/appzones/).

1. **Check first**, and tell me what you find before changing anything:
   - x86_64 NixOS configured with flakes (find my flake and this host's config);
   - a Wayland compositor with `wp_security_context_v1` (Hyprland, sway 1.9+, KDE Plasma 6's KWin, ...);
     window borders and tags per zone need Hyprland;
   - unprivileged user namespaces, `xdg-desktop-portal`, PipeWire;
   - an nftables firewall, or the kernel modules in the install page's "Kernel modules".
2. **Add the flake input** `appzones` (`github:mirql/appzones`, with
   `inputs.nixpkgs.follows = "nixpkgs"`), import `appzones.nixosModules.default`,
   and set `programs.appzones.enable = true` and `programs.appzones.user` to my user.
3. **Ask me which folders must never enter a zone** (code checkouts, secret
   stores, other disks) and put them in `programs.appzones.deniedPaths`
   (absolute, or `~/...`). If I use NetBird, set `netbirdInterface` (default `wt0`).
4. **Ask me which zones and apps I want** (for example `daily`, `work`, `untrusted`).
   Write `~/.config/appzones/zones.toml` starting from `examples/minimal.toml` (or
   put it where my dotfiles manage `~/.config`). Add binds only for folders I
   name; never my whole home, `~/.ssh`, or a folder where programs keep sockets.
5. **Optional:** if I use Hyprland, load `contrib/hyprland/appzones.lua` from my
   Hyprland config; if I use Waybar, add the custom module from the desktop page.
6. **Build without switching** (`nixos-rebuild build --flake .#<host>`), then run
   `./result/sw/bin/appzones check`. Show me what you changed and what it reports.
7. **Do not switch yourself.** Give me the switch command, and tell me that a
   switch can stop zones that are running.
8. **After I switch:** `appzones list`, then `appzones run <zone> <app>`.
```

</details>

### Use it with your AI agent

The [appzones skill](skills/appzones/SKILL.md) teaches a coding agent
(Claude Code, Codex, opencode, Cursor, ...) to run things in your zones, add
zones and apps, share folders, and debug what doesn't work inside one. It
asks before stopping a zone and never weakens isolation on its own.

```bash
npx skills add mirql/appzones
```

Or copy `skills/appzones/SKILL.md` into your agent's skills folder.

## Limits

> [!WARNING]
> Zones share your uid and your kernel: a kernel bug reachable from inside
> escapes everything. A compromised app can still read everything in its own
> zone, draw windows that look like anything (learn your border colours),
> and use the internet as you. X11-only apps don't run yet.
> See [Security and limits](docs/security.md) in the docs.

## Docs

- [Docs](https://mirql.github.io/appzones/) ([in the repo](docs/guide.md)): install, recipes, every `zones.toml`
  key, D-Bus, USB devices, runtime mounts, troubleshooting, how a zone is built.
- [examples/minimal.toml](examples/minimal.toml): a first config that works anywhere.
- [examples/zones.toml](examples/zones.toml): every key, commented.

## Development

```bash
nix develop
just test        # unit tests (fast, no Nix)
just build       # the package; runs the unit tests too
just test-vm     # NixOS VM test with Hyprland (~6 min, needs /dev/kvm)
```

See [CONTRIBUTING.md](CONTRIBUTING.md). Found a way out of a zone? Please
report it privately: [SECURITY.md](SECURITY.md).

## Contributing

Issues, ideas and PRs are welcome. Good places to start:

- **[Good first issues](https://github.com/mirql/appzones/labels/good%20first%20issue).** Small, self-contained
  tasks, each saying where to start and what done looks like. Most need no
  NixOS to build and test.
- **Other compositors.** Hyprland gets window tags and border colours, sway
  marks and title prefixes. niri or KWin hooks would help ([#20](https://github.com/mirql/appzones/issues/20)).
- **Other distributions.** A package for openSUSE; others: a package, and a VM
  test like the Debian and Ubuntu ones ([#22](https://github.com/mirql/appzones/issues/22)).
- **aarch64.** Nothing has been tried there yet.
- **Docs.** If something in the [docs](https://mirql.github.io/appzones/) was unclear to you,
  it will be to the next person too.

Questions go to [Discussions](https://github.com/mirql/appzones/discussions).

## Contributors

Thanks to everyone who made appzones what it is
([emoji key](https://allcontributors.org/docs/en/emoji-key)):

<!-- ALL-CONTRIBUTORS-LIST:START - Do not remove or modify this section -->
<!-- prettier-ignore-start -->
<!-- markdownlint-disable -->
<table>
  <tbody>
    <tr>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/askaramirov"><img src="https://avatars.githubusercontent.com/u/59274133?size=80" width="80px" height="80px" alt="Askar Amirov"/><br /><sub><b>Askar Amirov</b></sub></a><br /><a href="https://github.com/mirql/appzones/commits?author=askaramirov" title="Code">💻</a> <a href="https://github.com/mirql/appzones/commits?author=askaramirov" title="Documentation">📖</a> <a href="#design-askaramirov" title="Design">🎨</a> <a href="https://github.com/mirql/appzones/commits?author=askaramirov" title="Tests">⚠️</a> <a href="#security-askaramirov" title="Security">🛡️</a> <a href="#infra-askaramirov" title="Infrastructure (Hosting, Build-Tools, etc)">🚇</a> <a href="#maintenance-askaramirov" title="Maintenance">🚧</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/Saifeldin-H"><img src="https://avatars.githubusercontent.com/u/249896858?size=80" width="80px" height="80px" alt="Saif H"/><br /><sub><b>Saif H</b></sub></a><br /><a href="https://github.com/mirql/appzones/commits?author=Saifeldin-H" title="Documentation">📖</a> <a href="#design-Saifeldin-H" title="Design">🎨</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/Lil-Clanky"><img src="https://avatars.githubusercontent.com/u/190942173?v=4&size=80" width="80px" height="80px" alt="Marnix de Jong"/><br /><sub><b>Marnix de Jong</b></sub></a><br /><a href="https://github.com/mirql/appzones/commits?author=Lil-Clanky" title="Code">💻</a> <a href="https://github.com/mirql/appzones/commits?author=Lil-Clanky" title="Tests">⚠️</a> <a href="https://github.com/mirql/appzones/commits?author=Lil-Clanky" title="Documentation">📖</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/emreocell"><img src="https://avatars.githubusercontent.com/u/73295089?v=4&size=80" width="80px" height="80px" alt="Emre Öcel"/><br /><sub><b>Emre Öcel</b></sub></a><br /><a href="https://github.com/mirql/appzones/commits?author=emreocell" title="Code">💻</a> <a href="https://github.com/mirql/appzones/commits?author=emreocell" title="Tests">⚠️</a> <a href="https://github.com/mirql/appzones/commits?author=emreocell" title="Documentation">📖</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/tyza66"><img src="https://avatars.githubusercontent.com/u/83948628?v=4&size=80" width="80px" height="80px" alt="孙达明"/><br /><sub><b>孙达明</b></sub></a><br /><a href="https://github.com/mirql/appzones/commits?author=tyza66" title="Code">💻</a> <a href="https://github.com/mirql/appzones/commits?author=tyza66" title="Tests">⚠️</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/RiccardoBifulco"><img src="https://avatars.githubusercontent.com/u/154237075?v=4&size=80" width="80px" height="80px" alt="Riccardo Bifulco"/><br /><sub><b>Riccardo Bifulco</b></sub></a><br /><a href="https://github.com/mirql/appzones/commits?author=RiccardoBifulco" title="Code">💻</a> <a href="https://github.com/mirql/appzones/commits?author=RiccardoBifulco" title="Tests">⚠️</a> <a href="https://github.com/mirql/appzones/commits?author=RiccardoBifulco" title="Documentation">📖</a></td>
    </tr>
  </tbody>
</table>

<!-- markdownlint-restore -->
<!-- prettier-ignore-end -->

<!-- ALL-CONTRIBUTORS-LIST:END -->

Helped in a way that isn't a commit? Comment on any issue or PR:
`@all-contributors please add @name for ideas, review` (and so on).

## License

[MIT](LICENSE)
