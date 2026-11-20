#!/usr/bin/env bash
# appzones on Arch Linux (`just test-arch`): the AUR package (contrib/arch/PKGBUILD), built
# with makepkg from this checkout inside an Arch VM, installed, then tests/distro-test.sh
# against it: zones as an ordinary user, and a desktop under sway (1.11+ names a zone
# from its security context). Unlike the Debian and Ubuntu checks this needs the network,
# since nixpkgs has no Arch images: a pinned cloud image (its sha256 checked) and the Arch
# Linux Archive of one date, so every run gets the same packages. Needs /dev/kvm. A real
# systemd host, unlike the other distro checks.
set -euo pipefail
IMAGE=Arch-Linux-x86_64-cloudimg-20261115.625457.qcow2
IMAGE_URL=https://geo.mirror.pkgbuild.com/images/v20261115.625457/$IMAGE
IMAGE_SHA256=dbd02830f6eb0fe07ef6e5edaddbae8b577ce4296d9e79118904704cce0a1f60
ARCHIVE=https://archive.archlinux.org/repos/2026/11/19

here=$(cd "$(dirname "$0")/.." && pwd)
cache=${XDG_CACHE_HOME:-$HOME/.cache}/appzones-test-arch
mkdir -p "$cache"
if ! echo "$IMAGE_SHA256  $cache/$IMAGE" | sha256sum -c --status 2>/dev/null; then
  echo "downloading $IMAGE"
  curl -fL --retry 3 -o "$cache/$IMAGE.part" "$IMAGE_URL"
  echo "$IMAGE_SHA256  $cache/$IMAGE.part" | sha256sum -c --status || { echo "sha256 mismatch: $IMAGE_URL"; exit 1; }
  mv "$cache/$IMAGE.part" "$cache/$IMAGE"
fi

work=$(mktemp -d)
# APPZONES_TEST_KEEP=1: on a failure the VM stays up, for a look (the command is printed)
cleanup() {
  if [ "${APPZONES_TEST_KEEP:-}" = 1 ] && [ "${ok:-}" != yes ]; then
    echo "kept: ssh -i $work/key -p $port tester@127.0.0.1 (then: kill \$(cat $work/qemu.pid))"; return
  fi
  [ -f "$work/qemu.pid" ] && kill "$(cat "$work/qemu.pid")" 2>/dev/null || true; rm -rf "$work"
}
trap cleanup EXIT
qemu-img create -q -f qcow2 -b "$cache/$IMAGE" -F qcow2 "$work/disk.qcow2" 20G
ssh-keygen -q -t ed25519 -N "" -f "$work/key"
# uid 2000: the test's own user is uid 1000
cat >"$work/user-data" <<UD
#cloud-config
users:
  - name: tester
    uid: "2000"
    shell: /bin/bash
    sudo: ALL=(ALL) NOPASSWD:ALL
    ssh_authorized_keys: ["$(cat "$work/key.pub")"]
UD
echo "instance-id: appzones-arch-test" >"$work/meta-data"
genisoimage -quiet -output "$work/seed.iso" -volid cidata -joliet -rock "$work/user-data" "$work/meta-data"
port=$(python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
qemu-system-x86_64 -enable-kvm -m 4096 -smp 4 -cpu host -display none \
  -drive file="$work/disk.qcow2",if=virtio -drive file="$work/seed.iso",media=cdrom \
  -nic user,model=virtio,hostfwd=tcp:127.0.0.1:"$port"-:22 \
  -serial file:"$work/serial.log" -daemonize -pidfile "$work/qemu.pid"
vm() { ssh -q -i "$work/key" -p "$port" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
  -o ConnectTimeout=5 tester@127.0.0.1 "$@"; }
for _ in $(seq 60); do vm true 2>/dev/null && break; sleep 5; done
vm true || { echo "the VM did not come up"; tail -20 "$work/serial.log"; exit 1; }

# the packages of one date, then what the package and the test need. The archive is a
# courtesy service: two downloads at a time, and the packages kept in the cache for the
# next run (only the first run downloads them)
if [ -f "$cache/pacman-pkg.tar" ]; then
  vm "sudo tar x -C /var/cache/pacman/pkg" <"$cache/pacman-pkg.tar"
fi
vm "echo 'Server = $ARCHIVE/\$repo/os/\$arch' | sudo tee /etc/pacman.d/mirrorlist >/dev/null && \
  sudo sed -i 's/^#\?ParallelDownloads.*/ParallelDownloads = 2/' /etc/pacman.conf && \
  sudo pacman -Syyuu --noconfirm --needed --disable-download-timeout base-devel python bubblewrap passt nftables iproute2 wayland \
  pkgconf xdg-dbus-proxy dbus sudo dnsmasq procps-ng psmisc util-linux weston wayland-utils sway foot \
  >/tmp/pacman.log 2>&1 || { tail -20 /tmp/pacman.log; exit 1; }"
vm "sudo tar c -C /var/cache/pacman/pkg ." >"$cache/pacman-pkg.tar.part" && mv "$cache/pacman-pkg.tar.part" "$cache/pacman-pkg.tar"

# the package from this checkout: a tarball of the tracked files, as GitHub's would be
version=$(sed -n 's/^  version = "\(.*\)";/\1/p' "$here/nix/package.nix")
(cd "$here" && git ls-files -z | tar --null -T - --transform "s,^,appzones-$version/," -czf "$work/src.tar.gz")
vm "mkdir -p ~/pkg"
cat "$work/src.tar.gz" | vm "cat >~/pkg/appzones-$version.tar.gz"
(cd "$here/contrib/arch" && tar c PKGBUILD appzones.install appzones.sysusers) | vm "tar x -C ~/pkg"
vm "cd ~/pkg && sed -i -e 's/^pkgver=.*/pkgver=$version/' \
  -e 's|^source=(.*|source=(\"appzones-\$pkgver.tar.gz\"|' -e \"s/^sha256sums=('[0-9a-f]*'/sha256sums=('SKIP'/\" PKGBUILD && \
  makepkg -f --noconfirm >/tmp/makepkg.log 2>&1 || { tail -20 /tmp/makepkg.log; exit 1; }; \
  sudo pacman -U --noconfirm ~/pkg/appzones-$version-*-x86_64.pkg.tar.zst >/dev/null"
echo "built and installed appzones $version with makepkg"

# the shared test, against the installed package
tar c -C "$here" tests/distro-test.sh | vm "tar x -C /tmp"
rc=0
vm "cd / && sudo timeout 900 sh /tmp/tests/distro-test.sh /nonexistent '' pacman" | tee "$work/test.log" || rc=$?
grep -qx "ALL PASSED" "$work/test.log" || { echo "test-arch: failed (status $rc)"; exit 1; }
ok=yes
