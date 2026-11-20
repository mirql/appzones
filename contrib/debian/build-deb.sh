#!/bin/sh
# The .deb of appzones (amd64), from this checkout: `sh contrib/debian/build-deb.sh OUTDIR`.
# Build it on the oldest release it should run on (Debian 12: glibc 2.36, libwayland 1.21);
# `nix build .#deb` does that in a Debian 12 image (nix/deb.nix). Needs make, gcc,
# pkg-config, libwayland-dev, libwayland-bin, python3 and dpkg. Python and the C
# zone-wayland only: the optional Rust helpers are not packaged (yet, #121).
set -eu
out=$(realpath "${1:?usage: build-deb.sh OUTDIR}")
cd "$(dirname "$0")/../.."
version=$(sed -n 's/^  version = "\(.*\)";/\1/p' nix/package.nix)
[ -n "$version" ] || { echo "no version in nix/package.nix"; exit 1; }
root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT

make zone-wayland
[ -f build/zone-wayland ] || { echo "zone-wayland was not built (see above)"; exit 1; }
# -o helper: never the Rust helpers, whether or not cargo is here (the same package everywhere)
make -o helper install DESTDIR="$root" PREFIX=/usr PYTHON=/usr/bin/python3 HELPER= HELPER_BIN=build/none \
  SYSTEMD=yes APPARMOR=no SUDOERS=yes MOUNT_USERS=%appzones >/dev/null
install -m 755 contrib/debian/apparmor-profile "$root/usr/lib/appzones/apparmor-profile"
install -D -m 644 LICENSE "$root/usr/share/doc/appzones/copyright"

install -d -m 755 "$root/DEBIAN"
install -m 755 contrib/debian/postinst contrib/debian/postrm "$root/DEBIAN/"
echo /etc/sudoers.d/appzones > "$root/DEBIAN/conffiles"
size=$(du -sk "$root" | cut -f1)
cat > "$root/DEBIAN/control" <<CONTROL
Package: appzones
Version: $version-1
Architecture: amd64
Maintainer: Askar Amirov <59274133+askaramirov@users.noreply.github.com>
Installed-Size: $size
Depends: python3 (>= 3.11), bubblewrap, passt, nftables, iproute2, passwd, libc6 (>= 2.36), libwayland-client0 (>= 1.20)
Recommends: sudo, xdg-dbus-proxy, dbus
Suggests: apparmor
Section: admin
Priority: optional
Homepage: https://github.com/mirql/appzones
Description: long-lived sandboxes for desktop apps, dev tools and AI agents
 An app zone is a place apps live in, with its own home, its own network
 rules and its own window border colour, declared in one TOML file. Built on
 bubblewrap, pasta (passt) and nftables. Members of the group appzones may
 share folders with running zones (appzones mount, through a sudo rule for
 exactly its helper).
CONTROL

# the same bytes for the same source: file times from SOURCE_DATE_EPOCH (the CHANGELOG's by default)
epoch=${SOURCE_DATE_EPOCH:-$(stat -c %Y CHANGELOG.md)}
find "$root" -exec touch -h -d "@$epoch" {} +
mkdir -p "$out"
SOURCE_DATE_EPOCH=$epoch dpkg-deb --root-owner-group -Zxz --build "$root" "$out/appzones_${version}-1_amd64.deb"
