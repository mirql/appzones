## What and why

<!-- What does this change, and why? Link the issue: Fixes #123 -->

## Security

<!-- appzones is a sandbox. Answer both, even if the answer is "no". -->

- Does this let a zone see or reach more of the host (files, network,
  D-Bus, Wayland, devices, environment)? If yes, why is it safe?
- Does it touch `zone_mount_helper.py`, the never-lists, the nft rules or
  the sudo rule?

## Checklist

- [ ] `just test` and `just build` pass
- [ ] `just test-vm` passes (needed for anything beyond the validator)
- [ ] Tests added: unit tests for the validator, VM checks for isolation (asserting what is **refused**)
- [ ] New config key/value: `RELEASE` and `SINCE` bumped
- [ ] `docs/guide.md` and `examples/zones.toml` updated if behaviour or config changed
- [ ] Line added under "Unreleased" in `CHANGELOG.md`
