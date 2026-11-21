# appzones on macOS

Status: a plan (2026-11-21). Nothing of it is built yet but the dev shell
(`nix develop` on aarch64-darwin); tested on Apple silicon with macOS 27.

## What macOS has, and what it does not

appzones on Linux is built from kernel features macOS does not have: user, mount,
pid and network namespaces (bwrap), a network namespace with its own nft rules
(pasta), bind mounts, pidfds, `PR_SET_PDEATHSIG`, the Wayland security context.

macOS has its own sandbox, Seatbelt: `sandbox-exec -p PROFILE CMD` runs CMD (and
everything it starts) under a profile in Apple's sandbox language (SBPL). It is
what Codex and Claude Code use on macOS. `sandbox-exec` is marked deprecated but
ships and works (macOS 27). A profile can:

- deny everything by default, then allow file reads and writes by path
  (`subpath`, `literal`, `regex`), so a zone sees only its own home, its binds and
  the system it needs;
- allow or deny network: all outbound, none, or by `localhost` and port, and unix
  sockets by path; it cannot filter by address range (no "the internet but not the LAN");
- deny `process-exec` of chosen paths, `mach-lookup` of chosen services (the
  macOS counterpart of D-Bus names), signals to processes outside the sandbox.

Tested on the Mac (2026-11-21, `sandbox-exec` with a deny-by-default profile): the
zone's folder readable and writable, the rest of the home and `~/.ssh` refused, no
writes outside the zone; no network at all; or outbound with localhost refused (the
LAN then stays reachable); or outbound to one localhost port only, everything else
refused, the internet and the LAN included. Two notes for the profile: a filter in an
`allow` form applies to every operation in it (`(allow process-exec process-fork)` and
`(allow signal (target self))` must be separate), and a shell started outside the
zone's folders warns that it cannot read its working directory (start it in the zone's home).

