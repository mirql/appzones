# appzones. Nothing here touches a live session: the checks read files, the
# VM test runs in a VM.

default:
    @just --list

# Unit tests of the validator and parts of the appzones modules (fast, no Nix)
test:
    cd src && python3 -B -m unittest -q test_zonecfg

# Validate a zones.toml (default: the example)
check file="examples/zones.toml":
    python3 -B src/zonecfg.py check {{file}}

# Build the package (runs the unit tests too)
build:
    nix build -L .#default

# `make install` into a staging directory: every file in place, the sudo rule valid, uninstall clean
test-install:
    nice -n 19 nix build -L --no-link .#checks.x86_64-linux.make-install

# appzones on Ubuntu 24.04 and Debian 12, in their disk images: make install and the .deb, zones as a user (~3 min each)
test-distros:
    nice -n 19 nix build -L --no-link .#checks.x86_64-linux.debian12 .#checks.x86_64-linux.ubuntu2404 \
        .#checks.x86_64-linux.debian12-deb .#checks.x86_64-linux.ubuntu2404-deb

# The .deb (amd64, built in a Debian 12 image): result-deb/appzones_VERSION-1_amd64.deb
deb:
    nice -n 19 nix build -L -o result-deb .#deb

# NixOS VM test: Hyprland, zone units, isolation probes, window tags (~6 min, needs /dev/kvm)
test-vm:
    nice -n 19 nix build -L --no-link .#checks.x86_64-linux.vm

# The Rust helpers (helper/): cargo test, the protocol and never-list tests, the VM and distro tests with them (~15 min)
test-rust:
    nice -n 19 nix build -L --no-link .#checks.x86_64-linux.helper .#checks.x86_64-linux.policy-rust \
        .#checks.x86_64-linux.agent-protocol-rust .#checks.x86_64-linux.vm-rust \
        .#checks.x86_64-linux.debian12-rust .#checks.x86_64-linux.ubuntu2404-rust

# The Rust helpers' dependencies: advisories, licenses, sources (cargo deny; needs the network)
audit:
    cd helper && nix shell --inputs-from .. nixpkgs#cargo nixpkgs#cargo-deny -c cargo deny check

# Record the README's terminal demos in a VM: docs/demo-*.gif, docs/demo-*.cast (needs /dev/kvm)
demo:
    nice -n 19 nix build -L -o result-demo .#demo
    install -m 644 result-demo/* docs/
    rm result-demo

# Tag VERSION on origin/main (as fetched now) and publish a GitHub release with its CHANGELOG section as the notes
release version:
    #!/usr/bin/env bash
    set -euo pipefail
    git fetch -q origin
    sha=$(git rev-parse origin/main)  # one commit for every check, the prompt and the tag
    grep -q 'version = "{{version}}";' <(git show "$sha":nix/package.nix) \
        || { echo "nix/package.nix at ${sha:0:7} is not {{version}}"; exit 1; }
    notes=$(git show "$sha":CHANGELOG.md | awk '/^## {{version}}$/ {on=1; next} /^## / {on=0} on')
    [ -n "$(echo "$notes" | tr -d '[:space:]')" ] || { echo "no '## {{version}}' section in CHANGELOG.md at ${sha:0:7}"; exit 1; }
    ! git ls-remote --exit-code --tags origin "v{{version}}" >/dev/null || { echo "v{{version}} exists already"; exit 1; }
    echo "$notes"
    read -rp "Publish v{{version}} on ${sha:0:7} with these notes? [y/N] " ok
    [ "$ok" = y ] || exit 1
    gh release create "v{{version}}" --target "$sha" --title "appzones {{version}}" --notes "$notes"
    # the .deb, built from exactly that commit
    deb=$(nice -n 19 nix build --no-link --print-out-paths "github:mirql/appzones/$sha#deb")
    gh release upload "v{{version}}" "$deb"/appzones_{{version}}-1_amd64.deb

# Format the Nix files
fmt:
    nix fmt
