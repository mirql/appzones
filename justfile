# appzones. Nothing here touches a live session: the checks read files, the
# VM test runs in a VM.

default:
    @just --list

# Every quick offline check, before a PR: the Rust tests and clippy, the examples, the secret scan
check: test examples secret-scan

# The Rust tests and clippy (no Nix; the golden tests run in `just build`)
test:
    cd helper && cargo test --release -q && cargo clippy --release -q --all-targets -- -D warnings

# Validate a zones.toml
validate file:
    cd helper && APPZONES_CONFIG="$(realpath ../{{file}})" cargo run --release -q --bin appzones-helper -- cli check

# The example configurations are valid, on any host: a home of their own (the folders
# they bind), a stand-in nix-ld (`work` has nix_ld = true), no host never-list or D-Bus names
examples:
    #!/usr/bin/env bash
    set -euo pipefail
    t=$(mktemp -d); trap 'rm -rf "$t"' EXIT
    mkdir -p "$t"/home/{Downloads,Pictures,Documents,code} "$t"/nix-ld/lib
    touch "$t"/nix-ld/shim "$t"/nix-ld/lib/ld.so "$t"/denied-paths
    for f in examples/zones.toml examples/minimal.toml; do
        (cd helper && APPZONES_HOME_FOR_TESTS="$t/home" APPZONES_CONFIG="$(realpath ../$f)" \
            APPZONES_NIX_LD_SHIM="$t/nix-ld/shim" APPZONES_NIX_LD_LIB="$t/nix-ld/lib" \
            APPZONES_DENIED_PATHS="$t/denied-paths" APPZONES_DBUS_SERVICE_DIRS= \
            cargo run --release -q --bin appzones-helper -- cli check)
    done

# Secrets in the history or the files (committed, changed or untracked): gitleaks, from `nix develop`
# (rules: .gitleaks.toml). Without a readable history it fails: an empty scan finds nothing.
secret-scan:
    #!/usr/bin/env bash
    set -euo pipefail
    git rev-parse -q --verify HEAD >/dev/null || { echo "secret-scan: no git history here to scan" >&2; exit 1; }
    gitleaks git --no-banner --redact --log-level warn .
    gitleaks dir --no-banner --redact --log-level warn .

# Build the package (every Rust test runs in its build, the golden files' too)
build:
    nix build -L .#default

# `make install` into a staging directory: every file in place, the sudo rule valid, uninstall clean
test-install:
    nice -n 19 nix build -L --no-link .#checks.x86_64-linux.make-install

# appzones on Ubuntu 24.04 and Debian 12, in their disk images: make install and the .deb, zones as a user (~3 min each)
test-distros:
    nice -n 19 nix build -L --no-link .#checks.x86_64-linux.debian12 .#checks.x86_64-linux.ubuntu2404 \
        .#checks.x86_64-linux.debian12-deb .#checks.x86_64-linux.ubuntu2404-deb

# The .deb (amd64, glibc 2.17 and later): result-deb/appzones_VERSION-1_amd64.deb
deb:
    nice -n 19 nix build -L -o result-deb .#deb

# appzones on Arch: the AUR package built from this checkout, installed, the distro test (needs the network and /dev/kvm, ~10 min)
test-arch:
    nix shell --inputs-from . nixpkgs#qemu nixpkgs#cdrkit nixpkgs#openssh nixpkgs#curl -c nice -n 19 tests/cloud-test.sh arch

# appzones on openSUSE Leap 16 (SELinux enforcing): make install from this checkout, the distro test (network, /dev/kvm, ~15 min)
test-opensuse:
    nix shell --inputs-from . nixpkgs#qemu nixpkgs#cdrkit nixpkgs#openssh nixpkgs#curl -c nice -n 19 tests/cloud-test.sh opensuse

# appzones on Fedora (SELinux enforcing): the RPM built from this checkout, the distro test (needs the network and /dev/kvm, ~15 min)
test-fedora:
    nix shell --inputs-from . nixpkgs#qemu nixpkgs#cdrkit nixpkgs#openssh nixpkgs#curl -c nice -n 19 tests/cloud-test.sh fedora

