# appzones on Ubuntu 24.04 and Debian 12: their disk images from pinned .deb
# packages (nixpkgs vmTools), booted in a VM, run tests/distro-test.sh:
# `make install` (or appzones' .deb), then zones as an ordinary user. Offline, no systemd. The
# kernel is nixpkgs', so Ubuntu's AppArmor restriction on user namespaces is
# not in force here; the profile is only checked to parse.
{
  pkgs,
  image,
  # extra `make` and `make install` arguments (e.g. another helper implementation)
  makeFlags ? "",
  # the .deb (nix/deb.nix) to install instead of `make install`
  deb ? null,
}:
let
  # the VM's kernel modules: pasta's tun device, a dummy link for the test's
  # network, and what the zone's nft deny list uses (no autoload in a user namespace)
  vmTools = pkgs.vmTools.override {
    rootModules = [
      "virtio_pci"
      "virtio_mmio"
      "virtio_blk"
      "virtio_balloon"
      "virtio_rng"
      "ext4"
      "virtiofs"
      "crc32c"
      "tun"
      "dummy"
      "nf_tables"
      "nft_reject"
      "nft_reject_inet"
      "nf_reject_ipv4"
      "nf_reject_ipv6"
    ];
  };
  disk = vmTools.diskImageFuns.${image} {
    extraPackages = [
      "python3"
      "bubblewrap"
      "passt"
      "nftables"
      "xdg-dbus-proxy"
      "dbus"
      "make"
      "gcc"
      "pkg-config"
      "libwayland-dev"
      "libwayland-bin"
      "sudo"
      "iproute2"
      "procps"
      "psmisc"
      "util-linux"
      "mount"
      "bsdutils"
      "passwd"
      "login"
      "dnsmasq-base"
      "apparmor"
      # a compositor without the security-context protocol, and a client to ask it
      "weston"
      "wayland-utils"
      # a compositor with it (sway 1.9+; Debian 12's 1.7 has none), and a Wayland app
      "sway"
      "foot"
    ];
  };
in
vmTools.runInLinuxImage (
  pkgs.runCommand "appzones-on-${image}"
    {
      diskImage = disk;
      memSize = 2048;
      src = pkgs.lib.cleanSource ../.;
    }
    ''
      timeout 900 sh ${./distro-test.sh} $src ${pkgs.lib.escapeShellArg makeFlags} ${
        if deb == null then "" else "${deb}/appzones_*.deb"
      } > $NIX_BUILD_TOP/log 2>&1 || true
      cat $NIX_BUILD_TOP/log
      grep -qx "ALL PASSED" $NIX_BUILD_TOP/log
      mkdir -p $out && cp $NIX_BUILD_TOP/log $out/log
    ''
)
