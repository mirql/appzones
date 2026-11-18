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
  argument, a test address) gets `# NOSONAR: <why>` on its line, with a reason a reader
  can check. NOSONAR does not silence the taint findings (`pythonsecurity:*`, e.g.
  command or path injection): a maintainer marks those false positive in SonarQube,
  again when the line changes; the comment still documents why.
