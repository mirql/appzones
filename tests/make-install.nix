# `make install` as on Ubuntu or Debian, into a staging directory (DESTDIR), with the
# binaries given (cargo has no network here): every file in its place, the sudo rule
# valid, the AppArmor profile with real paths, `make uninstall` removing it all again.
{
  pkgs,
  helper,
  mountHelper,
}:
pkgs.runCommand "appzones-make-install"
  {
    src = pkgs.lib.cleanSource ../.;
    nativeBuildInputs = with pkgs; [
      gnumake
      bubblewrap
      passt
      sudo # visudo
    ];
  }
  ''
    cp -r --no-preserve=mode $src src-tree && cd src-tree
    bins="HELPER_BIN=${helper}/bin/appzones-helper MOUNT_HELPER_BIN=${mountHelper}/bin/appzones-mount-helper"
    d=$PWD/stage
    make DESTDIR=$d SUDOERS=yes APPARMOR=yes SYSTEMD=yes MOUNT_USERS=alice $bins install
    for f in usr/local/bin/appzones usr/local/libexec/appzones/appzones-helper usr/local/libexec/appzones/appzones-mount-helper \
             usr/local/share/appzones/zones.example.toml usr/local/share/appzones/contrib/runit/appzones-ZONE/run \
             usr/local/share/appzones/contrib/sway/appzones-sway \
             usr/local/lib/systemd/user/appzones@.service etc/sudoers.d/appzones etc/apparmor.d/appzones; do
      [ -e "$d/$f" ] || { echo "missing: $f"; exit 1; }
    done
    [ "$(stat -c %a $d/usr/local/libexec/appzones/appzones-mount-helper)" = 555 ]
    cmp ${mountHelper}/bin/appzones-mount-helper $d/usr/local/libexec/appzones/appzones-mount-helper
    grep -qx 'exec /usr/local/libexec/appzones/appzones-helper cli "$@"' $d/usr/local/bin/appzones
    grep -qx 'alice ALL=(root) NOPASSWD: /usr/local/libexec/appzones/appzones-mount-helper' $d/etc/sudoers.d/appzones
    visudo -cqf $d/etc/sudoers.d/appzones
    if grep -q '@BWRAP@\|@PASTA@' $d/etc/apparmor.d/appzones; then echo "AppArmor placeholders left"; exit 1; fi
    grep -qx 'ExecStart=/usr/local/bin/appzones serve %i' $d/usr/local/lib/systemd/user/appzones@.service
    grep -q "profile appzones-bwrap $(readlink -f $(command -v bwrap))" $d/etc/apparmor.d/appzones
    v=$($d/usr/local/libexec/appzones/appzones-helper cli --version)
    echo "$v"
    [ "$v" = "appzones $(sed -n 's/^  version = "\(.*\)";/\1/p' nix/package.nix) (config release $(sed -n 's/^pub const RELEASE: [a-z0-9]* = \([0-9]*\);/\1/p' helper/src/config.rs))" ]
    make DESTDIR=$d $bins install >/dev/null  # again: over an existing install
    make DESTDIR=$d SUDOERS=yes uninstall
    left=$(find $d -type f | grep -v -e '/etc/sudoers.d$' || true)
    [ -z "$left" ] || { echo "left after uninstall: $left"; exit 1; }
    # anywhere (a zone gets the binary bound in), but a staged install outside /usr gets no
    # sudo rule (root's file must sit where only root writes)
    make DESTDIR=$PWD/stage3 PREFIX=/opt/appzones APPARMOR=no SYSTEMD=no $bins install > opt.log
    grep -q "no sudo rule" opt.log
    [ -x stage3/opt/appzones/libexec/appzones/appzones-helper ] && [ ! -e stage3/etc/sudoers.d/appzones ]
    # the check is on where the mount helper goes (LIBDIR), whatever PREFIX says
    make DESTDIR=$PWD/stage5 PREFIX=/usr LIBDIR=/home/alice/az APPARMOR=no SYSTEMD=no $bins install > lib.log
    grep -q "no sudo rule" lib.log && [ ! -e stage5/etc/sudoers.d/appzones ]
    # and LIBDIR is a plain absolute path
    if make DESTDIR=$PWD/stage6 LIBDIR=lib/az $bins install >/dev/null 2>&1; then echo "a relative LIBDIR accepted"; exit 1; fi
    # PREFIX=/usr (a distribution package): the units where systemd looks for them; LIBDIR its choice
    make DESTDIR=$PWD/stage2 PREFIX=/usr LIBDIR=/usr/lib/appzones SYSTEMD=yes SUDOERS=yes MOUNT_USERS=%appzones \
      APPARMOR=no $bins install >/dev/null
    grep -qx 'ExecStart=/usr/bin/appzones serve %i' stage2/usr/lib/systemd/user/appzones@.service
    grep -qx 'exec /usr/lib/appzones/appzones-helper cli "$@"' stage2/usr/bin/appzones
    grep -qx '%appzones ALL=(root) NOPASSWD: /usr/lib/appzones/appzones-mount-helper' stage2/etc/sudoers.d/appzones
    # no binaries and no cargo: said so, nothing installed
    if PATH=${pkgs.gnumake}/bin:${pkgs.coreutils}/bin make DESTDIR=$PWD/stage4 install > none.log 2>&1; then
      echo "installed without binaries"; exit 1
    fi
    grep -q "needs cargo" none.log && [ ! -e stage4 ]
    touch $out
  ''
