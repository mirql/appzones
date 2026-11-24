# nix-darwin module for appzones on macOS (design/macos.md; docs/getting-started/install.md).
#
# Installs `appzones` (nix/package-darwin.nix). Zones are Seatbelt sandboxes, started on
# demand by `appzones up`/`run`/`shell` (no launchd agent: autostart is not on macOS yet).
# The zones themselves are declared in ~/.config/appzones/zones.toml, which this module
# never generates.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.programs.appzones;
in
{
  options.programs.appzones = {
    enable = lib.mkEnableOption "app zones (`appzones`)";

    package = lib.mkOption {
      type = lib.types.package;
      # built with the system's own nixpkgs
      default = pkgs.callPackage ./package-darwin.nix { };
      defaultText = lib.literalExpression "pkgs.callPackage ./package-darwin.nix { }";
      description = "The appzones package.";
    };

    deniedPaths = lib.mkOption {
      # (only what /etc/appzones/denied-paths gives back unchanged: nix/denied-path.nix)
      type = lib.types.listOf (import ./denied-path.nix lib);
      default = [ ];
      example = [
        "/Volumes/Work"
        "~/Secrets"
      ];
      description = ''
        Paths no bind may be, contain or be inside, and no zone may read, on top of the
        built-in never-list (~/.ssh, ~/Library/Keychains, the browsers' profiles, ...).
        Absolute, or `~/` for the home. Written to /etc/appzones/denied-paths.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = pkgs.stdenv.hostPlatform.isDarwin && pkgs.stdenv.hostPlatform.isAarch64;
        message = "programs.appzones (nix-darwin): appzones on macOS is built for Apple silicon only";
      }
    ];

    environment.systemPackages = [ cfg.package ];

    environment.etc."appzones/denied-paths" = lib.mkIf (cfg.deniedPaths != [ ]) {
      text = lib.concatLines cfg.deniedPaths;
    };
  };
}
