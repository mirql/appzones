#!/bin/sh
# The .deb of appzones (amd64), from this checkout: `sh contrib/debian/build-deb.sh OUTDIR`.
# Its two binaries: HELPER_BIN and MOUNT_HELPER_BIN when set (`nix build .#deb` gives
# nix/portable.nix's, which run on glibc 2.17 and later), else built here with cargo (they
# then need this machine's glibc or newer, and the package says so). Needs make, readelf and dpkg-deb.
set -eu
out=$(realpath "${1:?usage: build-deb.sh OUTDIR}")
cd "$(dirname "$0")/../.."
version=$(sed -n 's/^  version = "\(.*\)";/\1/p' nix/package.nix)
[ -n "$version" ] || { echo "no version in nix/package.nix"; exit 1; }
root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT
chmod 755 "$root"  # the package's ./ is /: never 0700, as mktemp makes it

# /usr/lib/appzones, as in every .deb so far (the sudo rule, a conffile, names it)
make install DESTDIR="$root" PREFIX=/usr LIBDIR=/usr/lib/appzones HELPER_BIN="${HELPER_BIN:-build/appzones-helper}" \
  MOUNT_HELPER_BIN="${MOUNT_HELPER_BIN:-build/appzones-mount-helper}" \
  SYSTEMD=yes APPARMOR=no SUDOERS=yes MOUNT_USERS=%appzones >/dev/null
install -m 755 contrib/debian/apparmor-profile "$root/usr/lib/appzones/apparmor-profile"
install -D -m 644 LICENSE "$root/usr/share/doc/appzones/copyright"

install -d -m 755 "$root/DEBIAN"
install -m 755 contrib/debian/postinst contrib/debian/postrm "$root/DEBIAN/"
echo /etc/sudoers.d/appzones > "$root/DEBIAN/conffiles"
size=$(du -sk "$root" | cut -f1)
# libc6: the newest glibc symbol version the binaries use (2.17 for nix/portable.nix's; a
# cargo build here needs this machine's), never less than 2.17
glibc=$(readelf -V "$root/usr/lib/appzones/appzones-helper" "$root/usr/lib/appzones/appzones-mount-helper" 2>/dev/null \
  | grep -o 'GLIBC_[0-9][0-9.]*' | sed 's/GLIBC_//' | { cat; echo 2.17; } | sort -V | tail -1)
cat > "$root/DEBIAN/control" <<CONTROL
Package: appzones
Version: $version-1
Architecture: amd64
Maintainer: Askar Amirov <59274133+askaramirov@users.noreply.github.com>
Installed-Size: $size
Depends: bubblewrap, passt, nftables, iproute2, passwd, libc6 (>= $glibc)
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
# (with the binaries' own dates too: dpkg-deb packs what it finds)
mkdir -p "$out"
SOURCE_DATE_EPOCH=$epoch dpkg-deb --root-owner-group -Zxz --build "$root" "$out/appzones_${version}-1_amd64.deb"
