# Install

| Where | What works | How |
|---|---|---|
| **NixOS** | everything: desktop apps, window borders on Hyprland, `packages`, `nix_ld` | [the flake's module](#nixos) |
| **Ubuntu 24.04, Debian 12** (preview) | command-line zones: dev tools and AI agents, also over SSH; desktop apps under sway 1.9+ (tested on Ubuntu 24.04) or GNOME (with `wayland = "host"`, tried by hand); no window borders | [the .deb, or `make install`](#ubuntu-and-debian-preview) |
| **Arch Linux** (preview) | the same as Ubuntu and Debian; sway names zones started without systemd too | [a PKGBUILD](#arch-linux-preview), or `make install` |
| **Fedora 45** (preview) | the same, with SELinux enforcing | [the RPM, or `make install`](#fedora-preview) |
| **openSUSE Leap 16** (preview) | the same, with SELinux enforcing; internet zones need one SELinux setting | [`make install`](#opensuse-preview) |
| **macOS** (Apple silicon, in development) | not zones yet: `check`, `list`, `which` and `new` work | [from a checkout](#macos-in-development) |

Other x86_64 distributions with systemd or another init, glibc 2.17 or newer and the
same tools should work like these; they are not tested yet.

## NixOS

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

Then write `~/.config/appzones/zones.toml`: copy `examples/minimal.toml`
(works on any host), or run `appzones new NAME -y` (creates the file if there
is none). `examples/zones.toml` shows every key; parts of it need more setup
(`nix_ld` needs `programs.nix-ld.enable`, binds need the folders to exist).
Run `appzones check`.

## Ubuntu and Debian (preview)

Tested on Ubuntu 24.04 and Debian 12, in VMs, for every change
(`just test-distros`): command-line zones, and on Ubuntu a Wayland app from a
zone under headless sway (its own security context: no screencopy, no
clipboard snooping). Ubuntu 22.04 has no passt.

**The package.** Each [release](https://github.com/mirql/appzones/releases/latest)
has a `.deb` for amd64 (built on Debian 12; tested on Debian 12 and Ubuntu 24.04).
Installed from a checkout before? Remove that first (`sudo make uninstall` in the
checkout, with the options you installed with), or its `/usr/local/bin/appzones`
comes first on `PATH`. Download `appzones_VERSION-1_amd64.deb` from there, then:

```bash
sudo apt install ./appzones_*_amd64.deb   # also fetches bubblewrap, passt, nftables, ...
sudo usermod -aG appzones $USER           # for appzones mount; log in again after it
```

It installs into `/usr`, with the systemd user units, a sudo rule for exactly
the mount helper for members of the group `appzones`, and on Ubuntu 23.10 and
later the AppArmor profile (see below). apt also installs the recommended
`xdg-dbus-proxy` and `dbus` (the desktop's parts); on a server,
`--no-install-recommends` leaves them out. Remove it with `sudo apt purge
appzones`. Then go on with step 3. The package holds appzones' one binary and its
mount helper, built to run on glibc 2.17 and later. To build it yourself:
`nix build .#deb` (the same binaries), or `sh contrib/debian/build-deb.sh OUTDIR`
(with cargo: that package needs the build machine's glibc or newer, and says so).

**Or from a checkout** (for any commit):

**1. The packages.** appzones is written in Rust; building it needs Rust 1.85 or
newer. Debian 12 and Ubuntu 24.04 ship older ones, so take it from
[rustup](https://rustup.rs) (as your user, not root).

```bash
sudo apt install bubblewrap passt nftables iproute2 git make gcc   # enough for a server
sudo apt install xdg-dbus-proxy dbus                               # for a desktop
```

**2. Install from a checkout.**

```bash
git clone https://github.com/mirql/appzones && cd appzones
make                 # as your user: cargo builds build/appzones-helper and build/appzones-mount-helper
sudo make install    # installs what make built
```

The second line is the desktop's part: `xdg-dbus-proxy` gives a zone the
filtered session bus (portals, notifications, the tray), and `dbus-daemon` an
app with `bus = "zone"` its own bus. Without `xdg-dbus-proxy` a zone starts
without the bus and says so in its log (`appzones logs ZONE`). An app with
`bus = "zone"` needs `dbus-daemon` (the `dbus` package), also on a server:
without it that app does not start, and appzones says what is missing.
Otherwise a server or a command-line zone needs only the first line. (Each
zone's own Wayland socket needs nothing more: zone-wayland is part of the binary.)
`sudo make install` puts:

- the binary (`appzones-helper`) and the mount helper in `/usr/local/libexec/appzones`,
  and the `appzones` command in `/usr/local/bin`. Any `PREFIX` works: each zone
  gets a read-only copy of the binary, and nothing else of appzones;
- the systemd user units (`appzones@.service` and the autostart unit) in
  `/usr/local/lib/systemd/user`, where there is systemd;
- a sudo rule for exactly the mount helper of `appzones mount`
  (`/etc/sudoers.d/appzones`), for the user who ran `sudo`;
- on Ubuntu 23.10 and later, an AppArmor profile (`/etc/apparmor.d/appzones`,
  loaded at once). Ubuntu blocks unprivileged user namespaces, and without them
  bwrap and pasta cannot build any zone ("setting up uid map: Permission
  denied"). The profile lets exactly these two programs create user
  namespaces, as every other distribution allows anyway. Debian needs none.

| Option | Default | |
|---|---|---|
| `PREFIX` | `/usr/local` | anywhere; outside `/usr` the sudo rule is written only when that directory and everything above it belong to root alone |
| `LIBDIR` | `PREFIX/libexec/appzones` | where the two binaries go (the .deb and the AUR package: `/usr/lib/appzones`) |
| `HELPER_BIN`, `MOUNT_HELPER_BIN` | built by `make` | the two binaries, built elsewhere (`nix build .#portable`: for glibc 2.17 and later) |
| `MOUNT_USERS` | the user who ran `sudo`, else `%users` | who may run the mount helper: a user, or `%group` |
| `SUDOERS` | `yes` | `no`: no sudo rule (`appzones mount` then does not work) |
| `APPARMOR` | `auto` (where the kernel restricts user namespaces) | `yes`, `no` |
| `SYSTEMD` | `auto` (where systemd runs) | `yes`, `no` |
| `DESTDIR` | | staging directory, for packages |

**3. Your first zone.** Each user, once: `systemctl --user daemon-reload`
(with systemd). Then a zone for command-line work:

```bash
mkdir -p ~/.config/appzones ~/src/my-project    # or bind a project you have
cat > ~/.config/appzones/zones.toml <<'TOML'
version = 1
requires = 5

[zones.dev]
colour  = "#a6e3a1"
devices = []                                     # no GPU, no sound: a server is fine
binds   = [{ path = "~/src/my-project", mode = "rw" }]
theme   = false                                  # no desktop settings to copy in
TOML
appzones check
appzones shell dev                               # a shell in the zone; inside, for example:
#   cd ~/src/my-project && npm install           # (with nodejs and npm installed on the host)
```

From a script, `appzones run --wait` passes its standard streams in and returns
the command's exit code. It never takes a terminal (no host terminal enters a
zone), so give it pipes or files:

```bash
appzones run --wait dev -- sh -c 'cd ~/src/my-project && npm test' </dev/null >test.log 2>&1
echo "npm test exited with $?"
```

The zone has its own home (`~/.local/share/appzones/dev/home`). Of your
files it sees only `~/src/my-project`, and everything in it: a key or token
kept inside that folder is readable there. With `theme = true` (the
default) it also gets copies of your GTK, Qt, font and cursor settings. The
rest of your home, `~/.ssh` and your keyrings stay out (they can never be
bound). It reaches the internet
but not your LAN or this machine. See [recipes](../recipes.md) for
AI agents in a zone.

**Your own never-list.** Paths no zone may ever bind or mount, on top of the
built-in list (`~/.ssh`, `~/.gnupg`, ...), go in `/etc/appzones/denied-paths`,
one per line, absolute or `~/`, `#` for comments (on NixOS: `deniedPaths`):

```bash
sudo mkdir -p /etc/appzones
printf '%s\n' /srv/code '~/Secrets' | sudo tee /etc/appzones/denied-paths
```

**What is different from NixOS.** A zone sees the host's `/usr` read-only
(on NixOS: the Nix store). `packages` needs Nix on the host (without it: a
warning, nothing built); `nix_ld` is NixOS's and not needed here, since
generic binaries run anyway. Window borders need Hyprland and its Lua config
([Desktop](../desktop.md)), as everywhere.

**Without systemd** (runit, s6, OpenRC): zones start on demand, or under your
process manager ([Zones without systemd](../without-systemd.md)). For
`appzones mount`, run `sudo mount --make-rshared /` at every boot, before zones
start (systemd does this itself).

**Update and remove.** In the checkout: `git pull && make && sudo make
install` (with the same options as the first time, such as `PREFIX`), then `appzones restart ZONE` for running zones. `sudo make uninstall`
removes everything `make install` put there; your zones' homes and settings
stay in `~/.local/share/appzones` and `~/.config/appzones`.

## Arch Linux (preview)

Tested on Arch with `just test-arch` (the package built from the checkout,
then the same test as Ubuntu and Debian, in a VM): no AppArmor profile is
needed, and sway 1.11 or later names a zone's windows also when the zone
was started without systemd. From 0.8.2 on the package needs
only what a server needs; for a desktop also install
`xdg-dbus-proxy` and `dbus` (its optional dependencies). The package is on the AUR,
[`appzones`](https://aur.archlinux.org/packages/appzones) (its `PKGBUILD` is
kept in `contrib/arch/`):

```bash
paru -S appzones                     # or yay -S appzones, or by hand:
# git clone https://aur.archlinux.org/appzones.git && cd appzones && makepkg -si
systemctl --user daemon-reload
sudo gpasswd -a "$USER" appzones     # for `appzones mount`; then log in again
```

The package puts appzones' binaries under `/usr/lib/appzones`, the command in
`/usr/bin`, the systemd user units in `/usr/lib/systemd/user`, and a sudo rule
for the mount helper for the group `appzones` (created by sysusers), so only
the users you add to it may use `appzones mount`. `pacman -R appzones` removes
it all. Then [your first zone](#ubuntu-and-debian-preview) as on Ubuntu (step 3).
`make install` from a checkout works too, as on Debian.

## Fedora (preview)

Tested on Fedora 45 with `just test-fedora` (the RPM built from the
checkout, then the same test as Ubuntu and Debian, in a VM), with SELinux
enforcing: no policy change and no AppArmor profile is needed. Releases from
0.11.1 on have the RPM (`appzones-VERSION-1.fc45.x86_64.rpm`, from
`contrib/fedora/appzones.spec`); download it from the
[latest release](https://github.com/mirql/appzones/releases/latest), then:

```bash
sudo dnf install ./appzones-*.fc45.x86_64.rpm   # also fetches bubblewrap, passt, nftables, ...
sudo usermod -aG appzones "$USER"              # for appzones mount; log in again after it
```

It installs into `/usr` like the .deb, with the systemd user units and a
sudo rule for exactly the mount helper for the group `appzones`. `sudo dnf
remove appzones` removes it. Or from a checkout:

```bash
sudo dnf install bubblewrap passt nftables iproute git make gcc cargo   # enough for a server
sudo dnf install xdg-dbus-proxy dbus-daemon                            # for a desktop
git clone https://github.com/mirql/appzones && cd appzones
make && sudo make install
```

Then [your first zone](#ubuntu-and-debian-preview) as on Ubuntu (step 3).
`sudo make uninstall` removes it again.

## openSUSE (preview)

Tested on Leap 16 with `just test-opensuse` (`make install` from the
checkout, then the same test as Ubuntu and Debian, in a VM), with SELinux
enforcing. There is no package yet.

```bash
sudo zypper install bubblewrap passt nftables iproute2 git make gcc cargo   # enough for a server
sudo zypper install xdg-dbus-proxy dbus-1-daemon                           # for a desktop
git clone https://github.com/mirql/appzones && cd appzones
make && sudo make install
```

**Internet zones and SELinux.** On Leap 16 `/usr/bin/pasta` is a link to the
`passt` binary, so SELinux runs pasta in passt's domain, which may not build a
network namespace: a zone with `network = "internet"` does not start, and its
log says why. Until openSUSE's passt has its own pasta binary, let that domain
run unconfined (denials are still logged):

```bash
sudo zypper install policycoreutils-python-utils
sudo semanage permissive -a passt_t
```

Zones with `network = "none"` or `"host"` work without it. Then
[your first zone](#ubuntu-and-debian-preview) as on Ubuntu (step 3).

## macOS (in development)

appzones builds on macOS (Apple silicon), but zones do not run there yet: `appzones
check`, `list`, `which` and `new` work, and `up`, `run`, `shell`, `restart`, `serve`,
`mount`, `apps` and `check --probe` say that zones do not run on this system yet. The
plan (macOS's own sandbox, Seatbelt) is in `design/macos.md` in the repository. To build
it: `nix develop` (or Rust 1.85 or newer), then `cd helper && cargo build --release`;
the command is `helper/target/release/appzones-helper cli` (as `appzones`).

## Requirements

- x86_64 Linux: NixOS, or Ubuntu 24.04, Debian 12, Arch, Fedora, openSUSE
  (preview, above). The release packages need glibc 2.17 or newer; building
  needs Rust 1.85 or newer.
- For desktop apps, a Wayland compositor with `wp_security_context_v1`
  (Hyprland, sway 1.9 and later, other wlroots 0.17+ compositors, KDE Plasma
  6's KWin): every zone gets its own security context. What the compositor
  hides from a zone's apps differs: Hyprland and sway hide screencopy,
  clipboard snooping, virtual input and the window list; KWin 6.8 hides
  clipboard snooping, fake input, the window list and screencast, but still
  offers them display configuration (`kde_output_management_v2`), DPMS and
  `org_kde_plasma_shell`, so a zone's app could rearrange or blank the screens. Under a compositor without it (GNOME's
  Mutter 46, Weston, older sway) a zone starts without a Wayland socket and
  its log says why; `wayland = "host"` then gives that zone the host's socket
  (no security context; under GNOME that hides little, since Mutter offers no
  screencopy or clipboard snooping to apps). Window tags and border colours need Hyprland 0.56 or later with
  its Lua config (see [Desktop](../desktop.md)). Without a Wayland session
  (a server, SSH) zones start without it.
- xdg-desktop-portal, PipeWire for `audio`, unprivileged user namespaces,
  nftables kernel modules (see [Kernel modules](#kernel-modules)).
- For `dns = "host"`: the resolver is the first nameserver in
  `/etc/resolv.conf` (systemd-resolved, dnsmasq, ...), or
  `programs.appzones.dnsServer` on NixOS (`APPZONES_DNS_HOST` elsewhere).

Zone apps get the host session's `XDG_CURRENT_DESKTOP`.

## Kernel modules

A zone loads its nft deny list inside an unprivileged user namespace,
where the kernel does not autoload modules. A host with an nftables
firewall has them loaded already; otherwise load them yourself. On NixOS:

```nix
boot.kernelModules = [ "nf_tables" "nft_reject" "nft_reject_inet" "nf_reject_ipv4" "nf_reject_ipv6" ];
```

Elsewhere, now:

```bash
sudo modprobe -a nf_tables nft_reject nft_reject_inet nf_reject_ipv4 nf_reject_ipv6
```

and at every boot: with systemd, list them in `/etc/modules-load.d/appzones.conf`
(one per line); with another init, in its module list (`/etc/modules` on
Debian's sysvinit, Alpine and others).

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
