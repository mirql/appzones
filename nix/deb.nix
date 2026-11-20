# The .deb of appzones (amd64): contrib/debian/build-deb.sh in a Debian 12 image
# (nixpkgs vmTools, pinned packages, offline), so it links against the oldest
# glibc and libwayland it supports. Debian 12 and Ubuntu 24.04 install it
# (checks debian12-deb, ubuntu2404-deb).
{
  pkgs,
  # file times in the package (the flake's lastModified: the same bytes for one commit)
  epoch ? 1,
}:
let
  disk = pkgs.vmTools.diskImageFuns.debian12x86_64 {
    extraPackages = [
      "make"
      "gcc"
      "pkg-config"
      "libwayland-dev"
      "libwayland-bin"
      "python3"
      "sudo"
    ];
  };
in
pkgs.vmTools.runInLinuxImage (
  pkgs.runCommand "appzones-deb"
    {
      diskImage = disk;
      memSize = 1024;
      src = pkgs.lib.cleanSource ../.;
    }
    ''
      cp -r $src /tmp/appzones && chmod -R u+w /tmp/appzones
      SOURCE_DATE_EPOCH=${toString epoch} sh /tmp/appzones/contrib/debian/build-deb.sh $out
      dpkg-deb --info $out/*.deb
      dpkg-deb --contents $out/*.deb
    ''
)
