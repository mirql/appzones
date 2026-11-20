# Design: appzones in Rust (experiment)

Status: proposal, on the branch `rust-version`. Nothing here reaches `main` until a step's
exit criteria hold. PRs for this go into `rust-version`; `main` is merged into it regularly
(merge commits), so it never drifts far. Reviewed by two other models before work started;
their points are in this version.

## What and why

appzones is about 4,800 lines of Python (standard library only), 1,700 lines of unit tests
(80 tests) and a 90-line C helper. Speed is not a reason to change: appzones starts
processes and waits.

**Phase 1 (decided)** moves the parts that run inside a zone or as root into Rust:

- **No Python in a zone.** Today zone-init runs with the host's Python inside every zone
  (`fs.bwrap_argv` ends with `sys.executable zone_init.py`), and so does the `xdg-open` shim
  for `links = "zone"`. That is why appzones must be installed under `/usr` or the Nix store
  (`server.install_error`). A static binary bound read-only into the zone removes both.
- **A smaller root step.** The mount helper is the one part that runs as root. A small
  executable with typed syscalls and no interpreter is easier to audit.
- **zone-wayland without C and libwayland.**

**Phases 2 and 3 (open)**: the configuration, then the CLI and the server. They are decided
after phase 1, with its measured cost (build time, binary size, test time, time spent).
Points against doing them now: removing host Python needs only `tomllib` replaced (Python
3.11), which a bundled `tomli` would do; the binary would still need bwrap, pasta, nft, the
sudo rule and the units, so it is never "one file"; the server is process orchestration
(signals, fds, timeouts), where Rust adds `unsafe` more than safety; and contributors find
their way in through small Python tasks.

## Before phase 1, on main

Small PRs into `main`, so both implementations can be tested the same way:

1. **Tests that do not name the implementation.** `tests/vm.nix` and `common.server_command`
   find servers and zone-init by their Python command lines (`zone.py serve`,
   `zone_init.py`). Use marks that both can share (the zone's lock and pidfd, a process
   title, or `/proc/PID/exe` against a known list), keeping the upgrade path from servers
   started by older versions.
2. **The VM test takes the helper as a parameter**, so `vm-python` and `vm-rust` can both
   run (about 6 minutes each); the distro tests the same.
3. **Golden files**: the agent protocol exchanges (JSON lines) of the existing VM test, and
   `fs.bwrap_argv` output for the example configs, recorded from Python. Cheaper than a VM
   run for comparing a port.
4. **Check `op_shell` in zone-init**: it forks a pty after the DNS threads have started.
   Python mostly copes, but it is the kind of thing a port must not copy blindly.

## Phase 1a: what runs in a zone, and zone-wayland

`appzones-helper`, one static binary (musl), with no root and no policy code:

```
appzones-helper zone-init          # in the zone (pid 2, after bwrap)
appzones-helper open-link URL      # in the zone: the xdg-open shim's http(s) part (links.sock)
appzones-helper zone-wayland SOCKET APP_ID INSTANCE
```

- **Into the zone**: the server binds the binary read-only into the zone (under
  `/run/zone/bin`, by descriptor as `pin_binds` does), and starts zone-init from there.
  Then appzones' own files no longer need to be visible in the zone.
- **Interfaces stay**: the agent protocol (see the top of `zone_init.py`), what the server
  passes (`ZONE_CTL_FD`, `ZONE_SERVER_FD`, `ZONE_DNS`, ...), zone-wayland's "ready" line and
  its context lifetime (the close-pipe writer lives as long as the process).
- **The switch** is a server setting chosen at install time (package or module), not an
  environment variable: systemd units, `sudo` and the tests' `env -i` do not pass one on.
