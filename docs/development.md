# Contributing

## Testing

Nothing is ever tested on the live desktop.

```bash
just test           # validator unit tests (no Nix)
just check FILE     # validate a zones.toml
just build          # the package (runs the unit tests too)
just test-vm        # NixOS VM: Hyprland, zone units, isolation, window tags (~6 min, /dev/kvm)
just test-install   # `make install` into a staging directory (for other distributions)
just test-distros   # Ubuntu 24.04 and Debian 12 in a VM: make install and the .deb, zones as a user (~3 min each)
just test-arch      # Arch: the AUR package built from this checkout, the same test (network, ~15 min)
just test-fedora    # Fedora 45, SELinux enforcing: the RPM from this checkout, the same test (network, ~15 min)
just test-opensuse  # openSUSE Leap 16, SELinux enforcing: make install, the same test (network, ~15 min)
just deb            # the .deb, in result-deb/
just test-rust      # the Rust helpers (helper/, design/rust.md): cargo test, protocol, never-list, VM, distros
just audit          # the Rust helpers' dependencies (cargo deny: advisories, licenses; needs the network)
just test-macos USER@MAC        # on a Mac over ssh (with Nix): this commit, clippy and cargo test
just test-macos-zones USER@MAC  # on a Mac: zones themselves, tests/macos.sh (~3 min)
```

`tests/macos.sh` runs on the Mac, in a temporary folder of the home (config, state, logs and
runtime directory: never the user's own zones). It starts a zone of each `network` kind and
runs `appzones check --probe` in each (every probe must pass: the home, `~/.ssh`, `/tmp`,
other volumes, the never-list, the zone's own mark, its server and zone-init, the machine's
identity; no network with `none`; with `internet` nothing around the proxy, no names, and
the proxy refusing the host, its LAN, loopback, private, tailnet and link-local ranges).
Then what the probes cannot see from inside: binds `rw`, `ro` and an `ro` one inside an `rw`
one; a zone's processes, detached ones too, ended by `stop`, by its server's end and by
zone-init's; a setting macOS cannot give refusing its zone alone.

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
  found" without it; `appzones check` refuses a zone with `nix_ld` on a host
  without nix-ld;
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

## Before a pull request

[CONTRIBUTING.md](https://github.com/mirql/appzones/blob/main/CONTRIBUTING.md) has the rules for
changes (tests, never-lists, the root helper, docs) and the layout of the code.
