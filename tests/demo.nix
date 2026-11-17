# The README's terminal demos, recorded in a NixOS VM (never on a live
# session). Each script is typed into a real pty, key by key, and what the
# terminal shows is saved as an asciicast (tests/demo-record.py); agg
# renders each to a GIF. The commands and their output are real: the
# zone's home, the refused LAN address, the host's dev server, the read-only
# mount and the reachable "internet" (a peer machine) are those of the VM.
# Narration ("say") appears in the recording only.
#
# Build (needs /dev/kvm, a few minutes):
#   just demo            # writes docs/demo-*.gif and docs/demo-*.cast
#   nix build -L .#demo
{
  pkgs,
  module,
}:
let
  inherit (pkgs) hyprland;

  hyprConfig = pkgs.writeText "hyprland.lua" ''
    hl.monitor({ output = "", mode = "preferred", position = "auto", scale = "1" })
    hl.config({
      animations = { enabled = false },
      misc = { disable_hyprland_logo = true, disable_splash_rendering = true },
    })
    hl.on("hyprland.start", function()
      hl.exec_cmd("systemctl --user import-environment WAYLAND_DISPLAY HYPRLAND_INSTANCE_SIGNATURE XDG_CURRENT_DESKTOP"
        .. " && systemctl --user start zones-demo-session.target; touch /tmp/hyprland-started")
    end)
  '';

  zonesToml = pkgs.writeText "zones.toml" ''
    version = 1

    [zones.untrusted]
    colour  = "#f38ba8"
    links   = "zone"
    devices = []

    [zones.agents]
    colour  = "#cba6f7"
    binds   = [{ path = "~/src/my-app", mode = "rw" }]
    devices = []
  '';

  # The demos, step by step. "say" is narration: it appears in the recording
  # only and is never typed. "run" is typed, key by key, into whichever shell
  # is in front (the host's, or the one `appzones shell` opened). `rows` is
  # the terminal's height: enough for the whole demo, so its last frame
  # shows all of it.
  demos = {
    # README top: what a zone keeps out
    untrusted = {
      rows = 22;
      steps = [
        { say = "alice's laptop, and one very precious SSH key."; }
        { run = "ls ~/.ssh"; }
        { say = "Let's open a zone for things we don't trust."; }
        { run = "appzones shell untrusted"; }
        { say = "A brand new home. Nothing to steal. Very minimalist."; }
        { run = "ls -A ~"; }
        { say = "Surely the SSH key is lying around somewhere?"; }
        { run = "cat ~/.ssh/id_ed25519"; }
        { say = "Fine. The router's admin page on the LAN, then?"; }
        { run = "curl http://192.168.1.2/"; }
        { say = "Nope. The internet still works, so the cat videos are safe."; }
        { run = "curl http://198.51.100.2/"; }
      ];
    };
    # README "AI agents in a zone"
    agents = {
      rows = 21;
      steps = [
        { say = "Three projects, and a dev server running on the host."; }
        { run = "ls ~/src"; }
        { run = "curl localhost:3000"; }
        { say = "The agents zone gets ~/src/my-app. What else can an agent find?"; }
        { run = "appzones shell agents"; }
        { run = "ls ~/src"; }
        { say = "Borrow your SSH keys to push somewhere \"helpful\"?"; }
        { run = "ssh-add -l"; }
        { say = "Poke the host's dev server?"; }
        { run = "curl localhost:3000"; }
        { say = "It can still break my-app. But only my-app."; }
      ];
    };
    # README "Opening a file you don't trust"
    file = {
      rows = 23;
      steps = [
        { say = "An invoice you weren't expecting. Classic."; }
        { run = "ls ~/Downloads"; }
        { say = "Lend the folder to the untrusted zone, read-only."; }
        { run = "appzones mount untrusted ~/Downloads --ro --as dl"; }
        { run = "appzones shell untrusted"; }
        { run = "ls /mnt/host/dl"; }
        { say = "Say the invoice gets ideas, like encrypting your downloads."; }
        { run = "echo pwned > /mnt/host/dl/invoice.pdf"; }
        { run = "exit"; }
        { say = "Done with it? Take it back."; }
        { run = "appzones umount untrusted dl"; }
      ];
    };
  };

  # Prompts in the zone's colour, as its window border: the zone's name as a
  # badge inside a zone (its hostname is NAME-zone), plain on the host. Zones
  # read the host's /etc/bashrc, so one promptInit covers both.
  promptInit = ''
    case "$HOSTNAME" in
      untrusted-zone) c="243;139;168" ;;
      agents-zone) c="203;166;247" ;;
      *) c="" ;;
    esac
    if [ -n "$c" ]; then
      PS1="\[\e[1;38;2;30;30;46;48;2;''${c}m\] ''${HOSTNAME%-zone} \[\e[0m\] \[\e[38;2;''${c}m\]\w\[\e[0m\] \$ "
    else
      PS1="\[\e[1;38;2;180;190;254m\]\u@\h\[\e[0m\] \[\e[38;2;137;180;250m\]\w\[\e[0m\] \$ "
    fi
  '';

  # the host's dev server on localhost:3000
  devServer = pkgs.writeText "dev-server.py" ''
    from http.server import BaseHTTPRequestHandler, HTTPServer
    class H(BaseHTTPRequestHandler):
        def do_GET(self):
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b"my-app dev server: hello, alice\n")
    HTTPServer(("127.0.0.1", 3000), H).serve_forever()
  '';

  record =
    name: d:
    "python3 ${./demo-record.py} ${pkgs.writeText "demo-${name}.json" (builtins.toJSON d.steps)} /tmp/demo-${name}.cast ${toString d.rows}";

  test = pkgs.testers.runNixOSTest {
    name = "appzones-demo";

    nodes.peer =
      { lib, ... }:
      {
        networking.firewall.enable = false;
        networking.interfaces.eth1.ipv4.addresses = lib.mkForce [
          {
            address = "192.0.2.2";
            prefixLength = 30;
          }
          {
            address = "198.51.100.2";
            prefixLength = 24;
          }
          {
            address = "192.168.1.2";
            prefixLength = 24;
          }
        ];
        services.nginx = {
          enable = true;
          virtualHosts.default = {
            default = true;
            locations."/".return = "200 'hello from the internet\\n'";
          };
        };
      };

    nodes.laptop =
      { lib, ... }:
      {
        imports = [ module ];
        programs.appzones = {
          enable = true;
          user = "alice";
        };

        virtualisation = {
          memorySize = 4096;
          cores = 4;
          qemu.options = [
            "-vga none"
            "-device virtio-gpu-pci"
          ];
        };
        hardware.graphics.enable = true;

        users.users.alice = {
          isNormalUser = true;
          uid = 1000;
        };
        services.getty.autologinUser = "alice";
        systemd.user.targets.zones-demo-session = {
          bindsTo = [ "graphical-session.target" ];
          wants = [ "graphical-session.target" ];
          before = [ "graphical-session.target" ];
        };
        environment.loginShellInit = ''
          if [ "$(tty)" = /dev/tty1 ] && [ "$(id -u)" = 1000 ]; then
            exec ${hyprland}/bin/Hyprland --config ${hyprConfig} >/tmp/hyprland.log 2>&1
          fi
        '';
        environment.sessionVariables.HYPRLAND_NO_CRASHREPORTER = "1";

        services.pipewire = {
          enable = true;
          pulse.enable = true;
        };
        xdg.portal = {
          enable = true;
          extraPortals = [ pkgs.xdg-desktop-portal-gtk ];
          config.common.default = "gtk";
        };

        networking.nftables.enable = true;
        boot.kernelModules = [
          "nf_tables"
          "nft_reject"
          "nft_reject_inet"
          "nf_reject_ipv4"
          "nf_reject_ipv6"
        ];

        networking.interfaces.eth1.ipv4.addresses = lib.mkForce [
          {
            address = "192.0.2.1";
            prefixLength = 30;
          }
        ];
        networking.interfaces.eth1.ipv4.routes = [
          {
            address = "192.168.1.0";
            prefixLength = 24;
            via = "192.0.2.2";
          }
        ];
        networking.defaultGateway = {
          address = "192.0.2.2";
          interface = "eth1";
        };

        programs.bash.promptInit = promptInit;

        systemd.services.dev-server = {
          wantedBy = [ "multi-user.target" ];
          serviceConfig.ExecStart = "${pkgs.python3}/bin/python3 ${devServer}";
        };

        environment.systemPackages = [
          pkgs.curl
          pkgs.openssh
          pkgs.python3
        ];
      };

    testScript = ''
      import json, re, shlex

      start_all()
      peer.wait_for_unit("nginx.service")
      laptop.wait_for_unit("multi-user.target")
      laptop.wait_for_file("/tmp/hyprland-started", timeout=120)
      laptop.wait_for_unit("user@1000.service")

      ENV = "export XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus; "

      def u(cmd):
          return laptop.succeed("su -l alice -c " + shlex.quote(ENV + cmd))

      u("install -d ~/.config/appzones ~/.ssh ~/src/my-app ~/src/other-project ~/src/secret-project ~/Downloads")
      u("install -m 644 ${zonesToml} ~/.config/appzones/zones.toml")
      u("echo '-----BEGIN OPENSSH PRIVATE KEY-----' >~/.ssh/id_ed25519 && chmod 600 ~/.ssh/id_ed25519")
      u("echo '%PDF-1.7 totally an invoice' >~/Downloads/invoice.pdf")
      u("appzones check")
      laptop.wait_for_unit("dev-server.service")
      for z in ["untrusted", "agents"]:
          u(f"appzones up {z}")

      # the recordings; the checks make sure each shows what it says
      def record(name, cmd, expect):
          """Record a demo; returns what its terminal showed, as plain text."""
          u("cd ~ && " + cmd)
          cast = laptop.succeed(f"cat /tmp/demo-{name}.cast").splitlines()
          shown = "".join(json.loads(l)[2] for l in cast[1:])
          plain = re.sub(r"\x1b\[[0-9;?]*[A-Za-z]|\r", "", shown)
          print(plain)
          for text, why in expect:
              assert text in plain, f"demo-{name}: {why}"
          laptop.copy_from_machine(f"/tmp/demo-{name}.cast", "")
          return plain

      record("untrusted", "${record "untrusted" demos.untrusted}", [
          ("No such file", "the host's key was visible"),
          ("Could not connect", "the LAN was reachable"),
          ("hello from the internet", "the internet was not reachable"),
      ])
      shown = record("agents", "${record "agents" demos.agents}", [
          ("hello, alice", "the host's dev server did not answer the host"),
          ("Could not open a connection to your authentication agent", "an SSH agent was reachable"),
          ("Failed to connect to localhost:3000", "the host's dev server was reachable"),
      ])
      inside = shown.split(" agents  ~ $ ", 1)[1]
      assert "my-app" in inside and "other-project" not in inside, "demo-agents: the zone saw other projects"
      record("file", "${record "file" demos.file}", [
          ("invoice.pdf", "the zone did not see the mount"),
          ("Read-only file system", "the mount was writable"),
      ])
      assert "totally an invoice" in u("cat ~/Downloads/invoice.pdf")
    '';
  };

  # Catppuccin Mocha: background, foreground, then the 16 terminal colours
  theme = "1e1e2e,cdd6f4,45475a,f38ba8,a6e3a1,f9e2af,89b4fa,f5c2e7,94e2d5,bac2de,585b70,f38ba8,a6e3a1,f9e2af,89b4fa,f5c2e7,94e2d5,a6adc8";
in
pkgs.runCommand "appzones-demo"
  {
    nativeBuildInputs = [ pkgs.asciinema-agg ];
  }
  ''
    mkdir $out
    for name in ${toString (builtins.attrNames demos)}; do
      cp ${test}/demo-$name.cast $out/
      agg --font-dir ${pkgs.jetbrains-mono}/share/fonts/truetype --font-family "JetBrains Mono" \
        --font-size 16 --theme ${theme} --idle-time-limit 2 --last-frame-duration 4 --fps-cap 15 \
        $out/demo-$name.cast $out/demo-$name.gif
    done
  ''
