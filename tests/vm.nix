# NixOS VM test for appzones (nix/package.nix, nix/module.nix).
#
# One machine runs Hyprland (nixpkgs') on the VM's virtual GPU with
# software rendering, started on
# tty1 by an autologin user, plus PipeWire, dbus-broker and the GTK portal.
# A second machine ("peer", the router: 192.0.2.2 on a /30 link) serves
# HTTP on a LAN address (192.168.1.2, refused from a zone) and on a "public"
# one (198.51.100.2, routed, not connected: the zone's
# internet, reached through the default route).
#
# Covered:
#   - the NixOS module: appzones@.service in appzones.slice (Type=notify), start,
#     stop, restart, crash (kill -9 zone-init: the unit restarts),
#     appzones-autostart (autostart zone + autostart app, launcher entries);
#   - the CLI: list, which, run APP / run -- CMD / run --wait / run --restart
#     on a stale zone, shell (a pty inside), new, check --probe, apps, ps;
#   - isolation probes (zone home, ~/.ssh, /srv/code, the runtime dir,
#     caps, no_new_privs, environment, machine-id), the network (host
#     loopback, a tailnet-like address on the host, the LAN peer refused; the
#     public peer reachable; nft not removable; network = none), D-Bus
#     (systemd1 refused, the portal answers with the zone's app id);
#   - Wayland: the zone's client sees no screencopy or data-control, its
#     window gets the tag zone:NAME and the zone colour
#     (contrib/hyprland/appzones.lua, copied into the store on its own,
#     next to another window.open_early handler), a
#     host window gets none; an app launched from its launcher entry too;
#   - single-instance GTK apps on the zone's own bus (bus = "zone"): Thunar
#     (with programs.thunar: its names are activatable on the host bus)
#     shows a window, a second launch (`appzones run ta -- thunar`, matched to
#     [apps.thunar]) hands over to the first; gnome-text-editor and eog start;
#     on the host bus Thunar fails and `appzones run` prints its exit status, last
#     output and the bus hint; bus = "none" leaves no bus socket at all; the
#     validator refuses owning org.freedesktop.Notifications and org.xfce.Thunar
#     on the host bus; `appzones run` reports an app that exits early; `appzones logs -f`;
#   - Firefox in two zones at once: each owns only its zone's remote name
#     (org.mozilla.firefox_zone_ZONE.*), and a second launch in one zone
#     opens in that zone's Firefox, never the other's;
#   - a browser (Firefox, headless) runs in a zone with its own sandbox:
#     content processes under seccomp in their own user namespace;
#   - audio: pipewire-0 and pulse/native are in a zone only with "audio";
#   - the FHS overlay's --bind-try: a buildFHSEnv app runs inside a zone;
#   - nix_ld (release 3): a generic glibc binary (interpreter
#     /lib64/ld-linux-x86-64.so.2, no RPATH) runs in a zone with
#     nix_ld = true and fails clearly without; the validator refuses
#     nix_ld = true on a host without nix-ld;
#   - tmp = "disk": /tmp and /var/tmp are kept across `appzones restart`, are
#     visible on the host (0700), old files are removed at start;
#   - runtime mounts: appzones mount / mounts / umount, ro is ro (also against a
#     remount from a nested user namespace), a busy umount lists the holder,
#     --lazy, never-list refused (also through a symlink), the root helper
#     refuses a target outside zones/*/mnt (symlink, ..), a directory the
#     user cannot reach, and anyone but its sudo caller; the sudo rule
#     allows nothing else; a zone restart drops every mount;
#   - DNS: the host's resolver through the zone's server (UDP, TCP, truncated
#     answers); dns = "public" refused on the host (a VPN's DNS leak
#     protection): the zone's log and check --probe say why.
#
# Run (needs /dev/kvm, ~6 min):
#   just test-vm
#   nix build -L .#checks.x86_64-linux.vm
{
  pkgs,
  module,
  # another build of appzones to test; null: the module's default
  package ? null,
}:
let
  inherit (pkgs) hyprland;
  # copied into the store on its own, as a config might do it
  zonesLua = ../contrib/hyprland/appzones.lua;

  # a desktop config usually has its own window.open_early handlers; keep
  # one here so the test shows both run.
  hyprConfig = pkgs.writeText "hyprland.lua" ''
    hl.monitor({ output = "", mode = "preferred", position = "auto", scale = "1" })
    hl.config({
      general = { border_size = 2 },
      animations = { enabled = false },
      misc = { disable_hyprland_logo = true, disable_splash_rendering = true },
      debug = { enable_stdout_logs = true },
    })
    hl.on("window.open_early", function(w)
      local f = io.open("/tmp/other-handler", "a")
      if f then f:write(tostring(w.pid), "\n"); f:close() end
    end)
    dofile("${zonesLua}")
    hl.on("hyprland.start", function()
      -- as uwsm does: the session's environment, then
      -- graphical-session.target (xdg-desktop-portal needs it; zone units
      -- are PartOf it, appzones-autostart is WantedBy it)
      hl.exec_cmd("systemctl --user import-environment WAYLAND_DISPLAY HYPRLAND_INSTANCE_SIGNATURE XDG_CURRENT_DESKTOP"
        .. " && systemctl --user start zones-test-session.target; touch /tmp/hyprland-started")
    end)
  '';

  # A generic Linux binary, as downloaded: interpreter /lib64/ld-linux-x86-64.so.2,
  # no RPATH. Runs only through nix-ld.
  genericBin = pkgs.runCommand "generic-bin" { nativeBuildInputs = [ pkgs.patchelf ]; } ''
    mkdir -p $out/bin
    cp ${pkgs.hello}/bin/hello $out/bin/generic-hello
    chmod +w $out/bin/generic-hello
    patchelf --set-interpreter /lib64/ld-linux-x86-64.so.2 --remove-rpath $out/bin/generic-hello
  '';

  # a "browser" for tz's links (links = "zone"): the server picks a declared app whose
  # command names firefox; this one writes the link it gets into the zone's home
  # a bwrap that waits before it starts: a zone held "starting" for a moment, whatever the
  # server is written in (the crash test kills its server then)
  # zone-wayland: the package's binary, as a wrapper script can run it
  zoneWayland = "${
    if package != null then package else pkgs.callPackage ../nix/package.nix { }
  }/libexec/appzones/appzones-helper zone-wayland";
  slowBwrap = pkgs.writeShellScript "slow-bwrap" "sleep 3; exec ${pkgs.bubblewrap}/bin/bwrap \"$@\"";
  # a notification server that, like gnome-shell, has more on the same connection (here
  # org.example.Other.Poke, standing in for Mutter's RemoteDesktop): a zone may notify, but
  # never call the rest through the owner's unique name
  fakeNotifier = pkgs.writeText "fake-notifier.py" ''
    import asyncio
    from dbus_next.aio import MessageBus
    from dbus_next.service import ServiceInterface, method

    class Notifications(ServiceInterface):
        def __init__(self):
            super().__init__("org.freedesktop.Notifications")

        @method()
        def GetServerInformation(self) -> "ssss":
            return ["fake", "appzones-test", "1", "1.2"]

    class Other(ServiceInterface):
        def __init__(self):
            super().__init__("org.example.Other")

        @method()
        def Poke(self) -> "s":
            open("/tmp/poked", "w").write("x")
            return "poked"

    async def main():
        bus = await MessageBus().connect()
        bus.export("/org/freedesktop/Notifications", Notifications())
        bus.export("/poke", Other())
        await bus.request_name("org.freedesktop.Notifications")
        open("/tmp/notifier", "w").write(bus.unique_name)
        await asyncio.Future()

    asyncio.run(main())
  '';
  notifierPython = pkgs.python3.withPackages (ps: [ ps.dbus-next ]);
  linkCatcher = pkgs.writeShellScriptBin "firefox-link-catcher" "printf '%s\\n' \"$1\" > \"$HOME/opened-link\"";

  fhsHello = pkgs.buildFHSEnv {
    name = "fhs-hello";
    runScript = pkgs.writeShellScript "fhs-hello" "echo fhs-ok";
  };

  # Drives `appzones shell ZONE` through a real pty, as a terminal does: types
  # a command, reads until the marker, exits. Prints what it read.
  shellDriver = pkgs.writeText "shell-driver.py" ''
    import os, pty, select, sys, time
    pid, fd = pty.fork()
    if pid == 0:
        os.execvp("appzones", ["appzones", "shell", sys.argv[1]])
    def read_until(marker, buf, secs):
        end = time.time() + secs
        while marker not in buf and time.time() < end:
            r, _, _ = select.select([fd], [], [], 1)
            if r:
                try:
                    buf += os.read(fd, 4096)
                except OSError:
                    break
        return buf
    buf = read_until(b"$ ", b"", 30)  # the zone's prompt
    os.write(fd, b"echo ZONE_IS=$ZONE; tty; echo DONE-$((40+2))\n")
    end = time.time() + 30
    while b"DONE-42" not in buf and time.time() < end:
        r, _, _ = select.select([fd], [], [], 1)
        if r:
            try:
                buf += os.read(fd, 4096)
            except OSError:
                break
    os.write(fd, b"exit\n")
    time.sleep(1)
    print(buf.decode(errors="replace"))
  '';

  zonesToml = pkgs.writeText "zones.toml" ''
    version = 1
    requires = 8
    [defaults]
    devices = []
    [zones.tw]
    colour = "#9399b2"
    allow = ["allowed.test"]
    [zones.ta]
    colour = "#a6e3a1"
    devices = ["audio"]
    binds = [ { path = "~/Shared", mode = "rw" }, { path = "~/linked", mode = "rw" } ]
    [zones.tb]
    colour = "#89b4fa"
    network = "none"
    [zones.tz]
    colour = "#f9e2af"
    links = "zone"
    autostart = true
    autostart_apps = ["sleeper"]
    [apps.foot]
    command = ["foot"]
    zones = ["ta", "tb"]
    [apps.sleeper]
    command = ["sleep", "3600"]
    zones = ["tz"]
    [apps.linkcatcher]
    command = ["${linkCatcher}/bin/firefox-link-catcher"]
    zones = ["tz"]
    [apps.thunar]
    command = ["thunar"]
    zones = ["ta"]
    bus = "zone"
    [apps.text-editor]
    command = ["gnome-text-editor"]
    zones = ["ta"]
    bus = "zone"
    [apps.eog]
    command = ["eog"]
    zones = ["ta"]
    bus = "zone"
    [zones.tc]
    colour = "#fab387"
    [zones.tp]
    colour = "#f2cdcd"
    dns = "public"
    [zones.td]
    colour = "#94e2d5"
    nix_ld = true
    tmp = "disk"
    [zones.tu]
    colour = "#74c7ec"
    devices = ["usb"]
    usb = ["0627:0001"]
    [zones.tv]
    colour = "#f5c2e7"
    devices = ["usb"]
    usb = ["dead"]
    [zones.tl]
    colour = "#eba0ac"
    network = "none"
    supervisor = "spawn"
    start_timeout = "20s"
    linger = "10s"
    [zones.tm]
    colour = "#b4befe"
    network = "none"
    linger = "10s"
    [apps.firefox]
    command = ["firefox"]
    zones = ["ta", "tc"]
  '';

  # Bus names a zone must never own on the host bus.
  badOwnToml =
    name:
    pkgs.writeText "zones-bad.toml" ''
      version = 1
      [zones.ta]
      colour = "#a6e3a1"
      dbus = { own = ["${name}"] }
    '';
