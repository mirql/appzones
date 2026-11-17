# appzones. Nothing here touches a live session: the checks read files, the
# VM test runs in a VM.

default:
    @just --list

# Unit tests of the validator and parts of zone.py (fast, no Nix)
test:
    cd src && python3 -B -m unittest -q test_zonecfg

# Validate a zones.toml (default: the example)
check file="examples/zones.toml":
    python3 -B src/zonecfg.py check {{file}}

# Build the package (runs the unit tests too)
build:
    nix build -L .#default

# NixOS VM test: Hyprland, zone units, isolation probes, window tags (~6 min, needs /dev/kvm)
test-vm:
    nice -n 19 nix build -L --no-link .#checks.x86_64-linux.vm

# Format the Nix files
fmt:
    nix fmt
