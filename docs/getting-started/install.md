# Install

| Where | What works | How |
|---|---|---|
| **NixOS** | everything: desktop apps, window borders on Hyprland, `packages`, `nix_ld` | [the flake's module](#nixos) |
| **Ubuntu 24.04, Debian 12** (preview) | command-line zones: dev tools and AI agents, also over SSH; desktop apps are not tested yet | [`make install`](#ubuntu-and-debian-preview) |
| **Arch Linux** (preview) | the same as Ubuntu and Debian | [a PKGBUILD](#arch-linux-preview), or `make install` |

Other distributions with systemd or another init, Python 3.11 and the same
tools should work like Ubuntu and Debian; they are not tested yet.

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
(`just test-distros`). Ubuntu 22.04 is too old (Python 3.10).

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
appzones`. Then go on with step 3. The package has Python and the C
zone-wayland, not the Rust helpers: for those, install from a checkout. To
build it yourself: `nix build .#deb`, or `sh contrib/debian/build-deb.sh OUTDIR`
on Debian 12.

**Or from a checkout** (for the Rust helpers, or any commit):

**1. The packages.**

```bash
sudo apt install python3 bubblewrap passt nftables iproute2 git make # enough for a server
sudo apt install xdg-dbus-proxy dbus gcc pkg-config libwayland-dev   # for a desktop
```

**2. Install from a checkout.**

```bash
git clone https://github.com/mirql/appzones && cd appzones
make
sudo make install
```

The second line is the desktop's part: `make` builds `zone-wayland` (each
zone's own Wayland socket) when the build tools are there, `xdg-dbus-proxy`
gives a zone the filtered session bus (portals, notifications, the tray), and
`dbus-daemon` an app with `bus = "zone"` its own bus. Without `zone-wayland`
or `xdg-dbus-proxy` a zone starts without that part and says so in its log
(`appzones logs ZONE`). An app with `bus = "zone"` needs `dbus-daemon` (the
`dbus` package), also on a server: without it that app does not start, and
appzones says what is missing. Otherwise a server or a command-line zone needs
only the first line.
`sudo make install` puts:

- the code in `/usr/local/lib/appzones` and the `appzones` command in
  `/usr/local/bin`. It must be under `/usr`: a zone sees the host's `/usr`,
  not your home or `/opt`, and appzones runs a small part of itself inside
  each zone;
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
| `PREFIX` | `/usr/local` | anywhere under `/usr` |
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

**The Rust helpers (optional).** Everything that runs inside a zone (zone-init,
and the part of the `xdg-open` shim that hands links out), zone-wayland, and the
mount helper (the one step of `appzones mount` that runs as root, its own small
executable) also exist in Rust (`design/rust.md`). With them a zone gets a
read-only copy of the helper and no Python of the host's, so appzones can be
installed outside `/usr` too
(`make install HELPER=rust PREFIX=/opt/appzones`; the sudo rule for `appzones
mount` is written only when that directory and everything above it belong to
root alone).
To use them: `make` with cargo and rustc 1.85 or newer, then `sudo make install
HELPER=rust` (or `HELPER_BIN=PATH` and `MOUNT_HELPER_BIN=PATH` for helpers
built elsewhere; without a Rust mount helper, the Python one is installed); on
Nix, the package `appzones-rust`. The `appzones` command then runs the Rust
helper for what it replaces; the mount helper keeps its path, so the sudo rule
stays the same.

**Update and remove.** In the checkout: `git pull && make && sudo make
install` (with the same options as the first time, such as `HELPER=rust` or
`PREFIX`), then `appzones restart ZONE` for running zones. `sudo make uninstall`
removes everything `make install` put there; your zones' homes and settings
stay in `~/.local/share/appzones` and `~/.config/appzones`.

## Arch Linux (preview)

Tested by hand on Arch (kernel 7.3, Python 3.14): the same as on Ubuntu and
Debian, and no AppArmor profile is needed. From 0.8.2 on the package needs
only what a server needs; for a desktop also install `wayland`,
`xdg-dbus-proxy` and `dbus` (its optional dependencies). The package is on the AUR,
[`appzones`](https://aur.archlinux.org/packages/appzones) (its `PKGBUILD` is
kept in `contrib/arch/`):

```bash
paru -S appzones                     # or yay -S appzones, or by hand:
# git clone https://aur.archlinux.org/appzones.git && cd appzones && makepkg -si
systemctl --user daemon-reload
sudo gpasswd -a "$USER" appzones     # for `appzones mount`; then log in again
```

The package puts appzones under `/usr/lib/appzones`, the command in
`/usr/bin`, the systemd user units in `/usr/lib/systemd/user`, and a sudo rule
for the mount helper for the group `appzones` (created by sysusers), so only
the users you add to it may use `appzones mount`. `pacman -R appzones` removes
it all. Then [your first zone](#ubuntu-and-debian-preview) as on Ubuntu (step 3).
`make install` from a checkout works too, as on Debian.

## Requirements

- x86_64 Linux: NixOS, or Ubuntu 24.04, Debian 12, Arch (preview, above).
  Python 3.11 or newer.
- For desktop apps, a Wayland compositor with `wp_security_context_v1`
  (Hyprland, sway, KWin and others): every zone gets its own security
  context, and a zone does not start without it when there is a Wayland
  session. Window tags and border colours need Hyprland 0.56 or later with
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
