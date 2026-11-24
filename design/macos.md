# appzones on macOS

Status: stages 1 to 5 built (2026-11-24): command-line zones with every `network`
setting (see "Stage 2, as built" and "Stage 3, as built"). Tested on Apple silicon with
macOS 27.

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
| `wayland`, `x11`, colours, window tags | no Wayland or X11 on macOS; desktop apps: see below. Colours and window tags: not possible (macOS has no hook for another app's window frames) |
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
- `zone_init.rs` runs on macOS too, as the zone's agent (run, shell, ps, status), with
  its Linux-only parts (signalfd, `close_range`, `prctl`) behind small portable
  functions (a self-pipe, closing descriptors one by one, `PT_DENY_ATTACH`). Built
  beside the sandbox, not in it: see "Stage 2, as built".
- `serve.rs` and `cli.rs`: the same, with pidfds and `PR_SET_PDEATHSIG` replaced on
  macOS by kqueue's `EVFILT_PROC` (a process exit event) and a process group.

The first build on the Mac (this commit's parent) fails in 10 files with 50 errors,
all of that kind: `pipe2`, `accept4`, `SOCK_CLOEXEC`, `prctl`, pidfds, `signalfd`,
`O_PATH`/`openat2`, abstract unix sockets, mount syscalls.

## Desktop apps (tried 2026-11-21)

- **Plain apps work.** kitty (from nixpkgs) opened its window inside a deny-by-default
  profile, with the user's home closed (the zone's own folder open) and no network:
  `design/macos/gui-zone.sb`. Beyond what a command-line zone needs, a GUI app needs
  `ipc*`, `process-info*`, `iokit*`, `file-ioctl`, `file-test-existence` and
  `sysctl-write` (to be narrowed to the one sysctl it writes), and `mach-lookup` (to be
  narrowed to the WindowServer's and the system's services: an open mach-lookup also
  reaches the pasteboard and services that act for the user).
- **It must be the app's binary, executed by the zone.** `open` and LaunchServices
  start an app through launchd, outside the sandbox. Apple's own apps (Calculator,
  Safari, TextEdit) are killed when anything but launchd starts them (launch
  constraints): they cannot be zone apps at all.
- **Apps run in the user's login session**, not in an ssh session (launchd's Background
  session has no WindowServer). A zone started from Terminal is there already;
  tests over ssh start the zone as a LaunchAgent in `gui/UID`.
- **Mac apps ignore `HOME`** for their folders: they ask the system for the home and
  write to the real `~/Library/...`, which the profile refuses. A zone app needs its
  `~/Library` folders redirected (bound copies are not possible on macOS), or allowed
  one by one per app; that is the main work for desktop apps.
- **Browsers: not yet.** Mozilla's signed Firefox runs unsandboxed but crashes inside
  the zone profile, with its own sandbox on or off, and with its `~/Library` folders
  allowed; the cause is not found yet. (nixpkgs' `firefox-bin` is not signed, and
  macOS on Apple silicon kills it before it starts.)

So: command-line zones first (stage 2), plain desktop apps right after (their profile
is known), browsers later, as their own piece of work.

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

## Stage 2, as built (2026-11-22)

- **zone-init runs beside the zone, outside its sandbox**, as the server's child, and
  starts each app as `sandbox-exec -f zone.sb -- APP`. Inside the sandbox it would share
  it with the zone's processes, and a profile that lets them signal each other (job
  control, `kill` of their own children) lets them stop zone-init too: the zone would
  then outlive its server. Outside, they cannot signal or look at it (another sandbox,
  `process-info` and `signal` only for `same-sandbox`).
- **The server hands zone-init what `_netsetup` does on Linux**: the control socket
  (made on the host side, an inherited descriptor) and, instead of a pidfd, the read end
  of a pipe whose write end only the server holds: it closes however the server ends.
- **No pid namespace, so the zone's processes are found and ended by their sandbox**:
  zone-init (on SIGTERM or the server's end) and the server (when zone-init ends, and
  before a start) send SIGKILL to every process of the zone, orphans and `setsid`
  included (a sandbox is inherited and cannot be left). A process is the zone's when
  `sandbox_check` says its profile may read one mark file and may not read another
  beside it: two empty files in the zone's state folder with a random name of the zone's,
  one allowed and one refused by name in its profile alone. A first version told them by
  "may read the zone's home, may not read the runtime folder": Apple's own agents
  (AMPDeviceDiscoveryAgent) pass that, and one was killed in a test.
