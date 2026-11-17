# NixOS module for appzones (docs/guide.md).
#
# Installs the `zone` tool and the systemd user units that run a zone:
# zone@NAME.service (one long-lived sandbox per zone, in zones.slice) and
# zones-autostart.service (at login: launcher entries, zones with
# autostart = true). The zones themselves are declared in
# ~/.config/zones/zones.toml, which this module never generates.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.programs.appzones;
  zone = lib.getExe cfg.package;
  onlyUser = lib.optionalAttrs (cfg.user != null) { ConditionUser = cfg.user; };
in
{
  options.programs.appzones = {
    enable = lib.mkEnableOption "app zones (`zone`, zone@.service)";

    package = lib.mkOption {
      type = lib.types.package;
      # built with the system's own nixpkgs
      default = pkgs.callPackage ./package.nix { };
      defaultText = lib.literalExpression "pkgs.callPackage ./package.nix { }";
      description = "The appzones package (zone, zone-init, zone-wayland).";
    };

    mountHelper = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        `zone mount`: a sudo rule that lets `user` (every member of
        `users` when user is null) run exactly the zones mount helper as
        root without a password. The helper only mounts a directory the
        caller can read (never-list checked) at
        ~/.local/share/zones/ZONE/mnt/NAME (docs/guide.md "Runtime mounts").
      '';
    };

    user = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "alice";
      description = ''
        Only this user's manager runs the zone units (/etc/systemd/user is
        read by every user's manager). null: every user.
      '';
    };

    deniedPaths = lib.mkOption {
      type = lib.types.listOf (lib.types.strMatching "(/|~/).*");
      default = [ ];
      example = [
        "/srv/code"
        "~/Secrets"
      ];
      description = ''
        Host paths no bind or runtime mount may be, contain or be inside,
        on top of the built-in never-list (~/.ssh, ~/.gnupg, /run/secrets,
        shell and session config, ...). Absolute, or `~/` for the home.
        Written to /etc/zones/denied-paths.
      '';
    };

    dnsServer = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "127.0.0.1";
      description = ''
        The resolver zones with dns = "host" use. null: the first
        nameserver in the host's /etc/resolv.conf (systemd-resolved's
        127.0.0.53, a local dnsmasq, ...).
      '';
    };

    netbirdInterface = lib.mkOption {
      type = lib.types.str;
      default = "wt0";
      example = "wt1";
      description = ''
        The NetBird interface. A zone with netbird = true reaches its IPv4
        network, never the host's own address in it.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];

    environment.etc."zones/denied-paths" = lib.mkIf (cfg.deniedPaths != [ ]) {
      text = lib.concatLines cfg.deniedPaths;
    };

    # The one root step of `zone mount`/`zone umount` (and of a zone's start
    # and stop, which detach mounts left over). Only this file, any
    # arguments: the helper checks them itself. A zone cannot use it: sudo
    # needs a setuid wrapper, which no_new_privs disables and which is not
    # in a zone anyway.
    security.sudo.extraRules = lib.mkIf cfg.mountHelper [
      {
        users = lib.optional (cfg.user != null) cfg.user;
        groups = lib.optional (cfg.user == null) "users";
        runAs = "root"; # not the default ALL:ALL: as any other user it is no use

        commands = [
          {
            command = "${cfg.package}/${cfg.package.mountHelper}";
            options = [ "NOPASSWD" ];
          }
        ];
      }
    ];

    # Launcher entries "App (zone)" live in
    # ~/.local/share/zones/launcher/share/applications (`zone apps`), not in
    # ~/.local/share/applications (often managed by dotfiles). A profile adds
    # its share/ to XDG_DATA_DIRS (as flatpak's exports do).
    environment.profiles = [ "$HOME/.local/share/zones/launcher" ];

    systemd.user.slices.zones = {
      description = "App zones";
      unitConfig = onlyUser;
    };

    systemd.user.services."zone@" = {
      description = "App zone %i";
      documentation = [ "file://${cfg.package}/share/zones/zones.example.toml" ];
      # Stops with the session (the compositor gone: every Wayland client is
      # dead anyway); autostart brings the autostart zones back with it.
      partOf = [ "graphical-session.target" ];
      after = [ "graphical-session.target" ];
      unitConfig = onlyUser;
      environment = {
        ZONES_NETBIRD_IFACE = cfg.netbirdInterface;
      }
      // lib.optionalAttrs (cfg.dnsServer != null) { ZONES_DNS_HOST = cfg.dnsServer; };
      serviceConfig = {
        Type = "notify";
        NotifyAccess = "main";
        ExecStart = "${zone} _supervise %i";
        Slice = "zones.slice";
        # SIGTERM to the whole cgroup: apps save their state; then SIGKILL.
        # When bwrap (pid 1 of the zone) exits the kernel kills the rest.
        KillMode = "control-group";
        TimeoutStartSec = "5min"; # `packages` are built at start
        TimeoutStopSec = 20;
        Restart = "on-failure";
        RestartSec = 2;
      };
    };

    systemd.user.services.zones-autostart = {
      description = "App zones: launcher entries and autostart zones";
      wantedBy = [ "graphical-session.target" ];
      after = [ "graphical-session.target" ];
      partOf = [ "graphical-session.target" ];
      unitConfig = onlyUser // {
        ConditionPathExists = "%h/.config/zones/zones.toml";
      };
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        ExecStart = "${zone} _autostart";
      };
    };
  };
}
