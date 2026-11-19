# appzones-helper (helper/): one static binary (musl) for the parts that run inside
# a zone, and zone-wayland. Called with pkgsStatic (flake.nix), so it links
# statically. The crates come from Cargo.lock, each by its checksum.
{ lib, rustPlatform }:

rustPlatform.buildRustPackage {
  pname = "appzones-helper";
  version = (lib.importTOML ../helper/Cargo.toml).package.version;
  src = lib.cleanSource ../helper;
  cargoLock.lockFile = ../helper/Cargo.lock;
  stripAllList = [ "bin" ]; # the symbol table too, not only debug info
  meta = {
    description = "appzones' helpers in Rust (experiment: design/rust.md)";
    license = lib.licenses.mit;
    platforms = [ "x86_64-linux" ];
    mainProgram = "appzones-helper";
  };
}
