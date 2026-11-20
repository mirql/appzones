# appzones' Rust binaries (helper/). By default appzones-helper, for the parts that run
# inside a zone, and zone-wayland: called with pkgsStatic (flake.nix), so it links
# statically. With bin = "appzones-mount-helper": the root step of `appzones mount`,
# called with the normal pkgs, so it links against glibc (the caller's user and groups
# come from NSS). The crates come from Cargo.lock, each by its checksum.
{
  lib,
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
  ];
  stripAllList = [ "bin" ]; # the symbol table too, not only debug info
  meta = {
    description = "appzones' helpers in Rust (optional: design/rust.md)";
    license = lib.licenses.mit;
    platforms = [ "x86_64-linux" ];
    mainProgram = bin;
  };
}
