# Agent notes for appzones

- Code: `src/` (`zone.py` the entry point and `appzones/` its modules: `client`
  the commands, `server` the supervisor, `fs` the bwrap command line, `net`,
  `portals`, `mounts`, `agent`, `upgrade`, `common` settings read as `common.X`;
  `zone_init.py` the agent
  inside a zone, `zonecfg.py` schema and validator, `zone_mount_helper.py`
  the root step of `appzones mount`, `zone-wayland/` the security-context
  helper). Nix: `nix/package.nix`, `nix/module.nix`, `tests/vm.nix`.
- User docs: the pages in `docs/` (the site https://mirql.github.io/appzones/; sidebar and titles in
  `docs/_config.yml`). When behaviour or a config key changes, update the page
  it belongs on (keys: `docs/configuration/reference.md`) and `examples/zones.toml`.
  `docs/recipes.md` repeats the README's recipes: change both. A new docs page
  goes into `docs/_data/llms.yml` too (llms.txt, llms-full.txt; a unit test checks).
- `skills/appzones/SKILL.md` is the skill users give their own agents. When a
  command, a flag, a config key or a path changes, update it in the same PR.
- The README's terminal GIFs (`docs/demo-*.gif`) are recorded in a VM from
  `tests/demo.nix`. When a command's name, arguments or output there
  changes, run `just demo` and commit the new GIFs.
- Never test on a live desktop session (no zone units, windows or
  screenshots there). Use `just test`, `just build` and `just test-vm`.
- Adding a config key or value: bump `RELEASE` in `zonecfg.py`, record it in
  `SINCE` (the package version in nix/package.nix is separate).
- `zone_mount_helper.py` runs as root through sudo. It clears every
  `APPZONES_*` and `ZONES_*` variable before importing anything; keep it that way.
- `main` takes only squash merges through a PR.
- SonarQube Cloud checks every PR. A finding that is not a real problem (a validated
  argument, a test address) gets the reason in a comment on the line above and a bare
  `# NOSONAR` at the end of its own line (`# NOSONAR: why` is not always recognised).
  A bare `# NOSONAR` hides every finding on that line, so keep such lines small. The
  taint findings (`pythonsecurity:*`: command or path injection) still show; a
  maintainer marks those false positive in SonarQube, again when the line changes.
- After a release: `just release-rpm VERSION` builds the RPM from the tag, tests it on Fedora, attaches it;
  the release PR bumps `Version:` in `contrib/fedora/appzones.spec` too.
- Before an AUR update: `just test-arch` (the package built with makepkg from the checkout, then the
  distro test, in an Arch VM). `just test-fedora` builds and tests the RPM on Fedora, `just test-opensuse` tests `make install` on openSUSE.
- A release (`just release VERSION`) attaches the `.deb` (built from the tagged commit:
  `nix build github:mirql/appzones/SHA#deb`), and is followed by the AUR package: in `contrib/arch/`,
  `pkgver`, the tarball's sha256 and `.SRCINFO` (a PR), then the same four files pushed
  to `ssh://aur@aur.archlinux.org/appzones.git` (needs the maintainer's AUR key).
