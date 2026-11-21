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
      # the dev shell also on macOS (Apple silicon): the macOS port (design/macos.md)
      devSystems = systems ++ [ "aarch64-darwin" ];
    in
    {
      packages = forAll (pkgs: rec {
        # appzones: one Rust binary (helper/), and the root step of `appzones mount`
        appzones = pkgs.callPackage ./nix/package.nix { };
        default = appzones;
        # the binary alone (static, with musl), its tests run in its build
        helper = appzones.helper;
        mount-helper = appzones.mountHelperPackage;
        # both for other distributions (glibc 2.17 and later): the .deb, the RPM, the AUR
        portable = pkgs.callPackage ./nix/portable.nix { };
        # the .deb for Debian 12 and Ubuntu 24.04
        deb = import ./nix/deb.nix {
          inherit pkgs portable;
          epoch = self.lastModified or 1;
        };
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
        let
          p = self.packages.${pkgs.stdenv.hostPlatform.system};
          bins = "HELPER_BIN=${p.portable}/bin/appzones-helper MOUNT_HELPER_BIN=${p.portable}/bin/appzones-mount-helper";
        in
        {
          package = p.default;
          # cargo test in the build: the units, the commands from the outside (tests/cli.rs),
          # the agent protocol (tests/agent.rs), Python's answers (tests/golden.rs)
          helper = p.helper;
          mount-helper = p.mount-helper;
          portable = p.portable;
          # `make install` (outside Nix) into a staging directory
          make-install = import ./tests/make-install.nix {
            inherit pkgs;
            helper = p.helper;
            mountHelper = p.mount-helper;
          };
          # what holds the repository together: the Hyprland hook's pre-0.6 copy, the docs catalog
          repo = import ./tests/repo.nix { inherit pkgs; };
          # appzones on other distributions, in their disk images (offline, ~3 min each):
          # `make install` with the portable binaries (Ubuntu's outside /usr), and the .deb
          debian12 = import ./tests/distro.nix {
            inherit pkgs;
            image = "debian12x86_64";
            makeFlags = bins;
          };
          ubuntu2404 = import ./tests/distro.nix {
            inherit pkgs;
            image = "ubuntu2404x86_64";
            makeFlags = "PREFIX=/opt/appzones ${bins}";
          };
          debian12-deb = import ./tests/distro.nix {
            inherit pkgs;
            image = "debian12x86_64";
            deb = p.deb;
          };
          ubuntu2404-deb = import ./tests/distro.nix {
            inherit pkgs;
            image = "ubuntu2404x86_64";
            deb = p.deb;
          };
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

      devShells = nixpkgs.lib.genAttrs devSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell {
            packages = [
              pkgs.cargo
              pkgs.rustc
              pkgs.clippy
              pkgs.rustfmt
              pkgs.just
              pkgs.nixfmt
            ];
          };
        }
      );

      formatter = forAll (pkgs: pkgs.nixfmt-tree);
    };
}
