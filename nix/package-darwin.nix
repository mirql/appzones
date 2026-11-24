# appzones on macOS (Apple silicon; design/macos.md): the one Rust binary (nix/helper.nix,
# its tests with it) and `appzones`, its command line. Zones there are Seatbelt sandboxes
# (sandbox-exec, part of macOS), so no tools come with it, and there is no root step:
# `appzones mount` is not on macOS. The version is helper/Cargo.toml's, as nix/package.nix's
# (a release bumps both).
{
  lib,
  callPackage,
  runCommand,
  makeWrapper,
  helper ? callPackage ./helper.nix { },
}:

let
  version = helper.version;
  examples = ../examples;
in
runCommand "appzones-${version}"
  {
    nativeBuildInputs = [ makeWrapper ];
    passthru = { inherit helper; };
    meta = {
      description = "App zones: long-lived, easy to manage sandboxes (macOS: Seatbelt)";
      homepage = "https://github.com/mirql/appzones";
      license = lib.licenses.mit;
      platforms = [ "aarch64-darwin" ];
      mainProgram = "appzones";
    };
  }
  ''
    lib=$out/libexec/appzones
    mkdir -p $lib $out/bin $out/share/appzones
    install -m 555 ${helper}/bin/appzones-helper $lib/appzones-helper
    cp ${examples}/zones.toml $out/share/appzones/zones.example.toml
    cp ${examples}/minimal.toml $out/share/appzones/zones.minimal.toml
    # (nothing on PATH from the store: a zone's PATH is the system's and the user's,
    # seatbelt::path_dirs)
    makeWrapper $lib/appzones-helper $out/bin/appzones --add-flags cli
    $out/bin/appzones --version | grep -qx 'appzones ${version} (config release [0-9]*)'
  ''