There is no pid namespace: a zone's processes are ordinary processes of the user.
There is no mount namespace: a bind is a path the profile allows, at its real path
(not at a path of the zone's choosing), and a mount at run time means a new profile,
so a restart.

## Two ways

- **A. Native, with Seatbelt (first).** Command-line zones: dev tools, package
  managers and AI agents, which is most of what a Mac user would put in a zone.
- **B. A Linux VM** (Apple's Virtualization framework through Lima, already on the
  machine): appzones as on Linux inside it, every setting, at the cost of a VM (a
  few GB of memory) and awkward desktop apps. Later, and also our aarch64 Linux
  test bed (#21).

## A, setting by setting

| setting | on macOS (A) |
|---|---|
| `binds` | allowed in the profile at their real path; `ro` and `rw` as file-read / file-write; the never-list as on Linux, with macOS paths added (`~/Library/Keychains`, `~/Library/Cookies`, `~/Library/Application Support/*` of browsers, `~/.ssh`, ...) |
| the zone's home | `~/.local/share/appzones/NAME/home`, set as `HOME`; the real home denied except the binds. Apps that ask the system for the home (`getpwuid`) get the real path and are refused there: they see an empty, unwritable home |
| `network = "none"` | all network denied |
| `network = "internet"` | the zone may connect to one port only: appzones' own proxy on localhost (HTTP CONNECT and SOCKS5, given to the zone as `HTTPS_PROXY`, `HTTP_PROXY`, `ALL_PROXY`). The proxy refuses the host's addresses, the LAN, private and CGNAT ranges (the lists of the nft rules) and resolves names itself. A program that ignores the proxy variables gets no network at all: it fails closed |
| `network = "host"` | everything, localhost too (warned, as on Linux) |
| `dns` | answered by the proxy: `host` (the system's resolver) or `public` (1.1.1.1), as on Linux; mDNSResponder is denied to the zone |
| `devices` | `gpu`: Metal is allowed (IOKit user clients); `audio`, `camera`, `mic`, `usb`: macOS's own permission prompts (TCC) decide, the profile allows the services; `usb` with ids: an error |
| `dbus` / `bus` | no D-Bus on macOS: `talk`/`own` an error, `bus` ignored; mach services of the session allowed by a fixed list |
| `wayland`, `x11`, colours, window tags | not on macOS: GUI apps are out of scope for A (an app that opens a window is not isolated as a window) |
| `packages`, `nix_ld` | `packages` with Nix (nix-darwin has it); `nix_ld` an error |
| `appzones mount` | not at run time: a refusal that says to add a bind and `appzones restart` |
| `linger`, `autostart` | as on Linux (autostart through a LaunchAgent instead of a systemd unit) |

A setting that cannot be honoured is an error in `appzones check` on macOS, never a
zone that silently gets more.

## The code

Most of it is portable: the config and its never-lists (`config.rs`, `policy.rs`,
`pyfmt.rs`), the commands' outputs, the agent protocol (zone-init's requests and
replies), `settings_hash`. What is Linux is the zone itself:

- `plan.rs` (bwrap command line, pasta, nft), `netsetup.rs`, `zone_wayland.rs`,
  `mount_helper.rs`: Linux only (`cfg(target_os = "linux")`); a new `seatbelt.rs`
  makes the macOS plan: an SBPL profile and the `sandbox-exec` command line.
- `zone_init.rs` runs on macOS too, inside the sandbox, as the zone's agent (run,
  shell, ps, status), with its Linux-only parts (signalfd, `close_range`, `prctl`)
  behind small portable functions (kqueue or a self-pipe, closing descriptors one
  by one, no dumpable flag but the sandbox's `process-info` denial).
- `serve.rs` and `cli.rs`: the same, with pidfds and `PR_SET_PDEATHSIG` replaced on
  macOS by kqueue's `EVFILT_PROC` (a process exit event) and a process group.

The first build on the Mac (this commit's parent) fails in 10 files with 50 errors,
all of that kind: `pipe2`, `accept4`, `SOCK_CLOEXEC`, `prctl`, pidfds, `signalfd`,
`O_PATH`/`openat2`, abstract unix sockets, mount syscalls.

## Hangar

[Hangar](https://github.com/baseguard/hangar) (Apache-2.0, Swift, macOS 27+) is B as a
product: Virtualization.framework VMs (macOS, Linux, Windows) for AI agents, with its own
network daemon (allow lists by host name, every connection logged from outside the VM),
API keys put into requests on the way out (the VM holds only a fake value), snapshots and
clones. For a user who wants a whole VM per agent, it is the answer, and appzones need not
build B itself: B can wait, or be "appzones inside a Lima or Hangar VM".

What appzones can take from it, on both systems:
- the proxy above is also where a per-zone allow list of host names (#24) and a log of a
  zone's connections fit (on Linux, the zone's DNS already passes through the server);
- keys that never enter a zone: the proxy adds a header for chosen hosts, the zone holds a
  placeholder (`keyring = "zone"` and the `*TOKEN*` never-list in a new light).

## Stages

1. **It builds on macOS**: the Linux-only parts behind `cfg`, portable wrappers for
   the rest. `appzones check`, `list`, `which`, `new` work there; `up` says zones
   are not built for macOS yet. `cargo test` passes on the Mac (`just test-macos`
   runs it there over ssh).
2. **Zones on macOS**: the Seatbelt plan, zone-init inside it, `run`, `shell`, `ps`,
   `stop`, `logs`; `network = "none"` and `"host"`; the settings table above, with its errors.
3. **The proxy**: `network = "internet"` on macOS (and later the allow list on both systems).
4. **Tests**: the isolation probes of `tests/vm.nix` that apply (no `~/.ssh`, no
   real home, binds only, no localhost with `internet`, no network with `none`),
   as a script run on the Mac.
5. **Packaging**: a nix-darwin module (as the NixOS one) first; Homebrew later.

## Open questions

- `network = "internet"` through a proxy: programs that ignore `HTTPS_PROXY` get no
  network in such a zone. Fail closed and say so in the docs, or offer a weaker mode
  (outbound allowed, LAN reachable) under its own name?
- Desktop apps on macOS (a browser in a zone): out of scope for now, or a goal?
