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
        # the Rust helpers (optional, design/rust.md): static, with musl
        helper = pkgs.pkgsStatic.callPackage ./nix/helper.nix { };
        # appzones with them
        # the root step of `appzones mount` in Rust: linked against glibc, not static
        mount-helper = pkgs.callPackage ./nix/helper.nix { bin = "appzones-mount-helper"; };
        appzones-rust = appzones.override {
          inherit helper;
          rustMountHelper = mount-helper;
        };
        # the .deb for Debian 12 and Ubuntu 24.04 (built in a Debian 12 image)
        deb = import ./nix/deb.nix {
          inherit pkgs;
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
        {
          # the unit tests run inside the package build
          package = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
          # `make install` (outside Nix) into a staging directory
          make-install = import ./tests/make-install.nix { inherit pkgs; };
          # the Rust helper builds, statically, and its tests pass (cargo test in the build)
          helper = self.packages.${pkgs.stdenv.hostPlatform.system}.helper;
          mount-helper = self.packages.${pkgs.stdenv.hostPlatform.system}.mount-helper;
          # the Rust zone-init meets the agent protocol's specification (AgentProtocolTest)
          agent-protocol-rust =
            pkgs.runCommand "appzones-agent-protocol-rust"
              {
                nativeBuildInputs = [ pkgs.python3 ];
                src = ./.;
              }
              ''
                cp -r --no-preserve=mode $src/src $src/examples $src/contrib . && cd src
                APPZONES_TEST_ZONE_INIT="${
                  self.packages.${pkgs.stdenv.hostPlatform.system}.helper
                }/bin/appzones-helper zone-init" \
                  HOME=$TMPDIR python3 -B -m unittest -v test_zonecfg.AgentProtocolTest
                touch $out
              '';
          # the never-list in Rust (the root step's) answers as zonecfg's, path by path
          policy-rust =
            pkgs.runCommand "appzones-policy-rust"
              {
                nativeBuildInputs = [ pkgs.python3 ];
                src = ./.;
              }
              ''
                cp -r --no-preserve=mode $src/src $src/examples $src/contrib . && cd src
                APPZONES_TEST_HELPER="${
                  self.packages.${pkgs.stdenv.hostPlatform.system}.helper
                }/bin/appzones-helper" HOME=$TMPDIR python3 -B -m unittest -v test_zonecfg.DifferentialPolicyTest
                touch $out
              '';
          # appzones on other distributions, in their disk images (offline, ~3 min each)
          debian12 = import ./tests/distro.nix {
            inherit pkgs;
            image = "debian12x86_64";
          };
          ubuntu2404 = import ./tests/distro.nix {
            inherit pkgs;
            image = "ubuntu2404x86_64";
          };
          # the same with the .deb (nix/deb.nix) instead of `make install`
          debian12-deb = import ./tests/distro.nix {
            inherit pkgs;
            image = "debian12x86_64";
            deb = self.packages.${pkgs.stdenv.hostPlatform.system}.deb;
          };
          ubuntu2404-deb = import ./tests/distro.nix {
            inherit pkgs;
            image = "ubuntu2404x86_64";
            deb = self.packages.${pkgs.stdenv.hostPlatform.system}.deb;
          };
          # the same with the Rust helper (the static build), installed outside /usr
          debian12-rust = import ./tests/distro.nix {
            inherit pkgs;
            image = "debian12x86_64";
            makeFlags = "PREFIX=/opt/appzones HELPER=rust HELPER_BIN=${
              self.packages.${pkgs.stdenv.hostPlatform.system}.helper
            }/bin/appzones-helper MOUNT_HELPER_BIN=${
              self.packages.${pkgs.stdenv.hostPlatform.system}.mount-helper
            }/bin/appzones-mount-helper";
          };
          ubuntu2404-rust = import ./tests/distro.nix {
            inherit pkgs;
            image = "ubuntu2404x86_64";
            makeFlags = "PREFIX=/opt/appzones HELPER=rust HELPER_BIN=${
              self.packages.${pkgs.stdenv.hostPlatform.system}.helper
            }/bin/appzones-helper MOUNT_HELPER_BIN=${
              self.packages.${pkgs.stdenv.hostPlatform.system}.mount-helper
            }/bin/appzones-mount-helper";
          };
        }
        // {
          # NixOS VM test (needs /dev/kvm, ~6 min)
          vm = import ./tests/vm.nix {
            inherit pkgs;
            module = self.nixosModules.default;
          };
          # the same VM test with the Rust helpers
          vm-rust = import ./tests/vm.nix {
            inherit pkgs;
            module = self.nixosModules.default;
            package = self.packages.${pkgs.stdenv.hostPlatform.system}.appzones-rust;
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
