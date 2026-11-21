# Design: appzones in Rust (experiment)

Status: phase 1 done and back on `main`, as an option chosen at install time (Python stays
the default); phases 2 and 3 not started (see "Phase 1: the result"). Work went on the
branch `rust-version`, with `main` merged into it regularly (merge commits). Reviewed by two
other models before work started; their points are in this version.

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
  `mount_source_error`, `kind_error`, the name rules (`helper/src/policy.rs`). Changed while
  building it: the Python `zonecfg` keeps its own copy for binds and `appzones check` (they
  should not need the helper), and a differential test (`DifferentialPolicyTest`, the
  flake check `policy-rust`) compares both never-lists entry by entry (an entry added on
  one side only fails it) and both answers, with their messages, for the same paths. Where
  it cannot be sure (a symlink loop, a directory it cannot list, a NUL byte in the host's
  file) the Rust one refuses; Python would fail with an error there too.
  The root step uses only the Rust one. Parity: Python's `os.path.realpath` is non-strict (a home that does not exist yet);
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

## Phases 2 and 3 (see the research below, and the decision after it)

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
| the mount helper (its own executable) | about 10 s | 341 KiB, glibc (NSS), only `libc` as a dependency | `vm-rust` 163 with every runtime-mount check (refusals through symlinks, as the caller, bad names, without sudo; read-only also from a nested user namespace); `debian12-rust`, `ubuntu2404-rust` mount through the sudo rule with it |
| stage 1: the server (`appzones-helper serve`, the runner of a zone's plan; about 900 lines with the host side of the DNS) | about 30 s for the crate | 785 KiB static helper, `serve` in it | the server holding a zone: **about 930 KB RSS, against 26 MB in Python**; `vm-rust` 163, every distro check, `test-fedora`, `test-arch`, `test-opensuse` |
| stage 2: the configuration (`config.rs`, `pyfmt.rs`) and the plan (`plan.rs`), 2,732 lines | about 40 s for the crate | 1.3 MB static helper (785 KiB before; toml 0.8 and the plan) | the server, now with the plan made in Rust too, about 1.3 MB RSS; `DifferentialConfigTest` 944 configurations, `DifferentialPlanTest` 12 cases (both layouts, either zone-init), all identical; `vm-rust` 163, every distro check, `test-fedora`, `test-arch`, `test-opensuse` |
| stage 3: the commands (`cli.rs`, `agent.rs`, `netsetup.rs`, `active.rs`), 2,240 lines | about 40 s for the crate | the same static helper; `appzones` is `appzones-helper cli` | with the Rust helper no Python runs at all; the commands that need no zone give Python's output byte for byte (help, check, which, list, new, errors); `vm-rust` 163, `vm` 162, every distro check (`appzones` the Rust command line on the Rust ones), `test-fedora`, `test-arch`, `test-opensuse` |
| stage 4: one binary everywhere, the Python removed (8,144 lines of Python, 89 of C); its 49 unit tests that tested internals ported (units in the modules, `tests/cli.rs`, `tests/agent.rs`: 1,048 lines of tests outside the modules), and its answers frozen (`tests/golden`: 937 configurations, 6 plans, 169 paths) | about a minute for the crate, with every test | `appzones-helper` 1.5 MB (static, Nix) or 1.4 MB (glibc 2.17, other distributions: `nix/portable.nix`, built with zig); `appzones-mount-helper` 371 KB | no Python and no C anywhere; the server 1.5 MB RSS static, about 3 MB with glibc; `vm` 163, `debian12`, `ubuntu2404` (`make install` with the portable binaries), both `-deb`, `test-fedora` (RPM built with Fedora's cargo), `test-arch` (AUR package, Arch's cargo), `test-opensuse` (`make install`, openSUSE's cargo) |

## Stage 4: one binary (what changed besides the language)

- **Packages.** Nix builds the binary static (musl) and the mount helper against glibc
  (NSS). The other distributions get `nix/portable.nix`: both binaries linked against glibc
  2.17's symbols with zig, so one build runs everywhere and the mount helper uses the
  system's NSS (a static musl one would not find LDAP or systemd-homed users). The .deb is
  packed from them with nixpkgs' dpkg; no Debian image any more. The RPM and the AUR
  package build from source with the distribution's cargo.
- **Where the binaries go.** `make install` puts them in `PREFIX/libexec/appzones`, not
  `lib/`: on Fedora pasta runs in its own SELinux domain and may execute only `bin_t`
  files (`/usr/libexec` is; `/usr/lib` is `lib_t`), without an audit message. Python never
  met this, since pasta ran `/usr/bin/python3`. The .deb and the AUR package keep
  `/usr/lib/appzones` (no SELinux there, and their sudo rule names it); the RPM uses
  `%{_libexecdir}`.
- **No install rule.** A zone gets a read-only copy of the binary, so appzones may be
  installed anywhere (`PREFIX=/opt/...`); the rule that it live under `/usr` is gone.
- **Python's tests.** Its answers are the golden files, captured in a Nix build (the same
  file system the Rust test runs in), each with the file tree it saw. Its unit tests of
  internals are Rust tests of the same behaviour; what only made sense in Python (which
  zone.py runs, the Python link shim) went with it.
- **Found on the way.** A zone lock held by a server from this version did not keep the
  pre-0.6 state directory from moving (only Python servers were seen); now any held lock does.

## Phase 1: the result

**What it cost.** About 2,650 lines of Rust (tests included) for 1,040 lines of Python and
89 of C; two binaries, `appzones-helper` (785 KiB, static) and `appzones-mount-helper`
(341 KiB, glibc); about a minute of build in Nix; a Rust toolchain only for those who choose
it; six direct dependencies, all pinned (`just audit`: no advisories, permissive licenses
only). More test time: the VM and distro tests run twice (`just test-rust`, about 15 min).
Two implementations of the never-list, kept equal by a differential test.

**What it gave.**

- No Python of the host's in any zone: a zone runs only the helper, bound read-only.
  appzones with it can be installed outside `/usr`.
- The root step is a small binary: no interpreter, no import path, no environment but
  `SUDO_UID`, every syscall in one module.
- zone-wayland without libwayland or C.
- Tests that hold for any implementation (`AgentProtocolTest`, the differential never-list
  test, VM checks that name no Python file), and three bugs found and fixed on `main`: an
  argv given as a string (#102), a mounted folder's copy that was not private (#112),
  names with a trailing newline (#112).

**Phases 2 and 3: not now.** Phase 2 would put every config key in two languages (or
replace `zonecfg`, which `appzones check` and the unit tests use without a build); phase 3
is the largest and riskiest part (`settings_hash`, the handoff to running servers), and
speed was never the reason. Neither removes Python from the host, which is what would make
them worth it. Look again when one of these holds: Python itself becomes the obstacle on a
distribution, a single static binary is asked for, or the Rust helpers have been the
default for a release without trouble.

**Next.** The Rust helpers stay optional (`appzones-rust`, `make install HELPER=rust`). After
a release with them, make them the default where cargo is present, if nothing came up.

## Phases 2 and 3: research (2026-11-20)

Measured on Arch (appzones 0.11, one zone with `network = "internet"`, started without
systemd; RSS and PSS from `/proc`):

| Per running zone | Python | Rust |
|---|---|---|
| the server (`appzones serve`) | 26 MB RSS, about 20 MB PSS | (not ported) |
| zone-init | 16.8 MB RSS, 11.4 MB PSS | 0.9 MB |

| One CLI call | time |
|---|---|
| `python3 -c pass` | 20 ms |
| `appzones --version`, `list`, `check` | about 180 ms each |
| of which imports (`python3 -X importtime`): `zonecfg` 43 ms, `appzones.client` 51 ms | about 110 ms |
| `appzones-helper --commands` (Rust) | 1 ms |

What that means:

- **Memory** is the clear win, and it is per running zone. Phase 1 already takes zone-init
  from 11 MB to 1 MB with the Rust helpers (#121 makes them the default). What is left is the
  Python server, about 20 MB per zone: 160 MB for eight zones.
- **CLI latency**: about 180 ms. About 110 ms of it is imports, which even `--version` pays.
  `zonecfg` (43 ms) loads for every command, so lazy imports in Python would remove at most
  the other 67 ms, leaving about 110 ms. Rust would make it a few ms, which a launcher
  click would feel, but not much more.
- **Reach**: Python 3.11 rules out Ubuntu 22.04, Debian 11, RHEL 9's default Python and Leap
  15. But the first two have no passt either, and appzones needs pasta. A static binary
  alone would not open them.
- **Not a gain**: speed of a zone's start (bwrap, pasta and the helpers dominate), and
  security of the host side (it runs as the user, the root step is Rust already).

Three ways on from here:

- **A. Phase 2 first (the configuration)**: two implementations of every key (or `zonecfg`
  replaced, which `appzones check` and 100 unit tests use without a build), a differential
  test for all of it. Nothing a user notices until phase 3 comes too.
- **B. The long-running server only.** `appzones serve` stays a short Python entry point. It
  loads and checks the configuration as today, works out what a zone needs (bwrap, pasta and
  proxy command lines, nft rules, binds to pin, the D-Bus names), writes that plan to the
  zone's runtime directory, and `exec`s a Rust server. The Python process is gone, so its
  memory is gone too. The Rust server holds the zone: the lock, the helpers and their
  readiness, the pidfd handoff, the log and DNS relays, link handling, linger, stop and
  cleanup. No second implementation of the configuration; the plan file is the interface,
  tested like the agent protocol. About 1,000 lines of Rust (as zone-init was), and what
  stays Python is the part that needs no memory once it ends.
- **C. Everything (phases 2 and 3 as first planned)**: one static binary and no Python on the
  host. About 4,200 lines of Python to port plus their tests, and the most risk:
  `settings_hash` and `which` byte for byte, the handoff to servers a Python version
  started, messages and suggestions from other libraries.

**Recommendation**: B, together with lazy imports in the Python CLI (cheap, Python only;
about 180 ms down to about 110 ms).
C only if Python itself becomes the problem: a distribution without a usable Python 3.11,
or a request for a single binary. Before B: a spike that writes the plan for one zone and
runs it with a minimal Rust server (no DNS, no links), measuring its memory and how much of
`server.py` moves.

## Everything in Rust (decided 2026-11-20, on rust-version)

The maintainer chose option C above as the branch's goal: all of appzones in Rust, on
`rust-version`, while `main` stays Python. It comes back to `main` only when it passes
everything `main` passes.

**The end state**: one Rust binary, `appzones` (the CLI, the server, zone-init,
zone-wayland, open-link), static where it can be, plus `appzones-mount-helper` on its own
(glibc, for NSS; unchanged). No Python on the host.

**What must not change** (the contract; each is tested from outside already or gets a test):
- `zones.toml`: keys, values, defaults, `requires`/`RELEASE`, and what `appzones check`
  refuses and warns about (messages close, not byte for byte);
- the commands, their options, outputs (`--json` ones exactly) and exit statuses;
- the runtime layout: `$XDG_RUNTIME_DIR/appzones/NAME.lock` and the zone's runtime
  directory, logs in `$XDG_STATE_HOME/appzones/`, the colours file, the launcher entries;
- `appzones serve NAME [--lock-fd L] [--notify-fd W]` and the systemd units;
- the agent protocol (zone-init: both exist already, `AgentProtocolTest`);
- `settings_hash` byte for byte while servers of the other implementation may be running
  (or a running zone looks stale);
- the mount helper and its sudo rule.

**Stages**, each done when its tests pass; numbers into "Measured" after each:

1. **The server.** `appzones serve` (Python) loads and checks the configuration, works out
   the zone's plan (command lines, rules, binds to pin, environment, timeouts) into a JSON
   file in the zone's runtime directory, and `exec`s `appzones-helper serve --plan FILE`,
   keeping the lock and notify descriptors. The Rust server holds the zone: helpers and
   their readiness, `_netsetup`, the pidfd handoff, the log and DNS relays, links, linger,
   autostart, stop and cleanup. Done: `vm-rust` and the distro tests with it.
   In two steps:
   - **1a (Python only)**: `supervise` split into making the plan and running it, with the
     plan as JSON; Python still runs it, so `vm` shows nothing changed, and the plan format
     is the tested interface. Parts that depend on what the helpers produce (the zone's
     Wayland socket, the host's as a fallback, the bus) are marked in bwrap's command line,
     and the runner keeps or drops them. `_netsetup` stays Python in this stage: it execs
     bwrap at once and holds no memory.
   - **1b**: the Rust runner (`appzones-helper serve --plan FILE`): helpers and their
     readiness (zone-wayland's exit 3 and the host fallback), the D-Bus proxy, the pidfd
     handoff, zone-init's answer, readiness, autostart, the log and DNS relays (`dns.py`),
     links, linger, a dead helper, stop, cleanup, the exit statuses. With the Rust helper
     installed, `appzones serve` writes the plan and execs it. About 800 to 1,000 lines.
2. **The configuration.** `zonecfg` in Rust (TOML with each key's origin, `[defaults]`,
   `RELEASE`/`SINCE`, the never-list from phase 1b), and the plan made in Rust. A
   differential test runs both on every example and unit-test case (results, warnings,
   `settings_hash`) until they agree.
3. **The commands.** The CLI in Rust, with the same outputs; Python's `zone.py` and
   `appzones/` removed from the branch. Done: the whole suite, and `appzones check` on the
   maintainer's real `zones.toml` agrees with Python's.
4. **Packaging.** One binary in the Nix package, the .deb, the RPM and the AUR; the Python
   dependency dropped. Then the decision whether it replaces Python on `main`.

**Tests in two tiers.** Every PR: `cargo test` and clippy, the Python unit tests while
Python is there, the protocol and differential tests (minutes). The end of each stage: the
whole suite (`vm`, `vm-rust`, the distro tests, `just test-arch`, `test-fedora`,
`test-opensuse`), once.

**`main` meanwhile**: bug fixes and small things only, no large features, so the branch
does not chase a moving target. Each fix reaches the branch with the next sync, and is
ported there if the code it touches is Rust already.

## When to stop

After each step this file records what it cost (build time, stripped binary size, test
time, time spent) and what it gave. If a step costs more than it gains, the branch stops
there, and only finished steps come back to `main`.
