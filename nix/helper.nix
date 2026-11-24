# appzones' Rust binaries (helper/). By default appzones-helper, for the parts that run
# inside a zone, and zone-wayland: called with pkgsStatic (flake.nix), so it links
# statically. With bin = "appzones-mount-helper": the root step of `appzones mount`,
# called with the normal pkgs, so it links against glibc (the caller's user and groups
# come from NSS). The crates come from Cargo.lock, each by its checksum.
{
  lib,
  stdenv,
  rustPlatform,
  bin ? "appzones-helper",
}:

rustPlatform.buildRustPackage {
  pname = bin;
  version = (lib.importTOML ../helper/Cargo.toml).package.version;
  src = lib.cleanSource ../helper;
  cargoLock.lockFile = ../helper/Cargo.lock;
  cargoBuildFlags = [
    "--bin"
    bin
  ];
  cargoTestFlags = [
    "--bin"
    bin
  ]
  ++ lib.optionals (bin == "appzones-helper") [
    # the commands from the outside, in a temporary home
    "--test"
    "cli"
  ]
  ++ lib.optionals (bin == "appzones-helper" && stdenv.hostPlatform.isLinux) [
    # what the Python implementation answered, before it was removed (helper/tests/golden):
    # Linux's answers (macOS's never-list and plan differ on purpose)
    "--test"
    "golden"
    # the agent protocol of zone-init
    "--test"
    "agent"
  ];
  APPZONES_GOLDEN = "1";
  # (macOS: builds run in /nix/var/nix/builds, which is on the never-list, so the tests'
  # temporary homes there would be refused; /private/tmp is in Nix's sandbox paths there)
  preCheck = lib.optionalString stdenv.hostPlatform.isDarwin ''
    oldTmp=$TMPDIR
    checkTmp=$(mktemp -d /private/tmp/appzones-check.XXXXXX)
    export TMPDIR=$checkTmp
  '';
  postCheck = lib.optionalString stdenv.hostPlatform.isDarwin ''
    export TMPDIR=$oldTmp
    rm -rf "$checkTmp"
  '';
  stripAllList = [ "bin" ]; # the symbol table too, not only debug info
  meta = {
    description = "appzones' helpers in Rust (optional: design/rust.md)";
    license = lib.licenses.mit;
    platforms = [
      "x86_64-linux"
      "aarch64-darwin"
    ];
    mainProgram = bin;
  };
}
