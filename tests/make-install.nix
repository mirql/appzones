# `make install` as on Ubuntu or Debian, into a staging directory (DESTDIR):
# every file in its place, the version filled in, zone-wayland built, the
# sudo rule valid, the AppArmor profile with real paths, and `make uninstall`
# removing it all again.
{ pkgs }:
pkgs.runCommand "appzones-make-install"
  {
    src = pkgs.lib.cleanSource ../.;
    nativeBuildInputs = with pkgs; [
      gnumake
      gcc
      pkg-config
      wayland
      wayland-scanner
      python3
      bubblewrap
      passt
      sudo # visudo
    ];
    buildInputs = [ pkgs.wayland ];
  }
  ''
    cp -r --no-preserve=mode $src src-tree && cd src-tree
    d=$PWD/stage
    make DESTDIR=$d SUDOERS=yes APPARMOR=yes SYSTEMD=yes MOUNT_USERS=alice install
    for f in usr/local/bin/appzones usr/local/lib/appzones/zone.py usr/local/lib/appzones/zone_init.py \
             usr/local/lib/appzones/zonecfg.py usr/local/lib/appzones/appzones/dns.py \
             usr/local/lib/appzones/appzones-mount-helper usr/local/lib/appzones/zone-wayland \
             usr/local/share/appzones/zones.example.toml usr/local/share/appzones/contrib/runit/appzones-ZONE/run \
             usr/local/lib/systemd/user/appzones@.service etc/sudoers.d/appzones etc/apparmor.d/appzones; do
      [ -e "$d/$f" ] || { echo "missing: $f"; exit 1; }
    done
    [ "$(stat -c %a $d/usr/local/lib/appzones/appzones-mount-helper)" = 555 ]
    head -1 $d/usr/local/lib/appzones/appzones-mount-helper | grep -q -- ' -IB$'
    grep -qx 'alice ALL=(root) NOPASSWD: /usr/local/lib/appzones/appzones-mount-helper' $d/etc/sudoers.d/appzones
    visudo -cqf $d/etc/sudoers.d/appzones
    ! grep -q '@BWRAP@\|@PASTA@' $d/etc/apparmor.d/appzones
    grep -q "profile appzones-bwrap $(readlink -f $(command -v bwrap))" $d/etc/apparmor.d/appzones
    v=$(python3 -B $d/usr/local/lib/appzones/zone.py --version)
    echo "$v"
    [ "$v" = "appzones $(sed -n 's/^  version = "\(.*\)";/\1/p' nix/package.nix) (config release $(sed -n 's/^RELEASE = //p' src/zonecfg.py))" ]
    make DESTDIR=$d install >/dev/null  # again: over an existing install
    make DESTDIR=$d SUDOERS=yes uninstall
    left=$(find $d -type f | grep -v -e '/etc/sudoers.d$' || true)
    [ -z "$left" ] || { echo "left after uninstall: $left"; exit 1; }
    ! make DESTDIR=$d PREFIX=/opt/appzones install 2>/dev/null  # a zone cannot see /opt
    touch $out
  ''
