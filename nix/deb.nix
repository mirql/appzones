# The .deb of appzones (amd64): contrib/debian/build-deb.sh with the binaries of
# nix/portable.nix (glibc 2.17 and later). Debian 12 and Ubuntu 24.04 install it (checks
# debian12-deb, ubuntu2404-deb).
{
  pkgs,
  portable,
  # file times in the package (the flake's lastModified: the same bytes for one commit)
  epoch ? 1,
}:
pkgs.runCommand "appzones-deb"
  {
    nativeBuildInputs = [
      pkgs.dpkg
      pkgs.gnumake
    ];
    src = pkgs.lib.cleanSource ../.;
  }
  ''
    cp -r $src tree && chmod -R u+w tree
    SOURCE_DATE_EPOCH=${toString epoch} HELPER_BIN=${portable}/bin/appzones-helper \
      MOUNT_HELPER_BIN=${portable}/bin/appzones-mount-helper sh tree/contrib/debian/build-deb.sh $out
    dpkg-deb --info $out/*.deb
    dpkg-deb --contents $out/*.deb > contents
    cat contents
    # ./ is the system's /: 0755, never mktemp's 0700
    head -1 contents | grep -q '^drwxr-xr-x root/root .* \./$'
  ''
