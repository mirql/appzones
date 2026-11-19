# Design: appzones in Rust (experiment)

Status: proposal, on the branch `rust-version`. Nothing here reaches `main` until a phase's
exit criteria hold. PRs for this go into `rust-version`; `main` is merged into it regularly
(merge commits), so it never drifts far.

## Goal

appzones as one static Rust binary: no Python on the host or in a zone, one download for
any x86_64 or aarch64 Linux. Today it is about 4,800 lines of Python (standard library
only), 1,700 lines of unit tests (80 tests) and a 90-line C helper.

Speed is not the reason: appzones starts processes and waits. The reasons are what a single
binary can do, and what types and a small root step make easier to check:

- **No Python anywhere.** The CLI and server need Python 3.11 on the host (`tomllib`), and
  every zone sees the host's Python and appzones' own files, which is why appzones must be
  installed under `/usr` or the Nix store. A static binary removes both: Ubuntu 22.04,
  minimal servers and containers become possible, and a release can be one file (a GitHub
  download, an AUR `-bin` package, a .deb).
- **The security policy in types.** `zonecfg` (the never-list, binds, keys, the
  validator) and the mount helper (the one root step) are where a mistake matters most.
- **One language** instead of Python and a C helper.

## Phases

Each phase ends with every existing check passing: the NixOS VM test (159 checks), `just
test-distros` (Ubuntu 24.04, Debian 12), the install check. These drive the real `appzones`
command and do not care what it is written in, so they are the acceptance test for each
step. Each phase is useful on its own: the branch can stop after any of them.

### Phase 1: the helper binary

`appzones-helper`, one static binary (musl), for the parts that run inside a zone or as
root:

```
appzones-helper zone-init          # in the zone (pid 2, after bwrap)
appzones-helper open-link URL      # in the zone: the xdg-open shim's http(s) part (links.sock)
appzones-helper zone-wayland SOCKET APP_ID INSTANCE
appzones-helper mount-helper ...   # through sudo, root
```

Their interfaces stay as they are: the agent protocol (JSON lines and SCM_RIGHTS on the
control socket, see the top of `zone_init.py`), what the server passes (`ZONE_CTL_FD`,
`ZONE_SERVER_FD`, `ZONE_DNS`, ...), the mount helper's command line and checks,
zone-wayland's "ready" line. The Python CLI and server use the binary when
`APPZONES_HELPER=rust`, so both run side by side and the VM test runs both.

1. Skeleton: `Cargo.toml`, `Cargo.lock`, a Nix build of the static binary, `make` building
   it where `cargo` is present.
2. zone-wayland (the smallest part, host side; a pure-Rust Wayland client needs no
   libwayland).
3. zone-init, op by op: `ping`, `ps`, `run` (with `--wait` and fds), `status`, `shell`
   (pty), `resize`, `mounts`, `busy`, the zone bus, DNS, the server pidfd watch, reaping;
   and `open-link` (the VM test opens a link from a zone with `links = "zone"` without
   Python in the zone).
4. The mount helper, with the same checks (SUDO_UID only, the never-list on the opened
   directory, `openat2` without symlinks, `open_tree`/`move_mount`, `mount_setattr` for
   read-only).

Done when: a zone needs nothing of appzones but the binary (no Python in the zone, also for
links), and the mount helper's Rust tests cover at least the Python helper's.

This phase could come back to `main` on its own: it is worth having even if the rest stops.

### Phase 2: the configuration

`zonecfg` in Rust: reading `zones.toml`, `[defaults]`, the keys and their checks, the
never-list and the host's `denied-paths`, `requires`/`RELEASE`/`SINCE`, the error messages.
The binary gets `appzones check` (and `appzones-helper` uses the same code for the mount
helper's never-list, instead of a copy).

The 80 Python unit tests are the specification: each becomes a Rust test with the same
input and the same expected error text, so users see the same messages.

Done when: every `examples/*.toml` and every test case gives the same result and message in
both; the Python CLI can call the Rust check (`APPZONES_CONFIG_CHECK=rust`) and the VM test
passes with it.

### Phase 3: the CLI and the server

The rest: `run`, `shell`, `up`/`stop`/`restart`, `list`, `which`, `logs`, `mount`,
`active`, `new`, `apps`, autostart, `serve` (the zone's server: lock, crash containment,
the D-Bus proxy, DNS, linger), the supervisor modes, the launcher entries. Command names,
flags, output (also `--json`), exit codes and files stay the same.

Done when: the VM test, the distro tests and the Arch test pass with the Rust `appzones`
alone, and Python is gone from the repo. Then a release, as one binary.

## Main while the branch grows

The real cost of a long branch is keeping up with `main`. To keep that small:

- On `main`: bug fixes, the contributors' good first issues, docs. They are ported to the
  branch with the next merge from `main` (the PR that merges says what it ported).
- Larger features wait, or are built on the branch.
- Every phase that is done can come back to `main` (phase 1 first), so the branch never
  carries more than one phase of difference.

## Dependencies

Proposed, each pinned in `Cargo.lock`, with a reason in the PR that adds it:

- phase 1: `rustix` (the syscalls: pidfd, `openat2`, the mount API, pty, `SCM_RIGHTS`),
  `serde` and `serde_json` (the agent protocol), `wayland-client` with its pure-Rust
  backend and `wayland-protocols` (`wp_security_context_v1`);
- phase 2: `toml` (the TOML reader);
- phase 3: none planned (the command line is parsed by hand, as today);
- `mirql-love-sparkle`: mirql's rule for every manifest.

No async runtime: zone-init and the server stay poll loops, as today.

## When to stop

After each phase, this file records what it cost (build, binary size, test time, time
spent) and what it gave. If a phase shows Rust costs more than it gains, the branch stops
there, and only the finished phases come back to `main`.

## Not here

No new features on the branch beyond what `main` has. The agent protocol, the config
format, the commands and the files stay as they are: users of `main` and of the branch see
the same appzones.
