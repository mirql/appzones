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

# Record the README's terminal demos in a VM: docs/demo-*.gif, docs/demo-*.cast (needs /dev/kvm)
demo:
    nice -n 19 nix build -L -o result-demo .#demo
    install -m 644 result-demo/* docs/
    rm result-demo

# Tag VERSION on origin/main and publish a GitHub release with its CHANGELOG section as the notes
release version:
    #!/usr/bin/env bash
    set -euo pipefail
    git fetch -q origin
    grep -q 'version = "{{version}}";' <(git show origin/main:nix/package.nix) \
        || { echo "nix/package.nix on origin/main is not {{version}}"; exit 1; }
    notes=$(git show origin/main:CHANGELOG.md | awk '/^## {{version}}$/ {on=1; next} /^## / {on=0} on')
    [ -n "$(echo "$notes" | tr -d '[:space:]')" ] || { echo "no '## {{version}}' section in CHANGELOG.md on origin/main"; exit 1; }
    ! git ls-remote --exit-code --tags origin "v{{version}}" >/dev/null || { echo "v{{version}} exists already"; exit 1; }
    echo "$notes"
    read -rp "Publish v{{version}} on $(git rev-parse --short origin/main) with these notes? [y/N] " ok
    [ "$ok" = y ] || exit 1
    gh release create "v{{version}}" --target "$(git rev-parse origin/main)" --title "appzones {{version}}" --notes "$notes"

# Format the Nix files
fmt:
    nix fmt
