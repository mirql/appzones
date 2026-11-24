# The nix-darwin module (nix/darwin-module.nix), on a Mac: a system with programs.appzones,
# built, never activated. `just test-macos-zones USER@MAC` builds it:
#   nix build --impure --no-link --expr '(import ./tests/darwin-module.nix).check'
# (needs the network once, for nix-darwin itself)
let
  appzones = builtins.getFlake (toString ../.);
  darwin = builtins.getFlake "github:nix-darwin/nix-darwin";
  sys = darwin.lib.darwinSystem {
    system = "aarch64-darwin";
    modules = [
      appzones.darwinModules.default
      {
        nixpkgs.hostPlatform = "aarch64-darwin";
        system.stateVersion = 6;
        programs.appzones.enable = true;
        programs.appzones.deniedPaths = [
          "~/Secrets"
          "/Volumes/Work"
        ];
      }
    ];
  };
  top = sys.config.system.build.toplevel;
  pkgs = appzones.inputs.nixpkgs.legacyPackages.aarch64-darwin;
in
{
  inherit top;
  # appzones in the system's path, and the host's denied paths written as given
  check = pkgs.runCommand "appzones-darwin-module" { } ''
    ${top}/sw/bin/appzones --version | grep -q '^appzones '
    printf '~/Secrets\n/Volumes/Work\n' | cmp - ${top}/etc/appzones/denied-paths
    touch $out
  '';
}
