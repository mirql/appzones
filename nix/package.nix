# appzones: `appzones` (CLI and supervisor), `zone-init` (the agent inside a
# zone) and `zone-wayland` (the security-context helper). The tools a zone
# needs at run time (bwrap, pasta, nft, xdg-dbus-proxy, dbus-daemon for a
# zone's own bus, ip) come from the store, so the package works whatever is
# on the caller's PATH. The unit tests run at build time.
{
  lib,
  callPackage,
  runCommand,
  makeWrapper,
  python3,
  bubblewrap,
  passt,
  nftables,
  xdg-dbus-proxy,
  dbus,
  iproute2,
  procps,
  # the Rust helpers (nix/helper.nix, design/rust.md): when given, appzones runs them
  # instead of the Python and C ones it has replaced so far
  helper ? null,
  # the Rust root step of `appzones mount` (nix/helper.nix, bin = "appzones-mount-helper"):
  # when given, it is the mount helper, at the same path, instead of the Python one
  rustMountHelper ? null,
}:

let
  # the package version; the config schema has its own release number
  # (zonecfg.RELEASE, what a zones.toml can require)
  version = "0.9.0";
  zone-wayland = callPackage ../src/zone-wayland { };
  runtimePath = lib.makeBinPath [
    bubblewrap
    passt
    nftables
    xdg-dbus-proxy
    dbus
    iproute2
    procps
    zone-wayland
  ];
  src = ../src;
  examples = ../examples;
  hyprland = ../contrib/hyprland; # only for the unit tests (zones.lua is a copy of appzones.lua)
  contrib = ../contrib; # service files for other process managers, in share/appzones/contrib
in
runCommand "appzones-${version}"
  {
    nativeBuildInputs = [
      python3
      makeWrapper
    ];
    passthru = {
      inherit zone-wayland;
      mountHelper = "libexec/appzones/appzones-mount-helper";
    };
    meta = {
      description = "App zones: long-lived, easy to manage sandboxes for desktop apps (bubblewrap + pasta)";
      homepage = "https://github.com/mirql/appzones";
      license = lib.licenses.mit;
      platforms = [ "x86_64-linux" ];
      mainProgram = "appzones";
    };
  }
  ''
    lib=$out/libexec/appzones
    mkdir -p $lib $out/bin $out/share/appzones
    cp --no-preserve=mode ${src}/zone.py $lib/zone.py
    # `appzones --version` prints the package version
    substituteInPlace $lib/zone.py --replace-fail '@version@' '${version}'
    cp -r --no-preserve=mode ${src}/appzones $lib/appzones
    cp ${src}/zone_init.py $lib/zone_init.py
    cp ${src}/zonecfg.py $lib/zonecfg.py
    cp ${examples}/zones.toml $out/share/appzones/zones.example.toml
    cp ${examples}/minimal.toml $out/share/appzones/zones.minimal.toml
    cp -r --no-preserve=mode ${contrib} $out/share/appzones/contrib
    chmod 755 $out/share/appzones/contrib/{runit,s6}/appzones-ZONE/{run,finish}

    # The root helper of `appzones mount` (the module's sudo rule names this
    # file): isolated Python (-I: no PYTHON* variables, no user site, not
    # even its own directory on sys.path; it adds $lib itself).
    ${
      if rustMountHelper != null then
        "install -m 555 ${rustMountHelper}/bin/appzones-mount-helper $lib/appzones-mount-helper"
      else
        ''
          { echo "#!${python3}/bin/python3 -IB"; cat ${src}/zone_mount_helper.py; } > $lib/appzones-mount-helper
          chmod 555 $lib/appzones-mount-helper
        ''
    }

    # unit tests of the validator (and parts of the appzones modules), against a fake home and sysfs
    work=$(mktemp -d)
    mkdir -p $work/src $work/examples $work/contrib/hyprland
    cp ${src}/zonecfg.py ${src}/zone.py ${src}/zone_init.py ${src}/zone_mount_helper.py ${src}/test_zonecfg.py $work/src/
    cp -r --no-preserve=mode ${src}/appzones $work/src/appzones
    cp ${examples}/*.toml $work/examples/
    cp ${hyprland}/*.lua $work/contrib/hyprland/
    (cd $work/src && HOME=$work python3 -B -m unittest -q test_zonecfg)
    python3 -B -m py_compile $lib/zone.py $lib/zone_init.py $lib/appzones/*.py
    ${lib.optionalString (
      rustMountHelper == null
    ) "python3 -B -c 'import ast, sys; ast.parse(open(sys.argv[1]).read())' $lib/appzones-mount-helper"}

    # /run/current-system/sw/bin last: systemctl, journalctl, nix and hyprctl
    # are the running system's.
    ${lib.optionalString (
      helper != null
    ) "install -m 555 ${helper}/bin/appzones-helper $lib/appzones-helper"}
    makeWrapper ${python3}/bin/python3 $out/bin/appzones \
      --add-flags "-B $lib/zone.py" ${
        lib.optionalString (helper != null) "--set APPZONES_HELPER rust"
      } \
      --prefix PATH : ${runtimePath} \
      --suffix PATH : /run/current-system/sw/bin
  ''