- **What a zone may**: read the system, not `/Users` (nor its firmlinked path under
  `/System/Volumes/Data`), `/Volumes`, the per-user temp and cache folders, `/tmp`, the
  never-list; read and write its home and tmp (`TMPDIR`; emptied at each start unless
  `tmp = "disk"`), read its `ro` binds and write its `rw` binds, at their real paths;
  `HOME` is its own home. Its own terminal (`/dev/tty`, ioctls on the pty it was given)
  but no new ptys: opening `/dev/ttysN` by path would reach the user's other terminals.
  Unix sockets in its own folders with `network = "none"`; everything with `"host"`.
  Services: names (opendirectoryd), the log, notifications; with `"host"` also DNS,
  configd and trustd. Never the preferences daemon or LaunchServices.
- **Machine identity**: the boot and volume UUID sysctls and the IOKit properties that
  tell this Mac (platform UUID, serial numbers, MAC addresses) are refused. gethostuuid(2)
  still answers in a sandbox (no profile rule reaches it): with `machine_id = "zone"` the
  zone's log says so.
- **The never-list on macOS** adds `~/Library/Keychains`, `~/Library/Cookies`,
  `~/Library/LaunchAgents`, TCC, Safari, Mail, Messages, the browsers' profiles and
  appzones' runtime folder (`~/Library/Caches/appzones`, as macOS has no per-user
  runtime directory and empties `$TMPDIR` of what was not used for days); `/private`,
  `/Users`, `/System`, `/Library`, `/Volumes` and `/Applications` are never bound whole
  (`/tmp`, `/var` and `/etc` are links into `/private` there).
- **Refused in `appzones check` and at the start**: what would give the zone more or
  cannot be done (`network = "internet"` until the proxy (stage 3), `dns = "public"`, `links`
  other than `"portal"`, `keyring = "zone"`, devices but gpu and audio, `usb`,
  `wayland = "host"`, `x11 = "own"`, `autostart`, `packages`, `dbus`, `nix_ld`,
  `supervisor = "systemd"`). What only gives it less is said in its log: `gpu` and
  `audio` are not given yet; `bus` means nothing without D-Bus. A per-zone refusal, not
  a config error: a `zones.toml` shared with a Linux machine still works for its other
  zones.

## Stage 3, as built (2026-11-23)

- **The proxy runs in the zone's server** (`helper/src/proxy.rs`), on a new port of
  127.0.0.1 at each start. The port is known only then, so the profile names it as a
  parameter: `(remote tcp (string-append "localhost:" (param "PROXY_PORT")))`, and
  zone-init starts each app with `sandbox-exec -D PROXY_PORT=N`. A profile run without
  the parameter is refused by sandbox-exec (fails closed). The zone may connect to that
  port and to nothing else: not other localhost ports, not the internet directly. The
  rule matches the port on every loopback address (`::1` and IPv4-mapped too; UDP is
  refused anyway), so the proxy listens on `[::1]:PORT` as well and nothing else can.
  When the host's networks cannot be read, the proxy lets nothing through.
