# Agent notes for appzones

- Code: `helper/`, one Rust binary (`appzones` is `appzones-helper cli`): `cli.rs` the
  commands, `serve.rs` the server, `plan.rs` the zone's plan and bwrap command line,
  `netsetup.rs` and `host_dns.rs` the network, `zone_init.rs` the agent inside a zone,
  `zone_wayland.rs` the security-context helper, `config.rs` and `policy.rs` the schema,
  validator and never-lists; `mount_helper.rs` is the root step of `appzones mount`, its
  own binary. Tests: units in the modules, `helper/tests/` (`cli.rs`, `agent.rs`,
  `golden.rs`: Python's answers, kept in `helper/tests/golden/`). Nix: `nix/package.nix`,
  `nix/module.nix`, `nix/portable.nix` (the other distributions' binaries), `tests/vm.nix`.
- Outputs and messages are an interface: tests (and users' scripts) match them. Keep them
  word for word unless the change is the point.
- User docs: the pages in `docs/` (the site https://mirql.github.io/appzones/; sidebar and titles in
  `docs/_config.yml`). When behaviour or a config key changes, update the page
  it belongs on (keys: `docs/configuration/reference.md`) and `examples/zones.toml`.
  `docs/recipes.md` repeats the README's recipes: change both. A new docs page
  goes into `docs/_data/llms.yml` too (llms.txt, llms-full.txt; the check `repo` checks).
- `skills/appzones/SKILL.md` is the skill users give their own agents. When a
  command, a flag, a config key or a path changes, update it in the same PR.
- The README's terminal GIFs (`docs/demo-*.gif`) are recorded in a VM from
  `tests/demo.nix`. When a command's name, arguments or output there
  changes, run `just demo` and commit the new GIFs.
- Never test on a live desktop session (no zone units, windows or
  screenshots there). Use `just test`, `just build` and `just test-vm`.
- Adding a config key or value: bump `RELEASE` in `helper/src/config.rs`, record it in
  `SINCE` (the package version in nix/package.nix and helper/Cargo.toml is separate).
- `mount_helper.rs` runs as root through sudo. It reads no environment and keeps
  `unsafe` in its `mod sys`; keep it that way.
- `main` takes only squash merges through a PR.
- SonarQube Cloud checks every PR. A finding that is not a real problem (a validated
  argument, a test address) gets the reason in a comment on the line above and a bare
  `// NOSONAR` at the end of its own line. A bare NOSONAR hides every finding on that
  line, so keep such lines small. A maintainer marks the rest false positive in
  SonarQube, again when the line changes.
- A release PR bumps the version in `nix/package.nix`, `helper/Cargo.toml` and `helper/Cargo.lock`
  (the package build fails when they differ) and `Version:` in `contrib/fedora/appzones.spec`.
  The RPM and the AUR package build with their distribution's cargo (crates from crates.io,
  pinned by Cargo.lock: the build VM needs the network); the .deb has nix/portable.nix's binaries.
- After a release: `just release-rpm VERSION` builds the RPM from the tag, tests it on Fedora, attaches it.
- Before an AUR update: `just test-arch` (the package built with makepkg from the checkout, then the
  distro test, in an Arch VM). `just test-fedora` builds and tests the RPM on Fedora, `just test-opensuse` tests `make install` on openSUSE.
- A release (`just release VERSION`) attaches the `.deb` (built from the tagged commit:
  `nix build github:mirql/appzones/SHA#deb`), and is followed by the AUR package: in `contrib/arch/`,
  `pkgver`, the tarball's sha256 and `.SRCINFO` (a PR), then the same four files pushed
  to `ssh://aur@aur.archlinux.org/appzones.git` (needs the maintainer's AUR key).
