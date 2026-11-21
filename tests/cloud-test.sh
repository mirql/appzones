#!/usr/bin/env bash
# appzones on another distribution, in its cloud image (`just test-arch`, `just test-fedora`):
#   arch    the AUR package (contrib/arch/PKGBUILD), built with makepkg from this checkout
#   fedora  the RPM (contrib/fedora/appzones.spec), built with rpmbuild from this checkout
#           (SELinux enforcing); APPZONES_RPM_OUT=DIR copies it there
#   opensuse  `make install` from this checkout, on Leap 16 (SELinux enforcing)
# then tests/distro-test.sh against it: zones as an ordinary user, and a desktop under sway
# (1.11+ names a zone from its security context). Unlike the Debian and Ubuntu checks this
# needs the network, since nixpkgs has no images of these: a pinned cloud image (its
# sha256 checked) and fixed packages (Arch: the Arch Linux Archive of one date; Fedora:
# the release repositories, without updates), so every run gets the same ones. Needs
# /dev/kvm. A real systemd host, unlike the other distro checks.
set -euo pipefail
distro=${1:?usage: cloud-test.sh arch|fedora|opensuse}
case $distro in
  arch)
    IMAGE=Arch-Linux-x86_64-cloudimg-20261115.625457.qcow2
    IMAGE_URL=https://geo.mirror.pkgbuild.com/images/v20261115.625457/$IMAGE
    IMAGE_SHA256=dbd02830f6eb0fe07ef6e5edaddbae8b577ce4296d9e79118904704cce0a1f60
    ARCHIVE=https://archive.archlinux.org/repos/2026/11/19
    PKGCACHE=/var/cache/pacman/pkg;;
  fedora)
    IMAGE=Fedora-Cloud-Base-Generic-45-1.4.x86_64.qcow2
    IMAGE_URL=https://dl.fedoraproject.org/pub/fedora/linux/releases/45/Cloud/x86_64/images/$IMAGE
    IMAGE_SHA256=50bdc5e70f6a24f540932d15411c1c6ae9fcfa40fb990d876d83c5cc823b2ad0
    PKGCACHE=/var/cache/libdnf5;;
  opensuse)
    IMAGE=Leap-16.0-Minimal-VM.x86_64-Cloud-Build18.95.qcow2
    IMAGE_URL=https://download.opensuse.org/distribution/leap/16.0/appliances/$IMAGE
    IMAGE_SHA256=cdf1e5eefc20b7479059eb7ef12694e395d787168a11792997a0209499084ae5
    PKGCACHE=/var/cache/zypp/packages;;
  *) echo "usage: cloud-test.sh arch|fedora|opensuse"; exit 2;;
esac

here=$(cd "$(dirname "$0")/.." && pwd)
cache=${XDG_CACHE_HOME:-$HOME/.cache}/appzones-test-$distro
mkdir -p "$cache"
parts=() work=""
# APPZONES_TEST_KEEP=1: on a failure the VM stays up, for a look (the command is printed)
cleanup() {
  rm -f "${parts[@]}"  # staging files a failure left in the cache (completed ones were renamed)
  [[ -n "$work" ]] || return 0
  if [[ "${APPZONES_TEST_KEEP:-}" == 1 && "${ok:-}" != yes ]]; then
    echo "kept: ssh -i $work/key -p $port tester@127.0.0.1 (then: kill \$(cat $work/qemu.pid))"; return
  fi
  [[ -f "$work/qemu.pid" ]] && kill "$(cat "$work/qemu.pid")" 2>/dev/null || true; rm -rf "$work"
}
trap cleanup EXIT
if ! echo "$IMAGE_SHA256  $cache/$IMAGE" | sha256sum -c --status 2>/dev/null; then
  echo "downloading $IMAGE"
  part=$(mktemp "$cache/$IMAGE.XXXXXX")  # (each run its own: two runs never share a file)
  parts+=("$part")
  curl -fL --retry 3 --proto '=https' --proto-redir '=https' -o "$part" "$IMAGE_URL"
  echo "$IMAGE_SHA256  $part" | sha256sum -c --status || { rm -f "$part"; echo "sha256 mismatch: $IMAGE_URL"; exit 1; }
  mv "$part" "$cache/$IMAGE"
