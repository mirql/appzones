# Contributing to appzones

Thanks for helping. appzones is a sandbox, so most changes are judged first
by one question: does this make a zone easier to escape, or let it see more
of the host? Changes that widen what a zone can reach need a reason and a
test.

We try to be kind, humble and curious here, and ask the same of everyone who
joins in.

## Before you start

- **New here?** Pick one of the [good first issues](https://github.com/mirql/appzones/labels/good%20first%20issue):
  each says where in the code to start and what done looks like, and most
  can be tested without NixOS. Say on the issue that you are on it, so no
  one else starts the same. Bigger tasks are under [help wanted](https://github.com/mirql/appzones/labels/help%20wanted).

- **Bugs:** open an issue with the bug template. Include `appzones check`
  output and the relevant part of your `zones.toml`.
- **Features and new config keys:** open an issue first, so we can agree
  on the shape before you write code.
- **Security problems** (an escape, a never-list bypass, a hole in the
  mount helper or the D-Bus filter): **do not open an issue.** See
  [SECURITY.md](SECURITY.md).

## Development

```bash
nix develop          # cargo, rustc, clippy, rustfmt, just, nixfmt
just test            # cargo test and clippy (no Nix: any Rust 1.85 or newer; the first build takes minutes)
just check FILE      # validate a zones.toml
just build           # the package; its build runs every Rust test, the golden files' too
just test-vm         # NixOS VM test with Hyprland (~6 min, needs /dev/kvm)
just test-install    # `make install` into a staging directory; also `make install DESTDIR=/tmp/x` without Nix
just test-distros    # Ubuntu 24.04 and Debian 12 in a VM (~3 min each, /dev/kvm): run it when you touch the
                     # install, the zone's file system or the network
just fmt             # format the Nix files
just demo            # re-record docs/demo-*.gif in a VM, when the CLI output changes
just release X.Y.Z   # after the version's PR is merged: tag it, publish its CHANGELOG section
```

**Never test on your live desktop session**: no zone units, windows or
screenshots there while developing. A broken zone, a stale mount or a bad
nft rule should break a VM, not your session. Use the unit tests and the
VM test.

`just test` runs the golden tests only in the Nix build (`just build`): their answers
about system paths were recorded there, and your machine's `/etc` or `/boot` differ.

## Layout

| path | what |
|---|---|
| `helper/src/main.rs` | the binary's subcommands: `cli` (what `appzones` runs), `zone-init`, `zone-wayland`, `open-link`, `serve`, `plan` |
| `helper/src/cli.rs` | the commands (`appzones run`, `up`, `list`, ...), starting and stopping servers, `--help` (`help.txt`) |
| `helper/src/project.rs` | project zones: a repository's `appzones.toml`, `appzones trust` |
| `helper/src/serve.rs`, `plan.rs` | the server (`appzones serve`, the unit's ExecStart): the zone's plan (theme, devices, nft rules, the bwrap command line), then running it |
| `helper/src/netsetup.rs`, `host_dns.rs` | the zone's network namespace step (`_netsetup`) and its DNS on the host side |
| `helper/src/zone_init.rs`, `zone_dns.rs`, `open_link.rs` | inside a zone: the agent (PID 2 under bwrap), its DNS, the link opener |
| `helper/src/zone_wayland.rs` | the `wp_security_context_v1` helper |
| `helper/src/config.rs`, `policy.rs` | the `zones.toml` schema, the validator and the never-lists |
| `helper/src/mount_helper.rs`, `mount_helper_linux.rs` | the root step of `appzones mount`, its own binary, run through sudo (the code is in `mount_helper_linux.rs`) |
| `helper/src/agent.rs`, `active.rs`, `pyfmt.rs` | talking to zone-init; `appzones active` and the sway hook; the formats (repr, JSON) that outputs keep from the Python implementation |
| `helper/src/sys.rs` | the system calls Linux and macOS spell differently |
| `helper/src/seatbelt.rs`, `proxy.rs` | macOS: a zone's Seatbelt profile; the proxy for `network = "internet"` |
| `helper/tests/` | `cli.rs` (the commands from outside), `agent.rs` (the agent protocol), `golden.rs` with `golden/` (what the Python implementation answered, kept as recorded answers) |
| `tests/macos.sh` | zones on a Mac (`just test-macos-zones`) |
| `nix/package.nix`, `nix/module.nix` | the package and the NixOS module |
| `nix/package-darwin.nix`, `nix/darwin-module.nix` | the macOS package and the nix-darwin module |
| `nix/helper.nix`, `nix/portable.nix`, `nix/deb.nix` | the binary (tests in its build); the binaries for other distributions; the .deb |
| `tests/vm.nix` | the NixOS VM test |
| `tests/demo.nix`, `tests/demo-record.py` | the README's terminal demos, recorded in a VM (`just demo`) |
| `skills/appzones/SKILL.md` | the agent skill for users' agents (keep it in step with the CLI) |
| `assets/concept/concept.py` | the README's concept animation (writes `concept-{dark,light}.svg`) |
| `contrib/hyprland/appzones.lua` | window tags and border colours for Hyprland |
| `docs/index.md`, `docs/_config.yml` | the docs site, mirql.github.io/appzones (GitHub Pages builds `docs/` on main) |
| `docs/**/*.md`, `examples/zones.toml` | user docs (the site's pages; `docs/guide.md` is their contents) and the full example |

## Rules for changes

- **Tests.** Validator and parsing changes get a unit test next to the code in
  `helper/src/`; a command's output, a test in `helper/tests/cli.rs`. Anything about isolation, the units, D-Bus,
  Wayland or mounts gets a check in `tests/vm.nix`. Isolation checks
  should assert that something is **refused**, not only that the happy
  path works.
- **New config key or value.** Bump `RELEASE` in `helper/src/config.rs`, record
  the key in `SINCE`, and document it in `docs/configuration/reference.md` and
  `examples/zones.toml`.
- **Never-lists** (paths, D-Bus names, environment) only grow by default.
  Removing an entry needs an explicit reason in the PR.
- **The mount helper (`mount_helper_linux.rs`) runs as root.** Keep it small, reading no
  environment but `SUDO_UID`, with `unsafe` only in its `mod sys`, and explain in the PR why a caller still cannot abuse it.
- **Golden files** (`helper/tests/golden/`) hold what the Python implementation
  answered. Change an answer only on purpose, in the same PR, and say why.
- **Docs.** If behaviour changes, update its page in `docs/` in the same PR.
  Add a line under "Unreleased" in [CHANGELOG.md](CHANGELOG.md).
- **Style.** Rust: `cargo clippy -- -D warnings` clean, every dependency pinned
  (`=x.y.z`) with a comment saying why, `unsafe` only with a `// Safety:` line. Nix: `just fmt`.

## Pull requests

- One topic per PR; small is better.
- `just test` and `just build` pass. Run `just test-vm` when you touch
  anything beyond the validator.
- Fill in the PR template, especially the security section.

By contributing you agree that your contribution is licensed under the
[MIT License](LICENSE).
