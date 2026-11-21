# This helper's answers as golden files, written in the same Nix build sandbox the golden
# test runs in (nix/helper.nix). Only for an answer changed on purpose:
#   nix build --impure --expr 'let f = builtins.getFlake (toString ./.); p = f.inputs.nixpkgs.legacyPackages.x86_64-linux; in p.pkgsStatic.callPackage ./helper/tests/golden/update.nix {}'
# then diff result/*.json against helper/tests/golden/ (every change: on purpose?) and copy them.
{ lib, rustPlatform }:
rustPlatform.buildRustPackage {
  pname = "appzones-golden-update";
  version = (lib.importTOML ../../Cargo.toml).package.version;
  src = lib.cleanSource ../..;
  cargoLock.lockFile = ../../Cargo.lock;
  buildPhase = "true";
  checkPhase = ''
    mkdir -p $out
    APPZONES_GOLDEN=1 APPZONES_GOLDEN_WRITE=$out cargo test --release --offline --test golden -- --test-threads 1
  '';
  installPhase = "true";
}