fi

work=$(mktemp -d)
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
echo "instance-id: appzones-$distro-test" >"$work/meta-data"
genisoimage -quiet -output "$work/seed.iso" -volid cidata -joliet -rock "$work/user-data" "$work/meta-data"
port=$(python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
qemu-system-x86_64 -enable-kvm -m 4096 -smp 4 -cpu host -display none \
  -drive file="$work/disk.qcow2",if=virtio -drive file="$work/seed.iso",media=cdrom \
  -nic user,model=virtio,hostfwd=tcp:127.0.0.1:"$port"-:22 \
  -serial file:"$work/serial.log" -daemonize -pidfile "$work/qemu.pid"
vm() { ssh -q -i "$work/key" -p "$port" -o IdentitiesOnly=yes -o IdentityAgent=none -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
  -o ConnectTimeout=5 tester@127.0.0.1 "$@"; }
for _ in $(seq 120); do vm true 2>/dev/null && break; sleep 5; done
vm true || { echo "the VM did not come up"; tail -20 "$work/serial.log"; exit 1; }

# fixed packages, then what the package and the test need. The package cache is kept for
# the next run (only the first run downloads; the Arch archive is a courtesy service)
if [[ -f "$cache/pkg.tar" ]]; then
  vm "sudo mkdir -p $PKGCACHE && sudo tar x -C $PKGCACHE" <"$cache/pkg.tar"
fi
if [[ $distro == arch ]]; then
  vm "echo 'Server = $ARCHIVE/\$repo/os/\$arch' | sudo tee /etc/pacman.d/mirrorlist >/dev/null && \
    sudo sed -i 's/^#\?ParallelDownloads.*/ParallelDownloads = 2/' /etc/pacman.conf && \
    sudo pacman -Syyuu --noconfirm --needed --disable-download-timeout base-devel rust python bubblewrap passt \
    nftables iproute2 xdg-dbus-proxy dbus sudo dnsmasq procps-ng psmisc util-linux weston \
    wayland-utils sway foot >/tmp/pkgs.log 2>&1 || { tail -20 /tmp/pkgs.log; exit 1; }"
elif [[ $distro == opensuse ]]; then
  # the release repositories only (no updates: the same packages every run), packages kept
  vm "for r in \$(sudo zypper -q lr -E | awk -F'|' 'NR>2 && tolower(\$2) ~ /update/ {gsub(/ /,\"\",\$2); print \$2}'); do \
      sudo zypper -q mr -d \"\$r\"; done; sudo zypper -q mr -k -a && \
    sudo zypper -n -q --gpg-auto-import-keys install --no-recommends gcc make cargo \
    python3 bubblewrap passt nftables iproute2 xdg-dbus-proxy dbus-1-daemon sudo dnsmasq procps psmisc \
    util-linux weston wayland-utils sway foot policycoreutils-python-utils >/tmp/pkgs.log 2>&1 \
    || { tail -20 /tmp/pkgs.log; exit 1; }"
else
  # (script, for the shell check, is its own package on Fedora)
  vm "echo keepcache=True | sudo tee -a /etc/dnf/dnf.conf >/dev/null && \
    sudo dnf -y -q --disablerepo='*' --enablerepo=fedora install rpm-build systemd-rpm-macros gcc make cargo rust \
    python3 bubblewrap passt nftables iproute xdg-dbus-proxy dbus-daemon sudo dnsmasq procps-ng psmisc \
    util-linux util-linux-script weston wayland-utils sway foot >/tmp/pkgs.log 2>&1 || { tail -20 /tmp/pkgs.log; exit 1; }"
fi
part=$(mktemp "$cache/pkg.tar.XXXXXX")
parts+=("$part")
vm "sudo tar c -C $PKGCACHE ." >"$part" && mv "$part" "$cache/pkg.tar"

version=$(sed -n 's/^  version = "\(.*\)";/\1/p' "$here/nix/package.nix")
if [[ $distro == arch ]]; then
  # the package from this checkout: a tarball of the tracked files, as GitHub's would be
  (cd "$here" && git ls-files -z | tar --null -T - --transform "s,^,appzones-$version/," -czf "$work/src.tar.gz")
  vm "mkdir -p ~/pkg"
  vm "cat >~/pkg/appzones-$version.tar.gz" <"$work/src.tar.gz"
  (cd "$here/contrib/arch" && tar c PKGBUILD appzones.install appzones.sysusers) | vm "tar x -C ~/pkg"
  vm "cd ~/pkg && sed -i -e 's/^pkgver=.*/pkgver=$version/' \
    -e 's|^source=(.*|source=(\"appzones-\$pkgver.tar.gz\"|' -e \"s/^sha256sums=('[0-9a-f]*'/sha256sums=('SKIP'/\" PKGBUILD && \
    makepkg -f --noconfirm >/tmp/makepkg.log 2>&1 || { tail -20 /tmp/makepkg.log; exit 1; }; \
    sudo pacman -U --noconfirm ~/pkg/appzones-$version-*-x86_64.pkg.tar.zst >/dev/null"
  echo "built and installed appzones $version with makepkg"
  mode=pacman src=/nonexistent
elif [[ $distro == opensuse ]]; then
  # Leap 16's pasta runs as passt (a link to the passt binary, passt_exec_t), which may not
  # build a network namespace: the documented workaround (docs: install, openSUSE)
  vm "sudo /usr/sbin/semanage permissive -a passt_t"
  # the checkout's tracked files; the test runs make and make install itself
  (cd "$here" && git ls-files -z | tar --null -T - -czf "$work/src.tar.gz")
  vm "mkdir -p /tmp/src && tar xz -C /tmp/src" <"$work/src.tar.gz"
  # the crates now, while the VM resolves names (the test points DNS at its own dnsmasq),
  # for root, who runs make in the test
  vm "cd /tmp/src/helper && sudo cargo fetch --locked >/tmp/fetch.log 2>&1 || { tail -5 /tmp/fetch.log; exit 1; }"
  mode="" src=/tmp/src
else
  # the RPM from this checkout: a tarball of the tracked files, as GitHub's would be
  (cd "$here" && git ls-files -z | tar --null -T - --transform "s,^,appzones-$version/," -czf "$work/src.tar.gz")
  vm "mkdir -p ~/rpmbuild/SOURCES ~/rpmbuild/SPECS"
  vm "cat >~/rpmbuild/SOURCES/appzones-$version.tar.gz" <"$work/src.tar.gz"
  vm "cat >~/rpmbuild/SPECS/appzones.spec" <"$here/contrib/fedora/appzones.spec"
  vm "cd ~/rpmbuild && sed -i 's/^Version: .*/Version:        $version/' SPECS/appzones.spec && \
    rpmbuild -bb SPECS/appzones.spec >/tmp/rpmbuild.log 2>&1 || { tail -30 /tmp/rpmbuild.log; exit 1; }; \
    sudo dnf -y -q --disablerepo='*' --enablerepo=fedora install ~/rpmbuild/RPMS/x86_64/appzones-$version-*.rpm >/dev/null"
  echo "built and installed appzones $version with rpmbuild"
  if [[ -n "${APPZONES_RPM_OUT:-}" ]]; then
    mkdir -p "$APPZONES_RPM_OUT"
    rpm=$(vm "ls ~/rpmbuild/RPMS/x86_64/appzones-$version-*.rpm")
    vm "cat $rpm" >"$APPZONES_RPM_OUT/$(basename "$rpm")"
    echo "the RPM: $APPZONES_RPM_OUT/$(basename "$rpm")"
  fi
  mode=rpm src=/nonexistent
fi

# the shared test, against the installed package
tar c -C "$here" tests/distro-test.sh | vm "tar x -C /tmp"
rc=0
vm "cd / && sudo timeout 900 sh /tmp/tests/distro-test.sh $src '' $mode" | tee "$work/test.log" || rc=$?
grep -qx "ALL PASSED" "$work/test.log" || { echo "test-$distro: failed (status $rc)"; exit 1; }
if [[ $distro == opensuse ]]; then  # the zone said why pasta may fail here, and how to fix it
  vm "sudo grep -q 'semanage permissive -a passt_t' /home/alice/.local/state/appzones/web.log" \
    || { echo "test-opensuse: no SELinux hint for pasta in the zone's log"; exit 1; }
  echo "the zone's log names the pasta/passt SELinux problem and its fix"
fi
ok=yes
