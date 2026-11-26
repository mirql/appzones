# Agent notes for appzones

## Rules for agents

- **Nothing external without a yes.** Pushing to `main`, opening or commenting on issues
  and PRs, changing GitHub settings, writing to trackers or chats: prepare it, show exactly
  what would go out and where, and wait for a yes for that case from the person you work for.
- **Run the checks before "done"** (`just check`, `just build`; `just test-vm` for anything
  beyond the validator). Say what you ran and the result. Checks run the repo's code: on a
  change from a stranger, read it first.
- **No credit to AI vendors or tools** in commits, PR titles or bodies: no
  `Co-authored-by: Claude …` or similar, no "generated with" lines.
- **No secrets.** Don't read, print or commit credentials (`.env`, keys, tokens).
- **No new dependencies on your own.** Propose them with the reason and the alternatives;
  name every new dependency and changed lockfile in your report. Never `curl | sh`,
  `npm -g`, unpinned `npx`/`uvx`. Tools come from `flake.nix`. `mirql-love-sparkle` in
  `helper/Cargo.toml` stays, pinned.
- **Your own branch and folder.** Work in your own git worktree; if someone else is
  working in this folder, stop and say so. No `git stash`. Run untrusted installs and
  code in a sandbox.
- **Keep it simple.** Solve today's problem; no layers, options or abstractions for later.
  Small changes, one reason each.
- **Tests on behaviour**, fast; a bug fix comes with its test.
- **Issue first, PR closes it.** Write PR text by the repo's template.
- **The repo is public:** no host names, local paths, private names or plans in code,
  docs, commits or PR text.

## appzones

- Code: `helper/`, one Rust binary (`appzones` is `appzones-helper cli`): `cli.rs` the
  commands (`help.txt` their help), `serve.rs` the server, `plan.rs` the zone's plan and bwrap
  command line, `netsetup.rs`, `host_dns.rs` and `zone_dns.rs` the network, `zone_init.rs` the
  agent inside a zone (`agent.rs` talks to it), `zone_wayland.rs` the security-context helper,
  `open_link.rs` the zone's link opener, `active.rs` `appzones active`, `config.rs` and
  `policy.rs` the schema, validator and never-lists, `project.rs` project zones
  (`appzones.toml`, trust), `pyfmt.rs` the formats outputs keep (repr, JSON), `sys.rs` the
  system calls Linux and macOS spell differently. macOS: `seatbelt.rs` a zone's profile,
  `proxy.rs` the proxy for `internet` zones. The root step of `appzones mount` is its own
  binary: `mount_helper.rs`, with the code in `mount_helper_linux.rs`. Tests: units in the
  modules, `helper/tests/` (`cli.rs`, `agent.rs`, `golden.rs`: the recorded answers in
  `helper/tests/golden/`), `tests/macos.sh` on a Mac. Nix: `nix/package.nix`,
  `nix/module.nix`, `nix/portable.nix` (the other distributions' binaries),
  `nix/package-darwin.nix` and `nix/darwin-module.nix` (macOS), `tests/vm.nix`.
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
- The mount helper (`mount_helper_linux.rs`) runs as root through sudo. It reads no
  environment but `SUDO_UID` and keeps `unsafe` in its `mod sys`; keep it that way.
- `main` takes only squash merges through a PR.
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