- **One port, three ways in**, told by the first byte: HTTP CONNECT (`HTTPS_PROXY`), a
  plain request with an absolute `http://` URL (`HTTP_PROXY`; rewritten for the origin,
  one request per connection, `Connection: close`, so a reused connection never goes to
  another host), SOCKS5 without authentication, CONNECT only (`ALL_PROXY=socks5h://`).
  Apps also get the lowercase forms and `NODE_USE_ENV_PROXY=1` (Node's fetch).
- **What it refuses**: the nft rules' list. The host's own addresses and every network it
  is directly connected to (read with getifaddrs at each connection, so a Mac that moves
  between networks is right at once), 0/8, 10/8, 100.64/10 (CGNAT, tailnets), 127/8,
  169.254/16, 172.16/12, 192.0.0/24, 192.168/16, 198.18/15, 224/4, 240/4; ::, ::1,
  fc00::/7, fe80::/10, multicast, fec0::/10, 64:ff9b:1::/48; IPv4-mapped and NAT64
  (64:ff9b::/96) addresses as the IPv4 address inside. Names are resolved by the proxy
  (the host's resolver), and the address connected to is the one checked: no second
  lookup, no rebinding. A name with only refused addresses: 403 (SOCKS: "not allowed by
  the rules"), and a line in the zone's log (`[proxy] refused HOST:PORT (why)`, at most a
  few a second).
- **In the zone** no names resolve (mDNSResponder is not among its services): a program
  must send names to the proxy. Certificate checks work (trustd, and
  TrustEvaluationAgent, which Apple's LibreSSL asks: without it `/usr/bin/python3` fails
  every https request).
- **Bounds**: 512 connections at once per zone, 15 s and 16 KiB for a request's head,
  10 s per connect. Tried on a Mac: curl (https, http, SOCKS5), git, Python's urllib,
  cargo and pip work; the LAN's gateway, the Mac's own address, localhost, the tailnet
  and the NAT64 form of the gateway are refused; a direct connection, ignoring the proxy,
  fails.
- **Not done**: `dns = "public"` (the proxy would ask 1.1.1.1 itself), `netbird`, and the
  per-zone allow list of host names (#24), which fits here. Any local process can use the
  port too (other zones cannot: their profiles name only their own); it reaches nothing
  the host could not reach itself.

## Stages

1. **It builds on macOS** (done): the Linux-only parts (the mount helper, `_netsetup`,
   zone-wayland, zone-init and its DNS) behind `cfg(target_os = "linux")`, `helper/src/sys.rs`
   for the calls the two systems spell differently. `appzones check`, `list`, `which`, `new`,
   `ps`, `ping`, `stop`, `logs` work there; `up`, `run`, `shell`, `restart`, `serve`, `mount`,
   `apps` say zones do not run on this system yet. Clippy and `cargo test` pass on the Mac
   (`just test-macos user@host` runs them there over ssh). Left for stage 2: finding a zone's
   server by its lock reads `/proc` (macOS: libproc), "dies with its parent" is Linux's
   `PR_SET_PDEATHSIG` (macOS: kqueue's process events), and pidfds (macOS: none).
2. **Zones on macOS** (done): the Seatbelt plan, zone-init beside it, `run`, `shell`,
   `ps`, `stop`, `logs`; `network = "none"` and `"host"`; the settings table above, with
   its errors.
3. **The proxy** (done): `network = "internet"` on macOS (and later the allow list on both
   systems).
4. **Tests** (done): `appzones check --probe` on macOS, with probes of its own (cli.rs,
   macos_probes), and `tests/macos.sh` (`just test-macos-zones USER@MAC`): a zone of each
   `network` kind, every probe in each, then binds and a zone's end, which the probes cannot
   see from inside. Checked against a weakened profile (the home and the network opened):
   the probes for them fail, as they must.
5. **Packaging** (done): `packages.aarch64-darwin.appzones` (nix/package-darwin.nix: the
   binary and `appzones`, nothing else; the helper's unit and CLI tests in its build, which
   on macOS runs them from /private/tmp, as Nix's build folder is under /nix/var, on the
   never-list) and `darwinModules.default` (nix/darwin-module.nix: the package and
   /etc/appzones/denied-paths). tests/darwin-module.nix builds a nix-darwin system with it.
   Homebrew later; a LaunchAgent for `autostart` with the module, when autostart comes.

## Decided

- `network = "internet"` through a proxy fails closed: a program that ignores
  `HTTPS_PROXY` gets no network, and the docs say so. A weaker mode (outbound allowed,
  the LAN reachable) only if asked for, under its own name.
- A Mac app's `~/Library` folders: one zone-wide rule (the zone may write
  `~/Library/*/<the app's id>` for its declared apps), the never-list still first.
- `machine_id = "zone"` (the default) cannot hide gethostuuid(2) on macOS: it stays the
  default there, with a note in the zone's log and the docs. Refusing it would make every
  Mac zone say `machine_id = "host"`, which hides nothing more and says less.
- New ptys in a zone (tmux, `script`, agents that run commands in a pty) are left out for
  now: they need `/dev/ttysN` by path, which reaches every terminal of the user. Later,
  zone-init can open a pty outside the sandbox and hand the zone its descriptors (as it
  does for `shell`), so a zone gets ptys of its own and never the user's.
