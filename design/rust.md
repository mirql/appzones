# Design: the parts of appzones in Rust (experiment)

Status: proposal, on the branch `rust-version`. Nothing here reaches `main` until the
exit criteria below hold. PRs for this go into `rust-version`; `main` is merged into it
regularly (merge commits), so it never drifts far.

## Why only some parts

appzones is about 5,000 lines of Python (standard library only) and a 90-line C helper.
Speed is not a reason to change: it starts processes and waits. The CLI and the server
stay in Python, where contributors find their way in. Three parts move, because Rust
changes what they can do or how easy they are to check:

| Part | Today | Why Rust |
|---|---|---|
| **zone-init**, the agent inside every zone (770 lines), and the zone's `xdg-open` shim for `links = "zone"` | `zone_init.py` and a shell script calling Python, both run by the host's Python inside the zone | A zone then needs no Python of the host's. Today the host's Python and appzones' own files must be visible in every zone, which is why appzones must be installed under `/usr` (or the Nix store). One static binary, bound read-only into the zone, removes that. The host still needs Python 3.11 for the CLI and the server (`zonecfg` reads TOML with `tomllib`). |
| **zone-wayland**, the security-context helper (89 lines of C) | C against libwayland, built by `make` | One build for all helpers; a pure-Rust Wayland client needs no libwayland at build or run time. |
| **the mount helper**, the root step of `appzones mount` (253 lines) | Python through sudo, with `-I` isolation and ctypes for `open_tree`, `move_mount`, `mount_setattr`, `openat2` | The one place that runs as root. A small binary with typed syscalls is easier to audit than an interpreter and its environment. |

## One binary

`appzones-helper`, one static binary (musl) with three commands:

```
appzones-helper zone-init          # in the zone (pid 2, after bwrap)
appzones-helper open-link URL      # in the zone: the xdg-open shim's http(s) part (links.sock)
appzones-helper zone-wayland SOCKET APP_ID INSTANCE
appzones-helper mount-helper ...   # through sudo, root
```

Everything outside stays the same: the agent protocol (JSON lines and SCM_RIGHTS on the
control socket, see the top of `zone_init.py`), the arguments and environment the server
passes (`ZONE_CTL_FD`, `ZONE_SERVER_FD`, `ZONE_DNS`, ...), the mount helper's command line
and checks, `zone-wayland`'s "ready" line. So the Python side does not change, and both
implementations can run side by side.

## Order

Each step is a PR into `rust-version`, with the full checks passing (unit tests, the NixOS
VM test, `just test-distros`).

1. **Skeleton**: `helper/` with `Cargo.toml`, `Cargo.lock`, a Nix build of the static
   binary, `make` building it where `cargo` is present. The binary is installed but nothing
   uses it yet.
2. **zone-wayland** in Rust (the smallest part, host side). The server runs it when
   `APPZONES_HELPER=rust`; the NixOS VM test runs both ways.
3. **zone-init** in Rust, op by op: `ping`, `ps`, `run` (with `--wait` and fds), `status`,
   `shell` (pty), `resize`, `mounts`, `busy`, the zone bus, DNS, the server pidfd watch,
   reaping; and `open-link`, which the `xdg-open` shim calls instead of Python (the VM test
   opens a link from a zone with `links = "zone"` without Python in the zone). Behind
   `APPZONES_HELPER=rust`; the VM test and the distro tests run with both.
4. **The mount helper** in Rust, with the same checks (SUDO_UID only, the never-list on
   the opened directory, `openat2` without symlinks, `open_tree`/`move_mount`,
   `mount_setattr` for read-only). The Python helper's unit tests become the Rust tests'
   cases.
5. **Switch**: Rust by default; then the Python zone-init, the C zone-wayland and the
   Python mount helper go, and the install no longer needs to be under `/usr`.

## Dependencies

Proposed, each pinned in `Cargo.lock`:

- `rustix`: the syscalls (pidfd, `openat2`, mount API, pty, `SCM_RIGHTS`).
- `serde_json`: the agent protocol.
- `wayland-client` with its pure-Rust backend, and `wayland-protocols` for
  `wp_security_context_v1`.
- `mirql-love-sparkle`: mirql's rule for every manifest.

No async runtime: zone-init stays a poll loop as today.

## Exit criteria

The experiment comes back to `main` when, with `APPZONES_HELPER=rust` as the default:

- the NixOS VM test (all checks), `just test-distros` and the Arch test pass unchanged;
- the binary builds reproducibly in Nix, statically, and with `make` on Ubuntu, Debian
  and Arch;
- a zone needs nothing of appzones but the binary: no Python in the zone (also for
  clicked links), no `/usr` requirement for the install (the host keeps Python 3.11 for
  the CLI);
- the mount helper's checks have at least the Python helper's tests.

If a step shows Rust costs more than it gains (build, size, contributors), the branch
stops there; what was learned goes into this file.

## Not here

The CLI and the server stay Python. No new features on this branch: features go to
`main` and arrive here with the next merge.
