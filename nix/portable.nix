# appzones' Rust binaries for other distributions (the .deb, the RPM and the AUR package
# install these): linked against glibc 2.17's symbols with zig (cargo-zigbuild), so one
# build runs on every glibc distribution and uses its NSS (the mount helper looks up the
# caller's user and groups). /lib64/ld-linux-x86-64.so.2 as the loader, as built: not
# patched for the Nix store (no fixup).
{
  lib,
  rustPlatform,
  cargo-zigbuild,
  zig,
}:

rustPlatform.buildRustPackage {
  pname = "appzones-portable";
  version = (lib.importTOML ../helper/Cargo.toml).package.version;
  src = lib.cleanSource ../helper;
  cargoLock.lockFile = ../helper/Cargo.lock;
  nativeBuildInputs = [
    cargo-zigbuild
    zig
  ];
  buildPhase = ''
    runHook preBuild
    export HOME=$TMPDIR ZIG_GLOBAL_CACHE_DIR=$TMPDIR/zig-cache
    cargo zigbuild --release --offline --target x86_64-unknown-linux-gnu.2.17 \
      --bin appzones-helper --bin appzones-mount-helper
    runHook postBuild
  '';
  doCheck = false; # the tests run in nix/helper.nix's build, of the same code
  installPhase = ''
    runHook preInstall
    for b in appzones-helper appzones-mount-helper; do
      install -D -m 555 target/x86_64-unknown-linux-gnu/release/$b $out/bin/$b
    done
    # only glibc's symbols, none newer than 2.17
    for b in $out/bin/*; do
      ! $READELF -V $b | grep -o 'GLIBC_[0-9.]*' | sort -uV | awk -F'[_.]' '$2 > 2 || ($2 == 2 && $3 > 17)' | grep .
      $READELF -l $b | grep -q '/lib64/ld-linux-x86-64.so.2'
    done
    runHook postInstall
  '';
  dontFixup = true;
  meta = {
    description = "appzones' binaries for distributions other than NixOS (glibc 2.17 and later)";
    license = lib.licenses.mit;
    platforms = [ "x86_64-linux" ];
  };
}