# NixOS VM test: Hyprland, zone units, isolation probes, window tags (~6 min, needs /dev/kvm)
test-vm:
    nice -n 19 nix build -L --no-link .#checks.x86_64-linux.vm

# Every offline check: the builds and their tests, make install, the repository, the distributions, the VM (~25 min)
test-all:
    nice -n 19 nix flake check -L

# On a Mac over ssh (HOST: user@address; it has Nix): this commit there, cargo test and clippy in the dev shell
test-macos host:
    #!/usr/bin/env bash
    set -euo pipefail
    sha=$(git rev-parse HEAD)
    ssh "{{host}}" 'test -d ~/src/appzones || git clone -q https://github.com/mirql/appzones ~/src/appzones'
    # a ref of its own for each commit: never a branch to move (an amend, another branch)
    git push -q "ssh://{{host}}/~/src/appzones" "HEAD:refs/test-macos/$sha"
    ssh "{{host}}" "cd ~/src/appzones && git checkout -q --detach $sha && nix develop -c sh -c 'cd helper && cargo clippy --release -q --all-targets -- -D warnings && cargo test --release -q'"

# On a Mac over ssh: zones themselves, this commit (tests/macos.sh: three zones, check --probe,
# binds, a zone's end, refusals; in a temporary folder there, never the user's own zones),
# then a nix-darwin system with the module (tests/darwin-module.nix, built, not activated)
test-macos-zones host:
    #!/usr/bin/env bash
    set -euo pipefail
    sha=$(git rev-parse HEAD)
    ssh "{{host}}" 'test -d ~/src/appzones || git clone -q https://github.com/mirql/appzones ~/src/appzones'
    git push -q "ssh://{{host}}/~/src/appzones" "HEAD:refs/test-macos/$sha"
    # (the checkout goes back to where it was, also when a test fails)
    ssh "{{host}}" "cd ~/src/appzones && was=\$(git symbolic-ref -q --short HEAD || git rev-parse HEAD) && git checkout -q --detach $sha && { nix develop -c sh -c 'cd helper && cargo build --release -q' && sh tests/macos.sh helper/target/release/appzones-helper && nix build --impure --no-link --expr '(import ./tests/darwin-module.nix).check'; rc=\$?; git checkout -q \$was; exit \$rc; }"

# The Rust dependencies: advisories, licenses, sources (cargo deny; needs the network)
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
    # the .deb, built from exactly that commit, before anything is published
    deb=$(nice -n 19 nix build --no-link --print-out-paths "github:mirql/appzones/$sha#deb")
    gh release create "v{{version}}" --target "$sha" --title "appzones {{version}}" --notes "$notes"
    gh release upload --clobber "v{{version}}" "$deb"/appzones_{{version}}-1_amd64.deb \
        || echo "the .deb was not attached: just release-deb {{version}}"

# Build the RPM from the commit tag vVERSION points to, test it on Fedora, attach it to the release (network, /dev/kvm)
release-rpm version:
    #!/usr/bin/env bash
    set -euo pipefail
    git fetch -q --tags origin
    tree=$(mktemp -d) out=$(mktemp -d)
    trap 'git worktree remove --force "$tree"; rm -rf "$out"' EXIT
    git worktree add -q --detach "$tree" "v{{version}}"
    APPZONES_RPM_OUT="$out" nix shell --inputs-from . nixpkgs#qemu nixpkgs#cdrkit nixpkgs#openssh nixpkgs#curl \
        -c nice -n 19 "$tree/tests/cloud-test.sh" fedora
    gh release upload --clobber "v{{version}}" "$out"/appzones-{{version}}-1.fc*.x86_64.rpm

# Attach (again) the .deb to release VERSION, built from the commit its tag points to
release-deb version:
    #!/usr/bin/env bash
    set -euo pipefail
    git fetch -q --tags origin
    sha=$(git rev-list -n 1 "v{{version}}")
    deb=$(nice -n 19 nix build --no-link --print-out-paths "github:mirql/appzones/$sha#deb")
    gh release upload --clobber "v{{version}}" "$deb"/appzones_{{version}}-1_amd64.deb

# Format the Nix files
fmt:
    nix fmt
