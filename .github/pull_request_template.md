Closes #

## What and why

<!-- What does this change, and why this way? What did you leave out? The motivation is on the issue; here goes the reasoning that isn't. -->

## Security

<!-- appzones is a sandbox. Answer both, even if the answer is "no". -->

- Does this let a zone see or reach more of the host (files, network,
  D-Bus, Wayland, devices, environment)? If yes, why is it safe?
- Does it touch the mount helper (`mount_helper_linux.rs`), the never-lists,
  the nft rules or the sudo rule?

## Checklist

- [ ] `just test` and `just build` pass (say what ran)
- [ ] `just test-vm` passes (needed for anything beyond the validator)
- [ ] Tests added: unit tests for the validator, VM checks for isolation (asserting what is **refused**)
- [ ] New config key/value: `RELEASE` and `SINCE` bumped
- [ ] The pages in `docs/` and `examples/zones.toml` updated if behaviour or config changed
- [ ] Line added under "Unreleased" in `CHANGELOG.md`
- [ ] Nothing private: no hosts, paths, names or credentials