in
pkgs.testers.runNixOSTest {
  name = "appzones";

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
          locations."/".return = "200 'peer-ok'";
        };
      };
    };

  nodes.machine =
    { lib, ... }:
    {
      imports = [ module ];
      programs.appzones = {
        enable = true;
        package = lib.mkIf (package != null) package;
        user = "alice";
        deniedPaths = [ "/srv/code" ]; # a host's own addition: binds and mounts refuse it
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
      systemd.user.targets.zones-test-session = {
        description = "Test session (stands in for uwsm's wayland-session target)";
        bindsTo = [ "graphical-session.target" ];
        wants = [ "graphical-session.target" ];
        before = [ "graphical-session.target" ];
      };
      # Hyprland on tty1, as the autologin user (a logind seat session).
      # It imports WAYLAND_DISPLAY and HYPRLAND_INSTANCE_SIGNATURE into the
      # user manager, as a session manager does, so appzones@.service sees them.
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

      # The zone's nft rules load from inside an unprivileged user namespace,
      # where the kernel does not autoload modules: load them here (a host's own
      # nftables firewall loads them there).
      networking.nftables.enable = true;
      boot.kernelModules = [
        "nf_tables"
        "nft_reject"
        "nft_reject_inet"
        "nf_reject_ipv4"
        "nf_reject_ipv6"
        "dummy"
      ];

      # The "internet": the default route to the peer over a /30 link (the
      # host's connected network, refused from a zone as any LAN); the public
      # address 198.51.100.2 is behind it, and so is the peer's LAN address
      # (192.168.1.2, refused as a private range).
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
      # names of the public peer, for the allow list (zone tw): the proxy resolves them here
      networking.hosts."198.51.100.2" = [
        "allowed.test"
        "other.test"
      ];
      # the measured hole: a service on a tailnet address of
      # the host (a dummy link), and one on its loopback.
      systemd.services.host-listeners = {
        path = [ pkgs.iproute2 ];
        wantedBy = [ "multi-user.target" ];
        after = [ "network-online.target" ];
        wants = [ "network-online.target" ];
        script = ''
          ip link add ts0 type dummy || true
          ip addr add 100.64.10.1/32 dev ts0 || true
          ip link set ts0 up
          ${pkgs.python3}/bin/python3 -m http.server --bind 127.0.0.1 8080 &
          ${pkgs.python3}/bin/python3 -m http.server --bind 100.64.10.1 8443 &
          ${pkgs.python3}/bin/python3 -m http.server --bind :: 8081 &
          wait
        '';
      };

      # /lib64/ld-linux-x86-64.so.2 is nix-ld's shim
      programs.nix-ld.enable = true;
      # a directory alice cannot reach, with a world-readable one inside
      systemd.tmpfiles.rules = [
        "d /srv/locked 0700 root root -"
        "d /srv/locked/inner 0755 root root -"
      ];

      # Thunar's names are activatable on
      # the host bus (thunar.service, xfconf)
      programs.thunar.enable = true;

      # The host's resolver on its loopback (as dnsmasq's 127.0.0.1 or
      # systemd-resolved's 127.0.0.53 on many hosts): a zone reaches it only
      # through its server (dns.py). big.example's TXT answer is bigger than
      # a UDP answer may be: truncated over UDP, whole over TCP.
      services.dnsmasq = {
        enable = true;
        settings = {
          no-resolv = true;
          address = [ "/zone-test.example/192.0.2.99" ];
          txt-record = map (i: "big.example,${lib.strings.replicate 200 "x"}${toString i}") (lib.range 1 8);
        };
      };

      environment.systemPackages = with pkgs; [
        hyprland # hyprctl on PATH (`appzones active`)
        dig # DNS checks in a zone
        runit # process managers for contrib/'s service files
        s6
        dinit
        gnome-text-editor
        eog
        foot
        wayland-utils
        firefox
        curl
        glib
        systemd
        procps
        psmisc # fuser: which process holds a zone's lock (its server)
        util-linux
        fhsHello
        jq
      ];
      fonts.enableDefaultPackages = true;
    };

  testScript = ''
    import json, shlex, time

    start_all()
    peer.wait_for_unit("nginx.service")
    machine.wait_for_unit("multi-user.target")
    machine.wait_for_unit("host-listeners.service")
    machine.wait_for_file("/tmp/hyprland-started", timeout=120)
    machine.wait_for_unit("user@1000.service")

    ENV = "export XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus; "

    def u(cmd, check=True):
        """cmd as alice, in her login environment (no HYPRLAND_*, no WAYLAND_*)."""
        full = "su -l alice -c " + shlex.quote(ENV + cmd)
        if check:
            return machine.succeed(full)
        return machine.execute(full)

    def inside(zone, cmd):
        """(rc, output) of cmd run as a process inside the zone."""
        return u(f"appzones run --wait {zone} -- bash -c {shlex.quote(cmd)} </dev/null 2>&1", check=False)

    def ok(zone, cmd, what):
        rc, out = inside(zone, cmd)
        assert rc == 0, f"{what}: expected success, rc={rc}: {out}"
        print(f"PASS  {what}")
        return out

    def refused(zone, cmd, what):
        rc, out = inside(zone, cmd)
        assert rc != 0, f"{what}: expected failure, it worked: {out}"
        print(f"PASS  {what}")

    def hypr(args):
        return u(f"${hyprland}/bin/hyprctl -i 0 {args}")

    def clients():
        return json.loads(hypr("-j clients"))

    def window(title):
        """The client foot opened with --title TITLE (the shell may retitle it later)."""
        cs = clients()
        print(json.dumps(cs, indent=1))
        return next(c for c in cs if title in (c.get("initialTitle"), c.get("title")))

    # ── setup: alice's files, zones.toml, host secrets that must stay out.
    # The home is laid out as appzones 0.5 left it (~/.config/zones,
    # ~/.local/share/zones): the first subtest moves it by hand, as appzones says.
    machine.succeed("install -d -o alice -g users /home/alice/.config/zones /home/alice/.ssh /home/alice/Shared")
    # a bind written through a symlink (~/linked -> ~/realproj): `run` from inside it
    machine.succeed("install -d -o alice -g users /home/alice/realproj/sub && ln -s realproj /home/alice/linked && chown -h alice:users /home/alice/linked")
    machine.succeed("install -o alice -g users -m 644 ${zonesToml} /home/alice/.config/zones/zones.toml")
    machine.succeed("chown alice:users /home/alice/.config")  # install -d made it root's
    u("mkdir -p ~/.local/share/zones/ta/home ~/.local/share/zones/launcher/share/applications "
      "&& echo kept >~/.local/share/zones/ta/home/from-0.5 "
      "&& touch ~/.local/share/zones/launcher/share/applications/zone-old-ta.desktop")
    machine.succeed("echo secret >/home/alice/.ssh/id_test; chown alice /home/alice/.ssh/id_test")
    machine.succeed("mkdir -p /srv/code && echo secret >/srv/code/secret")
    out = u("systemctl --user show-environment")
    assert "WAYLAND_DISPLAY=" in out, out
    assert "HYPRLAND_INSTANCE_SIGNATURE=" in out, out
    WL = [l.split("=", 1)[1] for l in out.splitlines() if l.startswith("WAYLAND_DISPLAY=")][0]
    SIG = [l.split("=", 1)[1] for l in out.splitlines() if l.startswith("HYPRLAND_INSTANCE_SIGNATURE=")][0]

    def invocation(zone):
        return u(f"systemctl --user show appzones@{zone}.service -p InvocationID --value").strip()

    # a zone's server: the process that holds the zone's lock (appzones' own rule, whatever
    # the server is written in); `fuser` prints the pids
    SERVER = "fuser -s /run/user/1000/appzones/{z}.lock"

    def server_pids(z):
        return machine.succeed(f"fuser /run/user/1000/appzones/{z}.lock 2>/dev/null || true").split()

    with subtest("left from 0.5: named with what to do, never read or moved; the host's old never-list fails closed"):
        machine.succeed("test -f /etc/appzones/denied-paths && test ! -e /etc/zones")
        rc, out = u("appzones list 2>&1", check=False)
        print(out)
        assert "/home/alice/.config/zones/zones.toml is from before appzones 0.6 and is not read: mkdir -p" in out, out
        assert "/home/alice/.local/share/zones (zone homes from before appzones 0.6) is not used: appzones stop --all && mv" in out, out
        u("test -f ~/.local/share/zones/ta/home/from-0.5 && test ! -e ~/.local/share/appzones")
        print("PASS  the old config and state are named, nothing read or moved")
        u("mkdir -p ~/.config/appzones && mv -n ~/.config/zones/zones.toml ~/.config/appzones/zones.toml && rmdir ~/.config/zones")
        u("appzones stop --all && mv -T ~/.local/share/zones ~/.local/share/appzones")
        out = u("appzones list 2>&1")
        assert "before appzones 0.6" not in out, out
        u("grep -qx kept ~/.local/share/appzones/ta/home/from-0.5")
        print("PASS  moved by hand as the warnings say: no more warnings, the zone home's files kept")
        # the host's never-list only at its pre-0.6 place: nothing starts (its paths would enter zones)
        machine.succeed("mv /etc/appzones/denied-paths /etc/appzones/denied-paths.moved && mkdir -p /etc/zones "
                        "&& cp /etc/appzones/denied-paths.moved /etc/zones/denied-paths")
        rc, out = u("appzones check 2>&1", check=False)
        print(out)
        assert rc != 0 and "/etc/zones/denied-paths is from before appzones 0.6 and is not read any more" in out, (rc, out)
        rc, out = u("appzones up ta 2>&1", check=False)
        assert rc != 0, out
        machine.succeed("rm -r /etc/zones && mv /etc/appzones/denied-paths.moved /etc/appzones/denied-paths")
        print("PASS  /etc/zones/denied-paths alone: check fails and no zone starts")

    with subtest("appzones check and which"):
        print(u("appzones check"))
        assert '"internet"' in u("appzones which ta")
        print(u("appzones new tn"))  # prints a block, writes nothing
        assert "tn" not in u("cat ~/.config/appzones/zones.toml")
        u("appzones list")

    with subtest("appzones-autostart: autostart zone, its app, launcher entries"):
        u("systemctl --user start appzones-autostart.service")
        machine.wait_until_succeeds("su -l alice -c " + shlex.quote(ENV + "appzones ping tz"), timeout=60)
        machine.wait_until_succeeds("su -l alice -c " + shlex.quote(ENV + "appzones ps tz | grep -q 'sleep 3600'"), timeout=30)
        cg = machine.succeed("cat /proc/$(pgrep -u alice -f 'sleep 3600' | head -1)/cgroup").strip()
        assert cg.endswith("/appzones.slice/appzones@tz.service"), cg
        u("test -f ~/.local/share/appzones/launcher/share/applications/appzones-foot-ta.desktop")
        print(u("cat ~/.local/share/appzones/launcher/share/applications/appzones-foot-ta.desktop"))
        u("test ! -e ~/.local/share/appzones/launcher/share/applications/zone-old-ta.desktop")
        print("PASS  launcher entries renamed, the pre-0.6 one removed")
        # the profile puts the launcher dir on XDG_DATA_DIRS of a login
        assert "/home/alice/.local/share/appzones/launcher/share" in u("echo $XDG_DATA_DIRS")
        u("appzones stop tz")
        u("! appzones ping tz")

    with subtest("start: appzones@ta.service"):
        u("appzones up ta")
        print(u("systemctl --user status appzones@ta.service --no-pager -n 20"))
        u("systemctl --user is-active appzones@ta.service")
        print(u("appzones list"))
        # what the sway hook names a window by (sway before 1.11): the zone's processes in
        # its unit's cgroup, as the host sees it
        pid = u("systemctl --user show appzones@ta.service -p MainPID --value").strip()
        init = machine.succeed(f"pgrep -u alice -x zone-init -s $(ps -o sid= -p {pid}) || pgrep -u alice -x zone-init").split()
        cg = [machine.succeed(f"cat /proc/{p}/cgroup").strip() for p in init]
        assert any("/appzones.slice/appzones@ta.service" in c for c in cg), cg
        print("PASS  the zone's processes are in appzones.slice/appzones@ta.service (the sway hook's cgroup rule)")

    with subtest("filesystem and process isolation"):
        ok("ta", "touch ~/probe && test -w ~", "zone home writable")
        machine.succeed("test ! -e /home/alice/probe")
        print("PASS  host home does not have ~/probe")
        refused("tb", "test -e ~/probe", "the other zone cannot see ~/probe")
        ok("ta", "touch ~/Shared/from-zone", "rw bind writable")
        machine.succeed("test -e /home/alice/Shared/from-zone")
        # cwd through a symlinked bind: the host's folder is the real one (getcwd), the zone's is the bind's
        out = u("cd ~/linked/sub && appzones run --wait ta -- pwd </dev/null 2>&1").strip()
        assert out == "/home/alice/linked/sub", out
        print("PASS  run from a symlinked bind starts in the same folder in the zone")
        refused("ta", "ls -d ~/.ssh", "~/.ssh absent")
        refused("ta", "ls -d /srv/code", "/srv/code absent")
        refused("ta", "ls /run/user/1000/systemd", "host runtime dir absent")
        ok("ta", "[ -z \"$(ls -A /run/user/1000 | grep -v -x -e bus -e zone-bus -e wayland-0 -e pipewire-0 -e pulse -e doc)\" ]",
           "runtime dir holds only zone sockets")
        refused("ta", "ls /sys/fs/cgroup", "no cgroupfs")
        refused("ta", "ls -d /nix/var", "/nix/var absent")
        ok("ta", "grep -q 'CapEff:.0000000000000000' /proc/self/status", "capabilities empty")
        ok("ta", "grep -q 'NoNewPrivs:.1' /proc/self/status", "no_new_privs")
        # zone-init's control socket: made by the host, outside the zone; nothing in
        # the zone can replace it or take it from zone-init
        machine.succeed("test -S /run/user/1000/appzones/ta/agent.sock")
        refused("ta", "test -e /run/zone/ctl.sock", "no control socket path in the zone")
        refused("ta", "touch /run/zone/planted", "/run/zone is read-only")
        refused("ta", "cat /proc/2/environ >/dev/null", "zone-init (pid 2) is not dumpable: not readable from the zone")
        refused("ta", "env | grep -E 'HYPRLAND|SSH_AUTH|TOKEN'", "no HYPRLAND_INSTANCE_SIGNATURE (the manager has it)")
        host_mid = machine.succeed("cat /etc/machine-id").strip()
        ok("ta", f"[ \"$(cat /etc/machine-id)\" != {host_mid} ]", "own machine-id")
        ok("ta", "[ \"$(cat /proc/sys/kernel/hostname)\" = ta-zone ]", "own hostname")
        ok("ta", "n=$(ls /proc | grep -c '^[0-9]'); [ $n -lt 8 ]", "pid namespace: zone processes only")
        ok("ta", "fhs-hello | grep -qx fhs-ok", "an FHS app (bwrap with --bind-try ...) runs in a zone")

    with subtest("network"):
        tcp = "timeout 3 bash -c '</dev/tcp/{}/{}'"
        ok("ta", "curl -sS -m 10 http://198.51.100.2/ | grep -q peer-ok", "internet (public peer) reachable")
        refused("ta", tcp.format("192.168.1.2", 80), "LAN peer refused")
        refused("ta", tcp.format("127.0.0.1", 8080), "host loopback refused")
        refused("ta", tcp.format("100.64.10.1", 8443), "host's tailnet address refused")
        machine.succeed("curl -sS -m 5 http://100.64.10.1:8443/ >/dev/null")
        print("PASS  (control: the host itself reaches 100.64.10.1:8443)")
        refused("ta", "nft flush ruleset", "nft flush refused")
        refused("ta", "nft flush ruleset 2>/dev/null; unshare -Ur nft flush ruleset 2>/dev/null; " + tcp.format("100.64.10.1", 8443),
                "still refused after flush attempts")
        # a LAN with global IPv6 addresses (the test network's 2001:db8:1::/64): refused like a private one
        peer6 = peer.succeed("ip -6 -o addr show dev eth1 scope global | awk '{print $4}' | cut -d/ -f1 | head -1").strip()
        assert peer6.startswith("2001:db8:"), peer6
        machine.succeed(f"curl -sS -m 5 'http://[{peer6}]/' | grep -q peer-ok")
        print(f"PASS  (control: the host reaches the IPv6 LAN peer {peer6})")
        rules = u("cat /run/user/1000/appzones/ta/nft.rules")
        assert any("2001:db8:1::/64" in l and l.strip().startswith("ip6 daddr") and l.rstrip().endswith("reject")
                   for l in rules.splitlines()), rules
        refused("ta", f"curl -sS -m 5 'http://[{peer6}]/'", "IPv6 LAN peer (global address) refused")
        me6 = machine.succeed("ip -6 -o addr show dev eth1 scope global | awk '{print $4}' | cut -d/ -f1 | head -1").strip()
        machine.succeed(f"curl -sS -m 5 http://192.0.2.1:8081/ >/dev/null && curl -sS -m 5 'http://[{me6}]:8081/' >/dev/null")
        print("PASS  (control: a dual-stack listener on the host's own addresses answers the host)")
        refused("ta", tcp.format("192.0.2.1", 8081), "the host's own IPv4 address refused")
        refused("ta", tcp.format("192.0.2.2", 80), "the router on the host's connected network refused")
        peer.succeed("true")
        refused("ta", f"curl -sS -m 5 'http://[{me6}]:8081/'", "the host's own IPv6 address refused")
        refused("tb", "curl -sS -m 5 http://198.51.100.2/", "network = none: no internet")

    with subtest("allow: only the names on the zone's list, through its proxy"):
        tcp = "timeout 3 bash -c '</dev/tcp/{}/{}'"
        ok("tw", "curl -sS -m 10 http://allowed.test/ | grep -q peer-ok", "allow: a name on the list, through the proxy")
        ok("tw", "[ \"$(curl -s -m 10 -o /dev/null -w '%{http_code}' http://other.test/)\" = 403 ]",
           "allow: a name off the list (the same address): 403 from the proxy")
        ok("tw", "[ \"$(curl -s -m 10 -o /dev/null -w '%{http_code}' http://198.51.100.2/)\" = 403 ]",
           "allow: an address, never on a list of names: 403")
        refused("tw", "curl -sS -m 5 --noproxy '*' http://198.51.100.2/", "allow: around the proxy, refused")
        # pasta's gateway address is the host's loopback: only the proxy's port of it
        machine.succeed("curl -sS -m 5 http://127.0.0.1:8080/ >/dev/null")
        refused("tw", tcp.format("192.0.2.2", 8080), "allow: the host's loopback on another port refused")
        ok("tw", "[ \"$(curl -s -m 10 -o /dev/null -w '%{http_code}' http://localhost:8080/)\" = 403 ]",
           "allow: the host's loopback through the proxy: 403")
        refused("tw", tcp.format("192.168.1.2", 80), "allow: the LAN peer refused")
        logs = u("appzones logs tw 2>&1")
        assert "refused other.test:80 (not on the zone's allow list)" in logs, logs
        print("PASS  allow: refusals in the zone's log")
        print(u("appzones check --probe tw"))
        print("PASS  allow: every probe in tw")

    with subtest("project zones: a repository's own zones, trusted at the terminal"):
        P = "/home/alice/src/app"
        u(f"mkdir -p {P}/.git {P}/sub && cd {P} && printf '%s\\n' 'version = 1' 'requires = 8' 'default = \"agent\"' "
          "'[zones.agent]' 'network = \"internet\"' 'allow = [\"allowed.test\"]' '[zones.dev]' 'checkout = \"rw\"' > appzones.toml")
        rc, out = u(f"cd {P} && appzones run --wait @ -- true </dev/null 2>&1", check=False)
        assert rc != 0 and "is not trusted" in out, out
        print("PASS  project: untrusted, nothing starts")
        out = u(f"cd {P}/sub && echo y | script -qec 'appzones trust' /dev/null")
        assert "! @agent (the default): the internet, only allowed.test" in out and "trusted:" in out, out
        print("PASS  project: trusted at the terminal, what it asks for shown first")
        def at(zone, cmd):
            return u(f"cd {P}/sub && appzones run --wait {zone} -- bash -c {shlex.quote(cmd)} </dev/null 2>&1", check=False)
        rc, out = at("@", "curl -sS -m 10 http://allowed.test/")
        assert rc == 0 and "peer-ok" in out, out
        rc, out = at("@", "[ \"$(curl -s -m 10 -o /dev/null -w '%{http_code}' http://other.test/)\" = 403 ]")
        assert rc == 0, out
        print("PASS  project: @ (the default) reaches only its allow list")
        rc, out = at("@", "pwd")
        assert rc == 0 and out.strip() == f"{P}/sub", out
        print("PASS  project: the command starts in the same folder")
        rc, out = at("@", f"touch {P}/x")
        assert rc != 0, out
        rc, out = at("@dev", f"touch {P}/x && echo written")
        assert rc == 0 and "written" in out, out
        print("PASS  project: the checkout read-only in @, read-write in @dev (checkout = rw)")
        rc, out = at("@", "ls /home/alice/Shared /home/alice/.ssh 2>&1; ls -A /run/user/1000; echo wl=$WAYLAND_DISPLAY bus=$DBUS_SESSION_BUS_ADDRESS")
        assert "No such file" in out and "wl= bus=" in out and "bus\n" not in out and "wayland-0" not in out, out
        print("PASS  project: nothing else of the home, no Wayland, no D-Bus")
        listing = u("appzones list")
        assert "app/agent" in listing and "app/dev" in listing, listing
        serving = lambda: len(machine.succeed("pgrep -u alice -f 'appzones-helper cli serve p' || true").split())
        assert serving() == 2, serving()
        # trusted again without @dev: its running zone is stopped (nothing else would find it)
        u(f"cd {P} && sed -i '/zones.dev/,$d' appzones.toml && echo y | script -qec 'appzones trust' /dev/null")
        machine.wait_until_succeeds("[ $(pgrep -u alice -f 'appzones-helper cli serve p' | wc -l) = 1 ]", timeout=30)
        print("PASS  project: a zone dropped from the file is stopped when it is trusted again")
        u(f"cd {P} && sed -i 's/allowed.test/other.test/' appzones.toml")
        rc, out = at("@", "true")
        assert rc != 0 and "changed since you trusted it" in out, out
        print("PASS  project: a changed file is refused until trusted again")
        u(f"cd {P} && appzones untrust")
        machine.wait_until_succeeds("su -l alice -c " + shlex.quote(ENV + "! appzones list | grep -q app/agent"), timeout=30)
        assert machine.execute("pgrep -u alice -f 'appzones-helper cli serve p'")[0] != 0
        print("PASS  project: untrust forgets it and stops its zones")
        # the xdg-open shim (links = "zone") is first on PATH: never writable from inside
        ok("tz", "test -x /run/zone/bin/xdg-open", "xdg-open shim present (links = zone)")
        refused("tz", "touch /run/zone/bin/firefox", "xdg-open shim directory read-only")
        # a link clicked in tz: the shim, the zone's links.sock, the server, its browser
        u("rm -f ~/.local/share/appzones/tz/home/opened-link")
        u("appzones run --wait tz -- xdg-open https://example.com/clicked-in-tz </dev/null 2>&1")
        machine.wait_until_succeeds(
            "grep -qx https://example.com/clicked-in-tz /home/alice/.local/share/appzones/tz/home/opened-link", timeout=60)
        print("PASS  a link clicked in a links = zone zone opens in that zone's browser")
        ok("tz", "! grep -q python /run/zone/bin/xdg-open && grep -q '/run/appzones-helper open-link' "
                 "/run/zone/bin/xdg-open", "the link part of xdg-open is appzones' own binary, no Python")
        rc, out = u("appzones run --wait tz -- xdg-open 'https://example.com/x y' </dev/null 2>&1; echo rc=$?", check=False)
        assert "rc=0" in out, out  # a space is fine; only http(s) URLs leave the zone this way
        # that link arrived too (its browser starts in the background), before the next check
        machine.wait_until_succeeds(
            "grep -qx 'https://example.com/x y' /home/alice/.local/share/appzones/tz/home/opened-link", timeout=60)
        # anything else sent to the link socket is refused, and no browser runs
        u("rm -f ~/.local/share/appzones/tz/home/opened-link")
        for bad in ["file:///etc/passwd", "javascript:alert(1)", "https://a.example/\nhttps://b.example/"]:  # a real line break: two links
            out = u("appzones run --wait tz -- ${pkgs.python3}/bin/python3 -c " + shlex.quote(
                "import socket,sys; s=socket.socket(socket.AF_UNIX); s.connect('/run/zone/links.sock'); "
                f"s.sendall({bad!r}.encode() + b'\\n'); sys.stdout.write(s.recv(3).decode())") + " </dev/null 2>&1")
            assert out.strip() == "no", (bad, out)
        machine.sleep(2)
        u("test ! -e ~/.local/share/appzones/tz/home/opened-link")
        print("PASS  the link socket refuses anything but one http(s) URL; no browser runs for it")
        # zones have no /sys/class/net; /proc/net/dev is the zone's own netns
        ok("tb", "[ \"$(tail -n +3 /proc/net/dev | cut -d: -f1 | tr -d ' ')\" = lo ]", "network = none: only lo")

    with subtest("D-Bus: filtered proxy, portal identity"):
        refused("ta", "busctl --user call org.freedesktop.systemd1 /org/freedesktop/systemd1 org.freedesktop.systemd1.Manager ListUnits",
                "systemd1 unreachable")
        out = ok("ta", "busctl --user list --no-pager --acquired", "bus names visible to the zone")
        assert "org.freedesktop.systemd1" not in out, out
        ok("ta", "gdbus call --session -d org.freedesktop.portal.Desktop -o /org/freedesktop/portal/desktop "
                 "-m org.freedesktop.portal.Settings.ReadOne org.freedesktop.appearance color-scheme",
           "the Settings portal answers through the proxy")
        rc, out = inside("ta", "gdbus call --session -d org.freedesktop.portal.Desktop -o /org/freedesktop/portal/desktop "
                               "-m org.freedesktop.portal.RemoteDesktop.CreateSession '@a{sv} {}'")
        print(out)
        assert rc != 0 and "AccessDenied" in out, out
        print("PASS  RemoteDesktop portal refused by the proxy")
        # notifications: only their own interface, not the whole connection that owns the name
        u("rm -f /tmp/notifier /tmp/poked; systemd-run --user --collect --unit=fake-notifier "
          "${notifierPython}/bin/python3 ${fakeNotifier}")
        machine.wait_until_succeeds("test -s /tmp/notifier", timeout=30)
        owner = machine.succeed("cat /tmp/notifier").strip()
        ok("ta", "gdbus call --session -d org.freedesktop.Notifications -o /org/freedesktop/Notifications "
                 "-m org.freedesktop.Notifications.GetServerInformation | grep -q appzones-test",
           "a notification server answers through the proxy")
        rc, out = inside("ta", f"gdbus call --session -d {owner} -o /poke -m org.example.Other.Poke")
        print(out)
        assert rc != 0 and "AccessDenied" in out, out
        machine.succeed("test ! -e /tmp/poked")
        rc, out = inside("ta", "gdbus call --session -d org.freedesktop.Notifications -o /poke -m org.example.Other.Poke")
        assert rc != 0, out
        machine.succeed("test ! -e /tmp/poked")
        u("systemctl --user stop fake-notifier")
        print("PASS  the notification server's other interfaces (by its name or its unique name) refused")
        # no blanket org.mpris.MediaPlayer2.*: owning implies talking to the host's and other zones' players
        refused("ta", "gdbus call --session -d org.freedesktop.DBus -o /org/freedesktop/DBus "
                      "-m org.freedesktop.DBus.RequestName org.mpris.MediaPlayer2.zoneprobe 4",
                "owning org.mpris.MediaPlayer2.* refused")

    with subtest("audio only with devices = audio"):
        ok("ta", "test -S /run/user/1000/pipewire-0 && test -S /run/user/1000/pulse/native && [ -n \"$PULSE_SERVER\" ]",
           "ta (audio): pipewire-0 and pulse/native")
        refused("tb", "test -e /run/user/1000/pipewire-0 || test -e /run/user/1000/pulse/native", "tb (no audio): no sockets")

    with subtest("usb: only the named device, its hidraw and sysfs; none without the device"):
        # the x86 test VM has QEMU's usb-tablet (0627:0001, HID) beside the root hub (1d6b)
        refused("ta", "ls -d /dev/bus/usb /dev/hidraw* /sys/bus/usb 2>/dev/null | grep -q .",
                "ta (no usb): no /dev/bus/usb, no hidraw, no /sys/bus/usb")
        dev = machine.succeed("for d in /sys/bus/usb/devices/*; do [ \"$(cat $d/idVendor 2>/dev/null):$(cat $d/idProduct 2>/dev/null)\" = 0627:0001 ] "
                              "&& printf '/dev/bus/usb/%03d/%03d %s\\n' $(cat $d/busnum) $(cat $d/devnum) $(readlink -f $d); done; true").split()
        assert len(dev) == 2, dev
        node, sysdir = dev
        hid = machine.succeed(f"for h in /sys/class/hidraw/*; do case $(readlink -f $h/device) in {sysdir}/*) echo /dev/''${{h##*/}};; esac; done").split()
        print(f"host: tablet at {node} ({sysdir}), hidraw {hid}")
        u("appzones up tu")
        # a glob, not find -type c: find trusts d_type, which is bwrap's mount-point file, not the bound node
        out = ok("tu", "for n in /dev/bus/usb/*/* /dev/hidraw*; do [ -c \"$n\" ] && echo $n; done; true", "tu: device nodes")
        assert sorted(out.split()) == sorted([node] + hid), out
        print("PASS  tu sees exactly the tablet's node and its hidraw, not the root hubs'")
        ok("tu", f"head -c 18 {node} >/dev/null", "tu: the device node opens (descriptors)")
        ok("tu", f"[ \"$(cat {sysdir}/idVendor)\" = 0627 ] && ls /sys/bus/usb/devices | grep -q .", "tu: its sysfs and /sys/bus/usb")
        refused("tu", "cat /sys/bus/usb/devices/usb*/idVendor", "tu: the root hubs' sysfs is not there")
        refused("tu", "test -e /dev/uinput", "tu: no /dev/uinput")
        print(u("appzones list"))
        u("appzones up tv")
        out = u("appzones logs tv")
        assert "no device matches dead" in out, out
        print("PASS  tv: no matching device, warned and started")
        refused("tv", "ls -d /dev/bus/usb /sys/bus/usb 2>/dev/null | grep -q .", "tv: nothing bound")
        u("appzones stop tv")
        u("appzones stop tu")

    with subtest("nix_ld: a generic glibc binary runs only with nix_ld = true"):
        gbin = "${genericBin}/bin/generic-hello"
        u("appzones up td")
        out = ok("td", f"{gbin}", "generic binary runs in td (nix_ld = true)")
        assert "Hello, world!" in out, out
        ok("td", "case \"$(readlink /lib64/ld-linux-x86-64.so.2)\" in /nix/store/*nix-ld*) ;; *) exit 1;; esac "
                 "&& [ \"$NIX_LD\" = /run/current-system/sw/share/nix-ld/lib/ld.so ] && [ -n \"$NIX_LD_LIBRARY_PATH\" ]",
           "/lib64 holds the host's nix-ld shim, NIX_LD set as on the host")
        rc, out = inside("tb", f"{gbin}")
        print(out)
        assert rc != 0 and ("required file not found" in out or "No such file" in out), (rc, out)
        print("PASS  without nix_ld the same binary fails: required file not found")
        refused("tb", "test -e /lib64 || [ -n \"$NIX_LD\" ]", "tb (no nix_ld): no /lib64, no NIX_LD")
        rc, out = u("APPZONES_NIX_LD_SHIM=/nonexistent/ld-linux-x86-64.so.2 appzones check 2>&1", check=False)
        print(out)
        assert rc != 0 and "programs.nix-ld.enable" in out, out
        print("PASS  the validator refuses nix_ld = true on a host without nix-ld")

    with subtest("tmp = disk: kept across a restart, on the host, old files removed"):
        T = "/home/alice/.local/share/appzones/td"
        ok("td", "echo kept >/tmp/keep && echo kept2 >/var/tmp/keep2 && mkdir -p /tmp/sub && echo s >/tmp/sub/f",
           "write /tmp and /var/tmp in td")
        machine.succeed(f"grep -qx kept {T}/tmp/keep && grep -qx kept2 {T}/var-tmp/keep2")
        assert machine.succeed(f"stat -c %a {T}/tmp {T}/var-tmp").split() == ["700", "700"]
        print("PASS  the files are on the host, in 0700 directories")
        u(f"touch -d '20 days ago' {T}/tmp/old {T}/tmp/sub/old; touch -d '40 days ago' {T}/var-tmp/old; "
          f"touch -d '20 days ago' {T}/var-tmp/young; ln -s /home/alice/.ssh/id_test {T}/tmp/link; "
          f"touch -h -d '20 days ago' {T}/tmp/link")
        u("appzones restart td")
        ok("td", "grep -qx kept /tmp/keep && grep -qx kept2 /var/tmp/keep2 && test -f /tmp/sub/f && test -f /var/tmp/young "
                 "&& ! test -e /tmp/old && ! test -e /tmp/sub/old && ! test -e /var/tmp/old && ! test -L /tmp/link",
           "after zone restart: kept files there, old ones (14 days /tmp, 30 days /var/tmp) removed")
        machine.succeed("test -f /home/alice/.ssh/id_test")
        print("PASS  cleanup removed the old symlink, not what it pointed to")
        out = u("appzones logs td")
        assert "unused for 14 days" in out, out
        refused("ta", "test -e /tmp/keep", "ta (tmp = ram) does not see it")

    with subtest("runtime mounts: appzones mount / umount / mounts"):
        HELPER = machine.succeed("dirname $(readlink -f $(command -v appzones))").strip() + "/../libexec/appzones/appzones-mount-helper"
        u("mkdir -p ~/proj ~/rosrc && echo A >~/proj/a.txt && echo R >~/rosrc/r.txt && ln -sfn ~/.ssh ~/sshlink")
        print(u("appzones mount td ~/proj"))
        ok("td", "grep -qx A /mnt/host/proj/a.txt && echo W >/mnt/host/proj/w.txt", "mounted rw: read and write")
        machine.succeed("grep -qx W /home/alice/proj/w.txt")
        print(u("appzones mount td ~/rosrc --ro --as ro"))
        ok("td", "grep -qx R /mnt/host/ro/r.txt", "ro mount readable")
        refused("td", "touch /mnt/host/ro/x", "ro mount not writable")
        refused("td", "unshare -Urm sh -c 'mount -o remount,bind,rw /mnt/host/ro && touch /mnt/host/ro/x'",
                "ro stays ro after a remount from a nested user namespace")
        refused("ta", "test -e /mnt/host/proj", "another zone does not see it")
        machine.succeed("mkdir -p /home/alice/proj/later && mount -t tmpfs later /home/alice/proj/later "
                        "&& touch /home/alice/proj/later/new && findmnt -no PROPAGATION /home/alice/proj/later")
        machine.fail("test -e /home/alice/.local/share/appzones/td/mnt/proj/later/new")
        refused("td", "test -e /mnt/host/proj/later/new",
                "a mount made under the source afterwards does not reach the zone (the copy is private)")
        machine.succeed("umount /home/alice/proj/later && rmdir /home/alice/proj/later")
        refused("td", "touch /mnt/host/planted", "/mnt/host itself is read-only in the zone")
        out = u("appzones mounts td")
        print(out)
        assert "proj" in out and "/home/alice/proj" in out and "ro" in out and "/home/alice/rosrc" in out, out

        # busy: an app with its cwd there
        u("appzones run td -- sh -c 'cd /mnt/host/proj && exec sleep 600'")
        machine.wait_until_succeeds("su -l alice -c " + shlex.quote(ENV + "appzones ps td | grep -q 'sleep 600'"), timeout=30)
        rc, out = u("appzones umount td proj 2>&1", check=False)
        print(out)
        assert rc != 0 and "busy" in out and "sleep 600" in out and "--lazy" in out, out
        print("PASS  a busy umount says which process holds it")
        ok("td", "test -f /mnt/host/proj/a.txt", "still mounted after the refused umount")
        print(u("appzones umount --lazy td proj"))
        refused("td", "test -e /mnt/host/proj/a.txt", "--lazy: gone from the zone at once")
        u("pkill -u alice -f '^sleep 600' || true")
        u("appzones umount td ro")
        refused("td", "test -e /mnt/host/ro", "umount removes it")
        assert "nothing mounted" in u("appzones mounts td")

        # never-list, through the CLI
        for src, why in [("~/.ssh", "never enters"), ("~/sshlink", "resolves to"), ("~/.config", "contains"),
                         ("/etc", "inside /etc"), ("/srv/code", "never enters"), ("~", "never allowed")]:
            rc, out = u(f"appzones mount td {src} 2>&1", check=False)
            assert rc != 0 and "refused" in out and why in out, (src, out)
        print("PASS  appzones mount refuses the never-list (also through a symlink)")

        # the root helper itself, called directly with sudo: same refusals
        def helper(args):
            rc, out = u(f"sudo -n {HELPER} {args} 2>&1", check=False)
            print(f"helper {args}: rc={rc} {out.strip()}")
            return rc, out
        for args, why in [("mount td x /home/alice/.ssh", "refused"), ("mount td x /home/alice/sshlink", "refused"),
                          ("mount td x /srv/locked/inner", "as alice"), ("mount td ../x /home/alice/proj", "name"),
                          ("mount ../td x /home/alice/proj", "not a zone name"), ("mount td x proj", "absolute")]:
            u("mkdir -p ~/.local/share/appzones/td/mnt/x")
            rc, out = helper(args)
            assert rc != 0 and why in out, (args, out)
        u("rmdir ~/.local/share/appzones/td/mnt/x")
        u("ln -s /etc ~/.local/share/appzones/td/mnt/evil")
        rc, out = helper("mount td evil /home/alice/proj")
        assert rc != 0 and "symlink" in out, out
        u("rm ~/.local/share/appzones/td/mnt/evil")
        u("mv ~/.local/share/appzones/td/mnt ~/.local/share/appzones/td/mnt.real && ln -s /etc ~/.local/share/appzones/td/mnt")
        rc, out = helper("mount td x /home/alice/proj")
        assert rc != 0 and "symlink" in out, out
        u("rm ~/.local/share/appzones/td/mnt && mv ~/.local/share/appzones/td/mnt.real ~/.local/share/appzones/td/mnt")
        # the state directory itself a symlink: mount, umount and umount-all refuse it
        u("mv ~/.local/share/appzones ~/.local/share/appzones.real && ln -s appzones.real ~/.local/share/appzones")
        try:
            for args in ("mount td x /home/alice/proj", "umount td x", "umount-all td"):
                rc, out = helper(args)
                assert rc != 0 and "symlink" in out, (args, out)
        finally:
            u("rm ~/.local/share/appzones && mv ~/.local/share/appzones.real ~/.local/share/appzones")
        print("PASS  the helper refuses a symlinked ~/.local/share/appzones (mount, umount, umount-all)")
        machine.succeed("! findmnt -rn -o TARGET | grep -q 'zones/td/mnt'")
        print("PASS  the helper refuses a target outside zones/*/mnt (symlinks), bad names, unreachable sources")
        rc, out = u(f"{HELPER} umount-all td 2>&1", check=False)
        assert rc != 0 and "sudo" in out, out
        rc, out = machine.execute(f"{HELPER} umount-all td 2>&1")
        assert rc != 0 and "SUDO_UID" in out, out
        rc, _ = u("sudo -n true", check=False)
        assert rc != 0
        rc, out = u(f"sudo -n -u nobody {HELPER} umount-all td 2>&1", check=False)
        assert rc != 0 and "appzones-mount-helper:" not in out, out  # sudo refuses, the helper never runs
        print("PASS  the helper runs only through sudo, as root for alice; sudo allows alice nothing else")
        refused("td", f"sudo -n {HELPER} umount-all td || /run/wrappers/bin/sudo -n true",
                "a zone cannot use sudo (no_new_privs, no wrappers)")

        # mounts end with the zone run
        u("appzones mount td ~/proj")
        ok("td", "test -f /mnt/host/proj/a.txt", "mounted again")
        # a mount under a name with a trailing newline, as older versions allowed: detached too
        machine.succeed("M=/home/alice/.local/share/appzones/td/mnt; mkdir \"$M/old\n\" "
                        "&& chown alice \"$M/old\n\" && mount --bind /home/alice/rosrc \"$M/old\n\" "
                        "&& findmnt -rn -o TARGET | grep -q 'td/mnt/old'")
        u("appzones restart td")
        refused("td", "test -e /mnt/host/proj", "appzones restart drops the mount")
        machine.succeed("! findmnt -rn -o TARGET | grep -q 'zones/td/mnt'")
        assert "nothing mounted" in u("appzones mounts td")
        print("PASS  no mount left on the host after the restart")
        u("appzones stop td")

    with subtest("Wayland: security context, window tag and colour"):
        out = ok("ta", "wayland-info", "wayland-info inside the zone")
        for hidden in ["zwlr_screencopy_manager_v1", "zwlr_data_control_manager_v1", "ext_data_control_manager_v1",
                       "zwlr_layer_shell_v1", "zwlr_virtual_pointer_manager_v1", "zwp_virtual_keyboard_manager_v1"]:
            assert hidden not in out, f"{hidden} visible to a zone client"
        assert "xdg_wm_base" in out
        host = u(f"WAYLAND_DISPLAY={WL} wayland-info")
        assert "zwlr_layer_shell_v1" in host, "control: the host client sees layer-shell"
        print("PASS  privileged globals hidden from the zone, visible to the host")

        u("appzones run ta foot --title zone-cli")
        machine.wait_until_succeeds(
            "su -l alice -c " + shlex.quote(ENV + "${hyprland}/bin/hyprctl -i 0 -j clients | grep -q zone-cli"), timeout=60)
        c = window("zone-cli")
        print(c)
        assert "zone:ta" in c["tags"], c
        prop = hypr(f"getprop address:{c['address']} active_border_color")
        print("border:", prop)
        assert "a6e3a1" in prop.lower(), prop
        machine.succeed(f"grep -qx {c['pid']} /tmp/other-handler")
        print("PASS  CLI-launched window tagged zone:ta, zone colour; the other open_early handler ran too")

        # from the launcher entry, as a launcher would run it (gio launch)
        u("appzones apps")
        u(f"WAYLAND_DISPLAY={WL} gio launch ~/.local/share/appzones/launcher/share/applications/appzones-foot-tb.desktop")
        machine.wait_until_succeeds(
            "su -l alice -c " + shlex.quote(ENV + "${hyprland}/bin/hyprctl -i 0 -j clients | jq -e '.[] | select(.tags | index(\"zone:tb\"))'"),
            timeout=60)
        print("PASS  launcher entry: window tagged zone:tb")

        # a host window: no zone tag
        u(f"WAYLAND_DISPLAY={WL} systemd-run --user --collect foot --title host-win")
        machine.wait_until_succeeds(
            "su -l alice -c " + shlex.quote(ENV + "${hyprland}/bin/hyprctl -i 0 -j clients | grep -q host-win"), timeout=60)
        c = window("host-win")
        assert not any(t.startswith("zone:") for t in c["tags"]), c
        print("PASS  host window not tagged")

        assert u(f"HYPRLAND_INSTANCE_SIGNATURE={SIG} appzones active").strip() in ("", "[ta]", "[tb]")

        # fails closed: windows the zone cannot be read for get zone:? and the warning colour
        def unidentified(title):
            machine.wait_until_succeeds(
                "su -l alice -c " + shlex.quote(ENV + f"${hyprland}/bin/hyprctl -i 0 -j clients | grep -q {title}"), timeout=60)
            c = window(title)
            print(c)
            assert "zone:?" in c["tags"] and not any(t.startswith("zone:") and t != "zone:?" for t in c["tags"]), c
            prop = hypr(f"getprop address:{c['address']} active_border_color")
            assert "f38ba8" in prop.lower(), prop

        # a zone appzones started itself (no unit, no cgroup to name it by)
        u(f"WAYLAND_DISPLAY={WL} APPZONES_SUPERVISOR=spawn appzones run tc -- foot --title spawn-win")
        unidentified("spawn-win")
        u("appzones stop tc")
        # under appzones.slice, but not a zone's unit
        u("systemd-run --user --collect --slice=appzones.slice --unit=not-a-zone foot --title slice-win")
        unidentified("slice-win")
        # a host process in a nested pid namespace (another sandbox): not a host window either
        u("systemd-run --user --collect unshare -Urpf foot --title nspid-win")
        unidentified("nspid-win")
        out = u(f"HYPRLAND_INSTANCE_SIGNATURE={SIG} appzones active").strip()
        assert out == "[?]", out  # the newest window has the focus
        print("PASS  fails closed: a zone without systemd, a cgroup under appzones.slice that is not a zone's, "
              "a nested pid namespace: zone:? and the warning colour")

    with subtest("single-instance GTK apps: the zone's own bus"):
        def zone_windows(zone, cls):
            return [c for c in clients() if f"zone:{zone}" in c["tags"] and cls in c["class"].lower()]

        def wait_windows(zone, cls, n):
            machine.wait_until_succeeds(
                "su -l alice -c " + shlex.quote(ENV + "${hyprland}/bin/hyprctl -i 0 -j clients | jq -e "
                    + shlex.quote(f'[.[] | select((.tags | index("zone:{zone}")) and (.class | ascii_downcase | contains("{cls}")))] | length >= {n}')),
                timeout=90)
            ws = zone_windows(zone, cls)
            print(json.dumps(ws, indent=1))
            return ws

        # the bug: on the host bus Thunar cannot own org.xfce.Thunar and exits;
        # `appzones run` says so, with its last output and the hint
        rc, out = u("appzones run --bus host ta -- thunar 2>&1", check=False)
        print(out)
        assert rc != 0 and "exited with status" in out and "could not own its D-Bus name" in out \
            and 'bus = "zone"' in out, out
        print("PASS  on the host bus Thunar exits; appzones run reports status, output and the bus hint")

        # a FIFO planted where zone-init writes the zone bus's config: it must neither block
        # zone-init nor be used (a new file is made)
        ok("ta", "rm -f /run/user/1000/zone-bus/session.conf; mkdir -p /run/user/1000/zone-bus && "
                 "mkfifo /run/user/1000/zone-bus/session.conf", "a FIFO planted at zone-bus/session.conf")
        u("appzones run ta thunar")
        w1 = wait_windows("ta", "thunar", 1)
        ok("ta", "test -f /run/user/1000/zone-bus/session.conf && ! test -p /run/user/1000/zone-bus/session.conf",
           "zone-init made its own session.conf (the FIFO did not block it)")
        pid = w1[0]["pid"]
        out = u("appzones logs ta")
        assert "zone bus up" in out and "(bus zone)" in out, out
        print("PASS  Thunar on the zone's own bus: a window, tagged zone:ta")
        # an ad-hoc command matching [apps.thunar] gets its bus, and the
        # second Thunar hands over to the first
        u("appzones run ta -- thunar")
        w2 = wait_windows("ta", "thunar", 2)
        assert {c["pid"] for c in w2} == {pid}, w2
        print("PASS  a second launch reuses the running Thunar (one process, two windows)")
        out = ok("ta", "DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/zone-bus/bus busctl --user list --acquired --no-pager",
                 "the zone bus lists Thunar's names")
        assert "org.xfce.Thunar" in out, out
        out = ok("ta", "busctl --user list --acquired --no-pager", "the host bus proxy")
        assert "org.xfce.Thunar" not in out, out
        machine.fail("su -l alice -c " + shlex.quote(ENV + "busctl --user status org.xfce.Thunar"))
        print("PASS  org.xfce.Thunar exists only on the zone's bus, never on the host's")

        u("appzones run ta text-editor")
        wait_windows("ta", "texteditor", 1)
        print("PASS  gnome-text-editor (GTK4, single instance) starts")
        u("appzones run ta eog")
        wait_windows("ta", "eog", 1)
        print("PASS  eog (single instance) starts")

        # bus = "none": no socket, no address
        rc, out = u("appzones run --wait --bus none ta -- bash -c "
                    + shlex.quote("ls -A /run/user/1000; test ! -e /run/user/1000/bus && test ! -e /run/user/1000/zone-bus "
                                  "&& [ -z \"$DBUS_SESSION_BUS_ADDRESS\" ] && test -S /run/user/1000/wayland-0 "
                                  "&& ! busctl --user list >/dev/null 2>&1 && test ! -e /run/zone/ctl.sock")
                    + " </dev/null 2>&1", check=False)
        print(out)
        assert rc == 0, out
        print("PASS  bus = none: no bus socket, no address, no zone-init control socket; the Wayland socket stays")

        # --wait hands only pipes, files and /dev/null in: never a socket or a directory (openat on a
        # directory reaches host files); a TCP socket from bash's /dev/tcp as stdin
        rc, out = u("exec 3<>/dev/tcp/127.0.0.1/8080; appzones run --wait ta -- true <&3 2>&1", check=False)
        assert rc != 0 and "only pipes" in out, out
        rc, out = u("appzones run --wait ta -- true </tmp 2>&1", check=False)
        assert rc != 0, out
        print("PASS  appzones run --wait refuses a socket and a directory as stdin")

        # an app that exits early: status and last output
        rc, out = u("appzones run ta -- sh -c 'echo early-boom >&2; exit 3' 2>&1", check=False)
        print(out)
        assert rc == 3 and "exited with status 3" in out and "early-boom" in out and "hint" not in out, out
        print("PASS  appzones run reports an app that exits early")
        rc, _ = u("APPZONES_RUN_CHECK=0 appzones run ta -- sh -c 'exit 4'", check=False)
        assert rc == 0
        assert "early-boom" in u("appzones logs ta")
        rc, out = u("timeout 3 appzones logs -f ta", check=False)
        assert rc == 124 and "early-boom" in out, (rc, out)
        print("PASS  appzones logs [-f]")

        # never owned on the host bus, whatever the TOML says
        for path in ["${badOwnToml "org.freedesktop.Notifications"}", "${badOwnToml "org.xfce.Thunar"}"]:
            rc, out = u(f"APPZONES_CONFIG={path} appzones check 2>&1", check=False)
            print(out)
            assert rc != 0 and "never granted" in out, out
        print("PASS  the validator refuses org.freedesktop.Notifications and org.xfce.Thunar as host-bus own")

    with subtest("Firefox in a zone keeps its own sandbox"):
        u("appzones run ta -- firefox --headless --no-remote http://198.51.100.2/")
        machine.wait_until_succeeds("su -l alice -c " + shlex.quote(ENV + "appzones ps ta | grep -q contentproc"), timeout=120)
        # one line per content process: "SECCOMP-MODE USERNS". The probe's own
        # bash also has "-contentproc" in its command line: skipped by name.
        # content processes come and go while Firefox starts, and take their seccomp filter
        # a moment after they appear: asked again until one is sandboxed (or 60 s)
        probe = ("for p in $(pgrep -f -- '-contentproc' | grep -v -x $$); do "
                 "[ \"$(cat /proc/$p/comm 2>/dev/null)\" = bash ] && continue; "
                 "echo $(awk '/^Seccomp:/{print $2}' /proc/$p/status 2>/dev/null) $(readlink /proc/$p/ns/user); done; "
                 "echo self $(readlink /proc/self/ns/user)")
        sandboxed, out = [], ""
        for _ in range(120):
            out = ok("ta", probe, "content processes listed") if not out else inside("ta", probe)[1]
            selfns = [l.split()[1] for l in out.splitlines() if l.startswith("self ")][0]
            procs = [l.split() for l in out.splitlines() if l.split()[:1] in (["0"], ["1"], ["2"])]
            sandboxed = [p for p in procs if len(p) > 1 and p[0] == "2" and p[1] != selfns]
            if sandboxed:
                break
            time.sleep(0.5)
        print(out)
        assert len(sandboxed) >= 1, f"no content process under seccomp in its own user namespace: {out}"
        print("PASS  content processes under seccomp in their own user namespace")

    with subtest("appzones shell: a pty inside"):
        out = u("${pkgs.python3}/bin/python3 ${shellDriver} ta")
        print(out)
        assert "ZONE_IS=ta" in out and "/dev/pts/" in out, out

    with subtest("stale config: warn, and run --restart restarts an idle zone"):
        u("appzones up tb")
        old = invocation("tb")
        u("sed -i 's/#89b4fa/#cba6f7/' ~/.config/appzones/zones.toml")
        u("appzones run tb -- true 2>&1 | grep -q 'appzones restart tb'")
        print("PASS  a stale zone warns")
        # tb has a foot window from the launcher test: --restart must refuse
        u("appzones run --restart tb -- true 2>&1 | tee /dev/stderr | grep -q 'not restarting'")
        u("pkill -u alice -x foot || true")
        machine.wait_until_succeeds("su -l alice -c " + shlex.quote(ENV + "[ -z \"$(appzones ps tb)\" ]"), timeout=30)
        u("appzones run --restart tb -- true 2>&1 | grep -q 'restarting it'")
        assert invocation("tb") != old
        print("PASS  run --restart restarted the idle stale zone")

    with subtest("stop, restart, crash"):
        old = invocation("ta")
        u("appzones restart ta")
        u("appzones ping ta")
        assert invocation("ta") != old
        print("PASS  restart")
        machine.succeed("pkill -9 -u alice -x zone-init")
        machine.wait_until_succeeds("su -l alice -c " + shlex.quote(ENV + "appzones ping ta"), timeout=60)
        n = u("systemctl --user show appzones@ta.service -p NRestarts --value").strip()
        assert int(n) >= 1, n
        print(f"PASS  crash: zone-init killed, unit restarted (NRestarts={n})")
        u("appzones run ta -- sleep 300")
        u("appzones stop --all")
        machine.fail("pgrep -u alice -x zone-init")
        machine.fail("pgrep -u alice -f 'sleep 300|xdg-dbus-proxy|zone-wayland|pasta|zone-bus/session.conf|thunar'")
        print("PASS  stop --all leaves no zone process")

    with subtest("appzones check --probe"):
        out = u("appzones check --probe ta")
        print(out)
        assert 'PASS  names resolve (dns = "host": ' in out, out
        u("appzones stop ta")

    with subtest("Firefox in two zones: each zone's own remote name"):
        def ff_windows(zone):
            return [c for c in clients() if f"zone:{zone}" in c["tags"] and "firefox" in c["class"].lower()]

        def wait_ff(zone, cond):
            machine.wait_until_succeeds(
                "su -l alice -c " + shlex.quote(ENV + "${hyprland}/bin/hyprctl -i 0 -j clients | jq -e "
                    + shlex.quote(f'[.[] | select((.tags | index("zone:{zone}")) and (.class | ascii_downcase | contains("firefox"))'
                                  f' and ({cond}))] | length >= 1')),
                timeout=180)

        u("appzones run ta firefox about:blank")
        wait_ff("ta", "true")
        u("appzones run tc firefox about:blank")
        wait_ff("tc", "true")
        print(json.dumps(ff_windows("ta") + ff_windows("tc"), indent=1))
        names = u("busctl --user list --acquired --no-pager")
        print(names)
        assert "org.mozilla.firefox_zone_ta." in names and "org.mozilla.firefox_zone_tc." in names, names
        print("PASS  each zone's Firefox owns its own remote name on the host bus")
        ta_before = [(c["pid"], c["title"]) for c in ff_windows("ta")]
        tc_pids = {c["pid"] for c in ff_windows("tc")}
        # a second launch in tc: handed to tc's Firefox (it exits, no profile
        # lock), never ta's. The page's title shows where it opened.
        ok("tc", "echo '<html><title>second-launch</title>x</html>' > ~/second.html", "a page in tc's home")
        u("appzones run tc firefox file:///home/alice/second.html")
        try:
            wait_ff("tc", '.title | contains("second-launch")')
        finally:
            print(u("appzones ps tc"))
            print(json.dumps(ff_windows("ta") + ff_windows("tc"), indent=1))
            print(u("appzones logs tc | tail -40"))
        machine.wait_until_succeeds("su -l alice -c " + shlex.quote(
            ENV + "[ $(appzones ps tc | grep -v -e contentproc -e crashhelper | grep -c 'firefox') -le 1 ]"), timeout=30)
        assert {c["pid"] for c in ff_windows("tc")} == tc_pids, ff_windows("tc")
        assert [(c["pid"], c["title"]) for c in ff_windows("ta")] == ta_before, ff_windows("ta")
        print("PASS  a second launch in tc opens in tc's Firefox, never ta's")
        u("appzones stop --all")

    with subtest("appzones serve: a foreground zone server, outside its unit, s6-style readiness"):
        appz = machine.succeed("readlink -f $(command -v appzones)").strip()
        # KillMode=process: stopping the unit signals only `serve` itself, so anything
        # it fails to clean up stays behind and is caught below
        u("systemd-run --user --collect --unit=serve-td -p KillMode=process /bin/sh -c "
          + shlex.quote(f"exec {appz} serve td --notify-fd 3 3>/tmp/td-ready"))
        machine.wait_until_succeeds("test -s /tmp/td-ready", timeout=90)
        assert machine.succeed("cat /tmp/td-ready") == "\n"
        print("PASS  readiness: a newline on --notify-fd once the zone answers")
        u("appzones ping td")
        u("! systemctl --user is-active appzones@td.service")
        ok("td", "true", "the zone runs apps without its systemd unit")
        # one server per zone: a second serve is refused and touches nothing
        rc, out = u("appzones serve td 2>&1", check=False)
        print(out)
        assert rc == 75 and "already served" in out, (rc, out)
        u("appzones ping td && test -S /run/user/1000/appzones/td/agent.sock && test -f /run/user/1000/appzones/td.lock")
        print("PASS  a second serve exits 75 (already served); the first zone keeps its socket and answers")
        # the unit, started against a zone served elsewhere: fails once, no restart loop
        u("! systemctl --user start appzones@td.service")
        machine.sleep(5)  # longer than RestartSec
        n = u("systemctl --user show appzones@td.service -p NRestarts --value").strip()
        assert n == "0", n
        u("appzones ping td && systemctl --user reset-failed appzones@td.service")
        print("PASS  appzones@td.service against a served zone: exit 75, no restart (NRestarts=0); the zone still answers")
        # stop follows who runs the zone: here the serve, not the unit
        u("appzones stop td")
        machine.wait_until_fails("su -l alice -c " + shlex.quote(ENV + "systemctl --user is-active serve-td"), timeout=60)
        machine.wait_until_fails("pgrep -u alice -f 'dev.mirql.zone.td|appzones/td/|zone-td-'", timeout=30)
        u("! appzones ping td")
        print("PASS  stop ends a serve run outside the unit; the zone and every helper are gone (nothing left behind)")

    with subtest("crash containment: kill -9 of the server leaves nothing behind"):
        appz = machine.succeed("readlink -f $(command -v appzones)").strip()
        for z, when in [("td", "running"), ("tb", "running"), ("td", "starting"), ("tb", "starting"), ("td", "stuck")]:
            unit = f"crash-{z}-{when}"
            u(f"rm -f /tmp/{z}-ready")
            # KillMode=process: systemd cleans up nothing; what stays is what the zone left
            slow = "-E APPZONES_BIN_BWRAP=${slowBwrap} " if when == "starting" else ""
            u(f"systemd-run --user --collect --unit={unit} -p KillMode=process {slow}/bin/sh -c "
              + shlex.quote(f"exec {appz} serve {z} --notify-fd 3 3>/tmp/{z}-ready"))
            if when in ("running", "stuck"):
                machine.wait_until_succeeds(f"test -s /tmp/{z}-ready", timeout=90)
                u(f"appzones ping {z}")
            if when == "stuck":
                # a zone process stops zone-init (same uid: it may): the zone still ends with
                # its server, from outside (pasta and bwrap die with it)
                init = machine.succeed(f"for p in $(pgrep -u alice -x zone-init); do tr '\\0' '\\n' </proc/$p/environ "
                                       f"| grep -qx ZONE={z} && echo $p; done").split()
                assert len(init) == 1, init
                # (from the host here: stopped from inside, `run --wait` would wait for its reply)
                machine.succeed(f"kill -STOP {init[0]}")
                assert "T" in machine.succeed(f"ps -o stat= -p {init[0]}"), "zone-init is not stopped"
            pid = "0"
            for _ in range(200):  # a live server, as early as possible
                pid = u(f"systemctl --user show {unit} -p MainPID --value").strip()
                if pid != "0":
                    break
                time.sleep(0.05)
            assert pid != "0", f"{unit}: no server to kill"
            if when == "starting":
                assert machine.succeed(f"cat /tmp/{z}-ready") == "", "the zone was ready already"
            machine.succeed(f"kill -9 {pid}")
            machine.wait_until_fails(f"pgrep -u alice -f 'dev.mirql.zone.{z}|appzones/{z}/' || pgrep -u alice -x zone-init", timeout=30)
            u(f"! appzones ping {z}")
            print(f"PASS  kill -9 of {z}'s server ({when}): zone-init, bwrap, pasta, the proxy and zone-wayland are gone")
            u(f"appzones up {z} && appzones ping {z} && appzones stop {z}")
            print(f"PASS  {z} starts again at once (the lock went with the killed server)")

    with subtest("start on demand without systemd"):
        S = "APPZONES_SUPERVISOR=spawn "

        def servers(z):
            return len(server_pids(z))

        # a stale agent socket (no one listening) does not get in the way
        u("mkdir -p /run/user/1000/appzones/tb && rm -f /run/user/1000/appzones/tb/agent.sock && ${pkgs.python3}/bin/python3 -c "
          "'import socket; socket.socket(socket.AF_UNIX).bind(\"/run/user/1000/appzones/tb/agent.sock\")'")
        u("test -S /run/user/1000/appzones/tb/agent.sock && ! appzones ping tb")
        u(S + "appzones run --wait tb -- true </dev/null 2>&1")
        u("appzones ping tb && ! systemctl --user is-active appzones@tb.service && test -s ~/.local/state/appzones/tb.log")
        assert servers("tb") == 1
        print("PASS  run starts a stopped zone itself: one server, no unit, its log in ~/.local/state/appzones")
        u(S + "appzones stop tb && ! appzones ping tb")
        assert servers("tb") == 0
        u("pids=; for i in $(seq 10); do " + S + "appzones run --wait tb -- true </dev/null >/dev/null 2>&1 & pids=\"$pids $!\"; done; "
          "for p in $pids; do wait $p || exit 1; done")
        assert servers("tb") == 1, servers("tb")
        print("PASS  ten runs at once on a stopped zone: all ran, exactly one server")
        u(S + "appzones stop tb")
        # a client killed before the zone is ready: zone-wayland made slow, so the start
        # is still going on when the client dies; the server finishes it
        zw = "${zoneWayland}"
        machine.succeed(f"printf '#!/bin/sh\\nsleep 4\\nexec {zw} \"$@\"\\n' >/tmp/slow-zw && chmod 755 /tmp/slow-zw")
        u("systemd-run --user --collect --unit=client-tb -p KillMode=process "
          "-E APPZONES_SUPERVISOR=spawn -E APPZONES_BIN_ZONE_WAYLAND=/tmp/slow-zw $(readlink -f $(command -v appzones)) up tb")
        machine.wait_until_succeeds(SERVER.format(z="tb"), timeout=30)
        u("! appzones ping tb")
        u("systemctl --user kill --kill-whom=main -s KILL client-tb")
        machine.wait_until_fails("su -l alice -c " + shlex.quote(ENV + "systemctl --user is-active client-tb"), timeout=30)
        machine.wait_until_succeeds("su -l alice -c " + shlex.quote(ENV + "appzones ping tb"), timeout=60)
        assert servers("tb") == 1
        print("PASS  a client killed before the zone was ready: the server finishes the start; one server")
        # zone-init, by its process name (whatever it is written in)
        init = machine.succeed("pgrep -u alice -x zone-init").split()
        assert len(init) == 1, init
        machine.succeed(f"kill -STOP {init[0]}")
        rc, out = u(S + "appzones up tb 2>&1", check=False)
        print(out)
        assert rc != 0 and "does not answer" in out, (rc, out)
        assert servers("tb") == 1
        machine.succeed(f"kill -CONT {init[0]}")
        print("PASS  a stuck zone: the client reports it, no second server")
        u(S + "appzones stop tb && ! appzones ping tb")

    with subtest("supervisor: auto picks the unit or appzones; never both; stop, restart, logs follow"):
        SP = "APPZONES_SUPERVISOR=spawn "

        def unit_active(z):
            return u(f"systemctl --user is-active appzones@{z}.service || true").strip() == "active"

        # a zone appzones started: the unit cannot start a second server, and does not loop
        u(SP + "appzones up tb")
        assert not unit_active("tb") and servers("tb") == 1
        u("! systemctl --user start appzones@tb.service")
        machine.sleep(5)  # longer than RestartSec
        assert u("systemctl --user show appzones@tb.service -p NRestarts --value").strip() == "0"
        u("systemctl --user reset-failed appzones@tb.service")
        assert servers("tb") == 1
        print("PASS  the unit against a zone appzones started: fails once, NRestarts=0, one server")
        # stop, restart and logs (default auto, the unit installed) follow who runs it
        u("appzones logs tb | tail -1 | grep -qxF \"$(tail -1 ~/.local/state/appzones/tb.log)\"")
        u("appzones restart tb && appzones ping tb")
        assert not unit_active("tb") and servers("tb") == 1
        u("appzones stop tb && ! appzones ping tb")
        assert servers("tb") == 0
        u("appzones up tb")
        assert unit_active("tb")
        u(SP + "appzones stop tb && ! appzones ping tb")
        assert not unit_active("tb") and servers("tb") == 0
        print("PASS  logs, restart and stop follow who runs the zone: appzones' server, or the unit")
        # auto without the unit (masked here): appzones starts the zone
        u("mkdir -p ~/.config/systemd/user && ln -sf /dev/null ~/.config/systemd/user/appzones@.service "
          "&& systemctl --user daemon-reload")
        u("appzones up tb && appzones ping tb")
        assert not unit_active("tb") and servers("tb") == 1
        u("appzones stop tb && rm ~/.config/systemd/user/appzones@.service && systemctl --user daemon-reload")
        print("PASS  auto without the unit: appzones starts the zone itself")
        # the user manager without WAYLAND_DISPLAY: a clear warning on start
        had = u("systemctl --user show-environment | grep '^WAYLAND_DISPLAY=' || true").strip()
        u("systemctl --user unset-environment WAYLAND_DISPLAY")
        out = u("WAYLAND_DISPLAY=wayland-9 appzones up tb 2>&1")
        print(out)
        assert "no WAYLAND_DISPLAY" in out, out
        u("appzones stop tb")
        if had:
            u("systemctl --user set-environment " + shlex.quote(had))
        print("PASS  a user manager without WAYLAND_DISPLAY: start warns")

    with subtest("zone settings: supervisor, linger"):
        # supervisor = "spawn" in the zone: appzones starts it, though the unit is installed
        u("appzones up tl && appzones ping tl")
        assert not unit_active("tl") and servers("tl") == 1
        print("PASS  supervisor = spawn in the zone: appzones starts it, not the unit")
        # linger = "10s": the zone stays while an app runs, and ends 10 s after the last one
        u("appzones run tl -- sleep 20")
        machine.sleep(16)
        u("appzones ping tl")
        machine.wait_until_fails("su -l alice -c " + shlex.quote(ENV + "appzones ping tl"), timeout=60)
        machine.wait_until_fails(SERVER.format(z="tl"), timeout=30)
        u("grep -q 'no app for 10s (linger); stopping' ~/.local/state/appzones/tl.log")
        print("PASS  linger: the zone stayed while its app ran, then ended on its own")
        # under systemd: ends with success, not restarted
        u("appzones up tm")
        assert unit_active("tm")
        machine.wait_until_fails("su -l alice -c " + shlex.quote(ENV + "systemctl --user is-active appzones@tm.service"), timeout=60)
        r = u("systemctl --user show appzones@tm.service -p Result -p NRestarts --value").split()
        assert r == ["success", "0"], r
        u("! appzones ping tm")
        print("PASS  linger under systemd: the unit ends with success and is not restarted")

    with subtest("process managers: runit, s6 and dinit run zones with contrib's files"):
        C = "$(dirname $(readlink -f $(command -v appzones)))/../share/appzones/contrib"

        def wait_up(z):
            machine.wait_until_succeeds("su -l alice -c " + shlex.quote(ENV + f"appzones ping {z}"), timeout=90)

        def wait_down(z):
            machine.wait_until_fails("su -l alice -c " + shlex.quote(ENV + f"appzones ping {z}"), timeout=60)
            machine.wait_until_fails(SERVER.format(z=z), timeout=30)

        def server_pid(z):
            return " ".join(server_pids(z))

        def wait_new_server(z, old):
            machine.wait_until_succeeds(f"p=$(fuser /run/user/1000/appzones/{z}.lock 2>/dev/null) && [ -n \"$p\" ] && [ $p != {old} ]", timeout=60)
            wait_up(z)

        # systemd: a stop (SIGTERM, exit 143) is a success, not a failed unit
        u("appzones up tb")
        assert unit_active("tb")
        u("systemctl --user stop appzones@tb.service")
        r = u("systemctl --user show appzones@tb.service -p Result --value").strip()
        assert r == "success", r
        print("PASS  systemd: stopping the unit ends it as a success (SuccessExitStatus=143)")
        # a stop while the zone starts: zone-wayland waits at a gate that never opens, so
        # the start is surely still going on. SIGINT (appzones stop) exits 0, SIGTERM
        # 143; neither is a failure
        zw = "${zoneWayland}"
        machine.succeed("printf '#!/bin/sh\\ntouch /tmp/zw-entered\\nwhile [ ! -e /tmp/zw-go ]; do sleep 0.1; done\\n"
                        f"exec {zw} \"$@\"\\n' >/tmp/gated-zw && chmod 755 /tmp/gated-zw")
        for how, want in [("appzones stop tb", "0"), ("systemctl --user kill -s TERM --kill-whom=main serve-slow", "143")]:
            u("rm -f /tmp/zw-entered /tmp/zw-go")
            u("systemd-run --user --unit=serve-slow -p RemainAfterExit=yes -E APPZONES_BIN_ZONE_WAYLAND=/tmp/gated-zw "
              "$(readlink -f $(command -v appzones)) serve tb")
            machine.wait_until_succeeds("test -e /tmp/zw-entered", timeout=30)
            u("! appzones ping tb")
            u(how)
            wait_down("tb")
            st = u("systemctl --user show serve-slow -p ExecMainStatus --value").strip()
            assert st == want, (how, st)
            u("systemctl --user stop serve-slow; systemctl --user reset-failed serve-slow || true")
        print("PASS  a stop while the zone starts: appzones stop exits 0, SIGTERM 143 (not a failure)")

        # runit: a failure is restarted; appzones stop and sv down keep it down
        u(f"rm -rf /tmp/rsv && mkdir /tmp/rsv && cp -r {C}/runit/appzones-ZONE /tmp/rsv/appzones-tb && chmod -R u+w /tmp/rsv")
        u("systemd-run --user --collect --unit=tree-runit runsvdir /tmp/rsv")
        wait_up("tb")
        assert servers("tb") == 1 and not unit_active("tb")
        pid = server_pid("tb")
        machine.succeed(f"kill -9 {pid}")
        wait_new_server("tb", pid)
        pid = server_pid("tb")
        u("sv restart /tmp/rsv/appzones-tb")
        wait_new_server("tb", pid)
        u("appzones stop tb")
        machine.sleep(4)  # longer than runit's restart delay
        u("! appzones ping tb && sv status /tmp/rsv/appzones-tb | grep -q '^down'")
        assert servers("tb") == 0
        u("sv up /tmp/rsv/appzones-tb")
        wait_up("tb")
        u("sv down /tmp/rsv/appzones-tb")
        wait_down("tb")
        u("systemctl --user stop tree-runit")
        print("PASS  runit: runs the zone, restarts it after kill -9 and sv restart, keeps it down after appzones stop and sv down")
        # s6: readiness through notification-fd; finish keeps a stopped zone down
        u(f"rm -rf /tmp/s6 && mkdir /tmp/s6 && cp -r {C}/s6/appzones-ZONE /tmp/s6/appzones-tb && chmod -R u+w /tmp/s6")
        u("systemd-run --user --collect --unit=tree-s6 s6-svscan /tmp/s6")
        machine.wait_until_succeeds("test -p /tmp/s6/appzones-tb/supervise/control", timeout=30)
        u("s6-svwait -U -t 90000 /tmp/s6/appzones-tb && appzones ping tb")
        pid = server_pid("tb")
        u("s6-svc -r /tmp/s6/appzones-tb")
        wait_new_server("tb", pid)
        u("appzones stop tb")
        machine.sleep(4)
        u("! appzones ping tb && [ $(s6-svstat -o up /tmp/s6/appzones-tb) = false ]")
        u("s6-svc -u /tmp/s6/appzones-tb && s6-svwait -U -t 90000 /tmp/s6/appzones-tb && appzones ping tb")
        u("s6-svc -d /tmp/s6/appzones-tb && s6-svwait -D -t 30000 /tmp/s6/appzones-tb")
        wait_down("tb")
        u("systemctl --user stop tree-s6")
        print("PASS  s6: ready when the zone answers (s6-svwait -U); s6-svc -r restarts it; stays down after appzones stop; s6-svc -d stops it")
        # dinit: one file for every zone (appzones@NAME); linger ends a zone for good
        u(f"rm -rf /tmp/dinit.d && mkdir /tmp/dinit.d && cp {C}/dinit/appzones /tmp/dinit.d/ "
          "&& printf 'type = internal\\n' >/tmp/dinit.d/boot")
        u("systemd-run --user --collect --unit=tree-dinit dinit --user --services-dir /tmp/dinit.d --socket-path /tmp/dinit.sock")
        machine.wait_until_succeeds("test -S /tmp/dinit.sock", timeout=30)
        D = "dinitctl --socket-path /tmp/dinit.sock "
        u(D + "start appzones@tb && appzones ping tb")
        pid = server_pid("tb")
        u(D + "restart appzones@tb")
        wait_new_server("tb", pid)
        u("appzones stop tb")
        machine.sleep(4)
        u("! appzones ping tb && " + D + "status appzones@tb | grep -q STOPPED")
        u(D + "start appzones@tl && appzones ping tl")
        wait_down("tl")  # linger = "10s", no app
        machine.sleep(4)
        u("! appzones ping tl && " + D + "status appzones@tl | grep -q STOPPED")
        u(D + "start appzones@tb && " + D + "stop appzones@tb")
        wait_down("tb")
        u("systemctl --user stop tree-dinit")
        print("PASS  dinit: appzones@NAME starts once the zone answers; dinitctl restart works; a stopped or lingered-out zone is not restarted")

    with subtest("DNS in the zone: zone-init, the server, the host's resolver"):
        def zdig(args, check=True):
            return u(f"appzones run --wait tc -- dig +time=3 +tries=1 {args} </dev/null 2>&1", check=check)

        # (inside sh -c: `run` refuses a host path as an argument)
        u("appzones run --wait tc -- sh -c 'grep -qx \"nameserver 127.0.0.53\" /etc/resolv.conf' </dev/null 2>&1")
        out = u("appzones run --wait tc -- getent hosts zone-test.example </dev/null 2>&1")
        assert "192.0.2.99" in out, out
        assert zdig("+short zone-test.example").strip() == "192.0.2.99"
        assert zdig("+tcp +short zone-test.example").strip() == "192.0.2.99"
        print("PASS  DNS in a zone: names from the host's loopback resolver, over UDP and TCP")
        out = zdig("big.example TXT")
        print(out)
        assert "Truncated, retrying in TCP mode" in out and out.count("\tTXT\t") == 8, out
        print("PASS  DNS in a zone: an answer too big for UDP comes truncated, then whole over TCP")
        rc, out = zdig("@192.0.2.1 +short zone-test.example", check=False)
        assert rc != 0 and "192.0.2.99" not in out, (rc, out)
        print("PASS  DNS in a zone: the host's resolver at its own address stays unreachable")
        u("appzones stop tc")

    with subtest("dns = public refused on the host (a VPN's DNS leak protection, #171)"):
        # as Mullvad's firewall: DNS only to its own resolver; a send to 1.1.1.1:53 fails (EPERM)
        machine.succeed("nft add table inet leak && nft add chain inet leak out '{ type filter hook output priority 0; }' && "
                        "nft add rule inet leak out ip daddr '{ 1.1.1.1, 1.0.0.1 }' meta l4proto '{ udp, tcp }' th dport 53 drop")
        said = ('names do not resolve: dns = "public": 1.1.1.1:53 and 1.0.0.1:53 refused on this host '
                '(Operation not permitted); a VPN\'s or a firewall\'s DNS leak protection? dns = "host" asks the host\'s resolver')
        u("appzones run --wait tp -- true </dev/null 2>&1")
        machine.wait_until_succeeds("su -l alice -c " + shlex.quote(ENV + "appzones logs tp | grep -qF " + shlex.quote(said)), timeout=30)
        rc, out = u("appzones run --wait tp -- dig +time=2 +tries=1 example.com </dev/null 2>&1", check=False)
        assert rc != 0, (rc, out)
        logs = u("appzones logs tp")
        assert logs.count("names do not resolve") == 1, logs
        print("PASS  dns = public: the zone's log says why names do not resolve, once")
        rc, out = u("appzones check --probe tp", check=False)
        print(out)
        assert rc == 1 and "FAIL  names resolve: " + said[len("names do not resolve: "):] in out, (rc, out)
        print("PASS  dns = public: check --probe fails, saying why")
        u("appzones stop tp")
        machine.succeed("nft delete table inet leak")
  '';
}
