# appzones: one Rust binary (helper/, built by nix/helper.nix, its tests with it): `appzones`
# is its command line, and the same binary is the server, zone-init inside a zone and
# zone-wayland; the root step of `appzones mount` is its own. The tools a zone needs at
# run time (bwrap, pasta, nft, xdg-dbus-proxy, dbus-daemon for a zone's own bus, ip) come
# from the store, so the package works whatever is on the caller's PATH.
{
  lib,
  callPackage,
  pkgsStatic,
  runCommand,
  makeWrapper,
  bubblewrap,
  passt,
  nftables,
  xdg-dbus-proxy,
  dbus,
  iproute2,
  procps,
  # static, with musl: it runs inside zones too, whatever their libraries
  helper ? pkgsStatic.callPackage ./helper.nix { },
  # linked against glibc: the caller's user and groups come from NSS
  mountHelper ? callPackage ./helper.nix { bin = "appzones-mount-helper"; },
}:

let
  # the package version (helper/Cargo.toml's too); the config schema has its own release
  # number (config::RELEASE, what a zones.toml can require)
  version = "0.11.3";
  runtimePath = lib.makeBinPath [
    bubblewrap
    passt
    nftables
    xdg-dbus-proxy
    dbus
    iproute2
    procps
  ];
  examples = ../examples;
  contrib = ../contrib; # service files for other process managers, in share/appzones/contrib
in
assert helper.version == version;
runCommand "appzones-${version}"
  {
    nativeBuildInputs = [ makeWrapper ];
    passthru = {
      inherit helper;
      mountHelperPackage = mountHelper;
      # (the module's sudo rule, relative to the package)
      mountHelper = "libexec/appzones/appzones-mount-helper";
    };
    meta = {
      description = "App zones: long-lived, easy to manage sandboxes for desktop apps (bubblewrap + pasta)";
      homepage = "https://github.com/mirql/appzones";
      license = lib.licenses.mit;
      platforms = [ "x86_64-linux" ];
      mainProgram = "appzones";
    };
  }
  ''
    lib=$out/libexec/appzones
    mkdir -p $lib $out/bin $out/share/appzones
    install -m 555 ${helper}/bin/appzones-helper $lib/appzones-helper
    # the module's sudo rule names this file
    install -m 555 ${mountHelper}/bin/appzones-mount-helper $lib/appzones-mount-helper
    cp ${examples}/zones.toml $out/share/appzones/zones.example.toml
    cp ${examples}/minimal.toml $out/share/appzones/zones.minimal.toml
    cp -r --no-preserve=mode ${contrib} $out/share/appzones/contrib
    chmod 755 $out/share/appzones/contrib/{runit,s6}/appzones-ZONE/{run,finish} $out/share/appzones/contrib/sway/appzones-sway
    # /run/current-system/sw/bin last: systemctl, journalctl, nix and hyprctl are the
    # running system's
    makeWrapper $lib/appzones-helper $out/bin/appzones \
      --add-flags cli \
      --prefix PATH : ${runtimePath} \
      --suffix PATH : /run/current-system/sw/bin
    $out/bin/appzones --version | grep -qx 'appzones ${version} (config release [0-9]*)'
  ''