- **The install-location rule** changes only with the Rust helper: `server.install_error`
  (and the Makefile's PREFIX check) keep requiring `/usr` or the Nix store for the Python
  zone-init, and drop it when the Rust helper is installed, since then only the bound
  binary must reach the zone. A test installs under `/opt` with the Rust helper (zones
  start) and without it (refused, as today). This never applies to the root step: the
  mount helper named in the sudo rule must sit where only root can change it. `make
  install` writes the sudo rule only when the helper and every directory above it are
  owned by root and writable by no one else (it refuses otherwise, as it refuses a PREFIX
  outside `/usr` today), and a test installs under a user-writable directory and checks
  that no sudo rule is written.

Order: skeleton (Cargo, a reproducible static Nix build, `make` where `cargo` is present,
the first numbers: build time and stripped size); zone-wayland; zone-init op by op (`ping`,
`ps`, `run` with fds and `--wait`, `status`, `shell`, `resize`, `mounts`, `busy`, the zone
bus, DNS, the server pidfd watch, reaping); `open-link`.

Traps the review named, each with a test:

- received descriptors with `MSG_CMSG_CLOEXEC` and owned (`OwnedFd`), closed on every error
  path, so a `--wait` client's pipes never leak into later apps; requests split across
  reads;
- the pty: `setsid`, `TIOCSCTTY`, resize, draining to EIO, SIGHUP on close, the host
  terminal restored;
- signals and reaping: `waitpid` also reaps the zone bus; exit at once when the server's
  pidfd fires, without blocking on output; `harden_self` first;
- DNS: id matching, TC then TCP, EDNS size, truncation, the budget, the total deadline.

Done when: `vm-rust` and the distro tests pass with zones that contain no Python of
appzones', a link opens from a `links = "zone"` zone without Python, and an install
outside `/usr` works with the Rust helper.

## Phase 1b: the policy core and the root helper

- **The never-list in Rust first**: `denied_paths`, `host_denied_paths`, `policy_error`,
  `mount_source_error`, `kind_error`, the name rules. The Python `zonecfg` calls it for
  these checks (a small command), so there is one implementation, not a copy in the root
  step. Parity: Python's `os.path.realpath` is non-strict (a home that does not exist yet);
  `fnmatch`'s `*` matches `/` and `.`, unlike Rust glob crates.
- **The mount helper as its own executable**, `appzones-mount-helper`, not a command of
  `appzones-helper`: the sudo rule allows its file with any arguments, so nothing else may
  be reachable through it. `#![forbid(unsafe_code)]` except the syscall layer, the
  environment ignored except `SUDO_UID`, linked against glibc (a static musl binary has no
  NSS: `getpwuid` and `getgrouplist` would break for LDAP or sssd users). Same checks and
  messages as today (SUDO_UID only, opened as the caller with its groups, `openat2` without
  symlinks, `open_tree`, `mount_setattr` on the detached tree, `move_mount`, unmount by
  descriptor); the VM test's refusal checks stay as they are.

Done when: `vm-rust` passes with the Rust mount helper, its tests cover at least the
Python helper's, and a separate security review of it is written down.

Phase 1 can come back to `main` on its own.

## Phases 2 and 3, if they come

- **Phase 2, the configuration**: the rest of `zonecfg` (keys and origins, `[defaults]`,
  `RELEASE`/`SINCE`), with the 80 unit tests as the specification (same results; messages
  close, not byte for byte: TOML parse errors and the "did you mean" suggestions come from
  different libraries). A differential test runs both checks on every example and test
  case. From then on a config key on `main` lands in Rust in the same week.
- **Phase 3, the CLI and the server**: in three parts (client commands; launching:
  `_netsetup`'s fd layout, bind pinning, the pidfd handoff; the server's lifecycle: lock,
  crash containment, linger, the D-Bus proxy). `settings_hash` and `which` must produce
  Python's exact JSON (or every running zone looks stale after the upgrade), and a Rust
  client must still find and stop a server started by a Python version.

## Main while the branch grows

- On `main`: bug fixes, the contributors' good first issues, docs, and the preparation
  above. They reach the branch with the next merge from `main`.
- New features go to `main` as usual; the branch takes them with the merge. The branch adds
  no features of its own.
- A finished phase comes back to `main` (phase 1 first), so the branch never carries more
  than one phase of difference.

## Dependencies

Proposed, each pinned in `Cargo.lock`, with a reason in the PR that adds it:

- `rustix` (features `fs`, `mount`, `net`, `process`, `pty`): the syscalls;
- `serde`, `serde_json`: the agent protocol (with size limits);
- `wayland-client` with its pure-Rust backend, `wayland-protocols` (staging,
  `wp_security_context_v1`);
- `mirql-love-sparkle`: mirql's rule for every manifest (`Cargo.toml` included);
- later, if phase 2 comes: `toml` (as an untyped `Table`, to keep each key's origin and
  tell `true` from `1`); phase 3 would need SHA-256 and IP prefixes.

No async runtime. The command line stays hand-parsed (`--help`, typo messages and `--`
handling as today). Release profile: `strip`, `lto`, `panic = "abort"`; `cargo deny` for the
dependencies. aarch64 is not solved by Rust (x86_64 in nix-ld paths, `flatpak_info`,
`platforms`): it stays its own issue (#21).

## Measured

| Step | Build (Nix, warm toolchain) | Binary | Notes |
|---|---|---|---|
| Skeleton (no code yet, one dependency) | about 45 s with the first toolchain download | 393 KiB, static-pie (musl), stripped; no runtime dependencies | `cargo test` runs in the Nix build; `make` builds it where cargo is present |
| zone-wayland (about 120 lines, replacing 89 lines of C) | about 25 s for the crate (cargo, release, LTO) | 513 KiB static; no libwayland at build or run time | new crates: wayland-client, -protocols, rustix, libc (and, through them, wayland-backend, bitflags, smallvec, downcast-rs, linux-raw-sys; at build time only: wayland-scanner, quick-xml, proc-macro2, quote); `vm-rust`: the 159 checks pass, as `vm` |
| zone-init (about 1,000 lines with DNS and tests, replacing 770 lines of Python) | about 40 s for the crate | 785 KiB static | new crate: serde_json; `AgentProtocolTest` (7) passes against it (flake check `agent-protocol-rust`); `vm-rust`: the 159 checks pass with zone-init in Rust in every zone (shell, zone bus, `bus = "none"`, DNS with TCP fallback, crash containment, linger, busy umount); no Python of appzones' runs in a zone, except the `xdg-open` shim for `links = "zone"` and `"zone:NAME"` |
| open-link (the shim's link part), and the install outside `/usr` | seconds | 785 KiB static (all three commands) | `vm-rust` 162 (the link round trip, no Python in the shim); `debian12-rust`, `ubuntu2404-rust`: installed under `/opt/appzones`, no Python in any zone, `appzones mount` through a sudo rule (written because `/opt/appzones` is root's alone). Phase 1a done |

## When to stop

After each step this file records what it cost (build time, stripped binary size, test
time, time spent) and what it gave. If a step costs more than it gains, the branch stops
there, and only finished steps come back to `main`.
