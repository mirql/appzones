{
  description = "appzones: long-lived, easy to manage sandboxes for desktop apps on Linux (and macOS, in development)";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      # x86_64 only for now: nix_ld binds the x86_64 loader path, and nothing
      # has been tried on aarch64 yet
      systems = [ "x86_64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      # macOS (Apple silicon): the package, its module, the dev shell (design/macos.md)
      darwinSystems = [ "aarch64-darwin" ];
      forDarwin = f: nixpkgs.lib.genAttrs darwinSystems (system: f nixpkgs.legacyPackages.${system});
      devSystems = systems ++ darwinSystems;
    in
    {
      packages =
        forDarwin (pkgs: rec {
          # appzones on macOS: the binary and `appzones` (nix/package-darwin.nix)
          appzones = pkgs.callPackage ./nix/package-darwin.nix { };
          default = appzones;
          helper = appzones.helper;
        })
        // forAll (pkgs: rec {
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
        appzones = final.callPackage (
          if final.stdenv.hostPlatform.isDarwin then ./nix/package-darwin.nix else ./nix/package.nix
        ) { };
      };

      nixosModules = rec {
        appzones = ./nix/module.nix;
        default = appzones;
      };

      # nix-darwin: programs.appzones (nix/darwin-module.nix)
      darwinModules = rec {
        appzones = ./nix/darwin-module.nix;
        default = appzones;
      };

      checks =
        forDarwin (pkgs: {
          # the package, with the helper's tests in its build (on macOS)
          package = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
        })
        // forAll (
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
            # what holds the repository together: the docs catalog, the shell scripts
            repo = import ./tests/repo.nix { inherit pkgs; };
            # the modules' deniedPaths type: only entries /etc/appzones/denied-paths gives back unchanged
            denied-path =
              let
                t = import ./nix/denied-path.nix nixpkgs.lib;
                j = x: builtins.fromJSON ("\"" + x + "\"");
                ok = [
                  "/srv/code"
                  "~/Secrets"
                  "/My Folder/x"
                  "/Caf\\u00e9"
                  "/"
                ];
                refused = [
                  "/opt/Secrets#2026"
                  "/a\\nb"
                  "/Secrets "
                  "/a\\tb"
                  "/a\\rb"
                  "/tmp/Secrets\\u00a0"
                  "/tmp/a\\u2028/tmp/b"
                  "/a\\u0085b"
                  "relative"
                  "~user/x"
                ];
              in
              assert builtins.all (x: t.check (j x)) ok;
              assert !(builtins.any (x: t.check (j x)) refused);
              pkgs.runCommand "appzones-denied-path" { } "touch $out";
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
              pkgs.gitleaks
            ];
          };
        }
      );

      formatter = forAll (pkgs: pkgs.nixfmt-tree);
    };
}
