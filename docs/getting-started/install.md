# Install

On NixOS, with the flake's module:

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

## Other distributions (preview)

Ubuntu 24.04 and Debian 12 run command-line zones (dev tools, AI agents, also
over SSH); desktop apps there are not tested yet. From a checkout:

```bash
sudo apt install python3 bubblewrap passt nftables xdg-dbus-proxy \
  make gcc pkg-config libwayland-dev    # the last three: to build zone-wayland, for desktop apps
make
sudo make install        # PREFIX=/usr/local; `sudo make uninstall` removes it all
```

`make install` puts appzones under `/usr/local/lib/appzones` (it must be
under `/usr`: a zone sees the host's `/usr`, not your home or `/opt`), the
`appzones` command in `/usr/local/bin`, the systemd user units where there
is systemd, and a sudo rule for `appzones mount` (for the user who ran sudo;
`MOUNT_USERS=%group` for a group, `SUDOERS=no` for none). On Ubuntu 23.10 and
later it also installs an AppArmor profile that lets bwrap and pasta create
user namespaces, which Ubuntu otherwise blocks for every zone. `packages`
needs Nix; `nix_ld` is not needed outside NixOS.

Requirements:

- x86_64 Linux: NixOS, or Ubuntu 24.04 / Debian 12 (preview, above). Python
  3.11 or newer.
- A Wayland compositor with `wp_security_context_v1` (Hyprland, sway,
  KWin and others): every zone gets its own security context, and a zone
  does not start without it. Window tags and border colours need Hyprland
  0.56 or later with its Lua config (see [Desktop](../desktop.md)).
- xdg-desktop-portal, PipeWire for `audio`, unprivileged user namespaces,
  nftables kernel modules (see [Kernel modules](#kernel-modules)).
- For `dns = "host"`: the resolver is the first nameserver in
  `/etc/resolv.conf` (systemd-resolved, dnsmasq, ...), or
  `programs.appzones.dnsServer`.

Zone apps get the host session's `XDG_CURRENT_DESKTOP`.

## Kernel modules

A zone loads its nft deny list inside an unprivileged user namespace,
where the kernel does not autoload modules. A host with an nftables
firewall has them loaded already; otherwise load them yourself:

```nix
boot.kernelModules = [ "nf_tables" "nft_reject" "nft_reject_inet" "nf_reject_ipv4" "nf_reject_ipv6" ];
```

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
