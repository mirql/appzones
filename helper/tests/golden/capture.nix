# The golden files (tests/golden/capture.py), captured in a build sandbox:
#   nix build --impure --expr 'import ./helper/tests/golden/capture.nix { pkgs = (builtins.getFlake (toString ./.)).inputs.nixpkgs.legacyPackages.x86_64-linux; src = ./.; }'
{ pkgs, src }:
pkgs.runCommand "appzones-golden" { nativeBuildInputs = [ pkgs.python3 ]; } ''
  cp -r ${src} tree && chmod -R u+w tree && cd tree/src
  HOME=$TMPDIR python3 -B ../helper/tests/golden/capture.py . $out
''
