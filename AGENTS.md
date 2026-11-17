# Agent notes for appzones

- Code: `src/` (`zone.py` CLI and supervisor, `zone_init.py` the agent
  inside a zone, `zonecfg.py` schema and validator, `zone_mount_helper.py`
  the root step of `appzones mount`, `zone-wayland/` the security-context
  helper). Nix: `nix/package.nix`, `nix/module.nix`, `tests/vm.nix`.
- User docs: `docs/guide.md`. When behaviour or a config key changes, update
  it and `examples/zones.toml`.
- Never test on a live desktop session (no zone units, windows or
  screenshots there). Use `just test`, `just build` and `just test-vm`.
- Adding a config key or value: bump `RELEASE` in `zonecfg.py`, record it in
  `SINCE` (the package version in nix/package.nix is separate).
- `zone_mount_helper.py` runs as root through sudo. It clears every
  `ZONES_*` variable before importing anything; keep it that way.
- `main` takes only squash merges through a PR. Put the co-author trailer in
  the merge commit's message (`gh pr merge --squash --body ...`), or it is
  lost: `Co-authored-by: Saif H <249896858+Saifeldin-H@users.noreply.github.com>`.
