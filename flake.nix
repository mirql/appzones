{
  description = "appzones: long-lived, easy to manage sandboxes for desktop apps on Linux";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      # x86_64 only for now: nix_ld binds the x86_64 loader path, and nothing
      # has been tried on aarch64 yet
      systems = [ "x86_64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAll (pkgs: rec {
        appzones = pkgs.callPackage ./nix/package.nix { };
        default = appzones;
      });

      overlays.default = final: _prev: {
        appzones = final.callPackage ./nix/package.nix { };
      };

      nixosModules = rec {
        appzones = ./nix/module.nix;
        default = appzones;
      };

      checks = forAll (
        pkgs:
        {
          # the unit tests run inside the package build
          package = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
          # `make install` (outside Nix) into a staging directory
          make-install = import ./tests/make-install.nix { inherit pkgs; };
          # appzones on other distributions, in their disk images (offline, ~3 min each)
          debian12 = import ./tests/distro.nix {
            inherit pkgs;
            image = "debian12x86_64";
          };
          ubuntu2404 = import ./tests/distro.nix {
            inherit pkgs;
            image = "ubuntu2404x86_64";
          };
        }
        // {
          # NixOS VM test (needs /dev/kvm, ~6 min)
          vm = import ./tests/vm.nix {
            inherit pkgs;
            module = self.nixosModules.default;
          };
        }
      );

      # The README's terminal demos, recorded in a VM: `just demo`
      legacyPackages = forAll (pkgs: {
        demo = import ./tests/demo.nix {
          inherit pkgs;
          module = self.nixosModules.default;
        };
      });

      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          packages = [
            pkgs.python3
            pkgs.just
            pkgs.nixfmt
          ];
        };
      });

      formatter = forAll (pkgs: pkgs.nixfmt-tree);
    };
}
