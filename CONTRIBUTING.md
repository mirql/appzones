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
nix develop          # python3, just, nixfmt
just test            # unit tests (seconds, no Nix)
just check FILE      # validate a zones.toml
just build           # the package; runs the unit tests too
just test-vm         # NixOS VM test with Hyprland (~6 min, needs /dev/kvm)
just test-install    # `make install` into a staging directory; also `make install DESTDIR=/tmp/x` without Nix
just fmt             # format the Nix files
just demo            # re-record docs/demo-*.gif in a VM, when the CLI output changes
just release X.Y.Z   # after the version's PR is merged: tag it, publish its CHANGELOG section
```

**Never test on your live desktop session**: no zone units, windows or
screenshots there while developing. A broken zone, a stale mount or a bad
nft rule should break a VM, not your session. Use the unit tests and the
VM test.

## Layout

| path | what |
|---|---|
| `src/zone.py` | the entry point: usage text and `main()` (`appzones`, `serve`, `_netsetup`) |
| `src/appzones/` | its modules: `client` (the commands), `server` (`appzones serve`, the unit's ExecStart), `fs` (theme, devices, the bwrap command line), `net` (nft, pasta, DNS), `portals` (D-Bus proxy, links), `agent` (talking to zone-init), `mounts`, `upgrade`, `common` (paths and settings; read as `common.X`, never copied) |
| `src/zone_init.py` | the agent inside a zone (PID 2 under bwrap) |
| `src/zonecfg.py` | the `zones.toml` schema, the validator and the never-lists |
| `src/zone_mount_helper.py` | the root step of `appzones mount`, run through sudo |
| `src/zone-wayland/` | the `wp_security_context_v1` helper (C) |
| `nix/package.nix`, `nix/module.nix` | the package and the NixOS module |
| `tests/vm.nix` | the NixOS VM test |
| `tests/demo.nix`, `tests/demo-record.py` | the README's terminal demos, recorded in a VM (`just demo`) |
| `skills/appzones/SKILL.md` | the agent skill for users' agents (keep it in step with the CLI) |
| `assets/concept/concept.py` | the README's concept animation (writes `concept-{dark,light}.svg`) |
| `contrib/hyprland/appzones.lua` | window tags and border colours for Hyprland |
| `docs/index.md`, `docs/_config.yml` | the docs site, mirql.github.io/appzones (GitHub Pages builds `docs/` on main) |
| `docs/**/*.md`, `examples/zones.toml` | user docs (the site's pages; `docs/guide.md` is their contents) and the full example |

## Rules for changes

- **Tests.** Validator and parsing changes get a unit test in
  `src/test_zonecfg.py`. Anything about isolation, the units, D-Bus,
  Wayland or mounts gets a check in `tests/vm.nix`. Isolation checks
  should assert that something is **refused**, not only that the happy
  path works.
- **New config key or value.** Bump `RELEASE` in `src/zonecfg.py`, record
  the key in `SINCE`, and document it in `docs/configuration/reference.md` and
  `examples/zones.toml`.
- **Never-lists** (paths, D-Bus names, environment) only grow by default.
  Removing an entry needs an explicit reason in the PR.
- **`zone_mount_helper.py` runs as root.** Keep it small, keep it
  clearing every `APPZONES_*` and `ZONES_*` variable before it imports anything, and
  explain in the PR why a caller still cannot abuse it.
- **Docs.** If behaviour changes, update its page in `docs/` in the same PR.
  Add a line under "Unreleased" in [CHANGELOG.md](CHANGELOG.md).
- **Style.** Python: standard library only (the package has no Python
  dependencies), type hints where they help. Nix: `just fmt`.

## Pull requests

- One topic per PR; small is better.
- `just test` and `just build` pass. Run `just test-vm` when you touch
  anything beyond the validator.
- Fill in the PR template, especially the security section.

By contributing you agree that your contribution is licensed under the
[MIT License](LICENSE).
