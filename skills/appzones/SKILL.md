---
name: appzones
description: Run, inspect and configure app zones with the `appzones` CLI (long-lived bubblewrap sandboxes for desktop apps, dev tools and AI agents on Linux/NixOS, declared in ~/.config/appzones/zones.toml). Use when the user asks to run an app or command "in a zone", start/stop/restart a zone, add or change a zone or app, share a folder with a zone, isolate a browser, package manager or AI agent, or asks why something does not work inside a zone (a site or name that does not resolve, the LAN or localhost unreachable, a missing file, an app that exits, no sound or camera). Also when they mention appzones, zones.toml, `appzones run`, zone homes or zone border colours. Not for DNS zones, Kubernetes or cloud availability zones, or time zones.
---

# appzones

appzones runs desktop apps in **zones**: long-lived sandboxes, each with its
own home (`~/.local/share/appzones/NAME/home`), its own network rules and its
own window border colour. A zone is a systemd user unit
(`appzones@NAME.service`) built from bubblewrap, pasta + nftables,
xdg-dbus-proxy and the Wayland security context. Zones are declared in one
TOML file; the `appzones` CLI starts them and runs things inside.

Docs: https://mirql.github.io/appzones/ . When a task needs a key or a detail not covered here, read
the page as raw markdown: https://raw.githubusercontent.com/mirql/appzones/main/docs/configuration/reference.md (every key),
`.../configuration/files.md` (binds, mounts), `.../configuration/dbus.md`,
`.../configuration/devices.md` (USB), `.../troubleshooting.md`, `.../desktop.md`
(Hyprland), `.../security.md` (limits).

## Rules (read first)

These protect the user's work and the sandbox. Follow them even when asked
to "just make it work"; explain the trade-off and let the user decide.

1. **Ask before anything that stops or restarts a zone**: `appzones stop`,
   `restart`, `up --all`, and `run --restart`. It closes every app in the
   zone (browsers, chats, editors, unsaved work) and drops its runtime
   mounts. Show `appzones list` (the APPS column) and say what will close.
   `appzones run` without `--restart` and `appzones up ZONE` are safe: they
   start a stopped zone and touch nothing running.
2. **Never weaken isolation without the user's explicit yes for that
   change**: `network = "host"`, `force_host`, a writable bind of a large
   or sensitive folder, `dbus` `talk`/`own` grants, `userns = false`,
   `devices` the user did not ask for. Say what the zone could then reach.
3. **Never bind or mount secrets or the whole home**, and never try to get
   around the never-list (`~/.ssh`, `~/.gnupg`, shell and session config,
   `/run/secrets`, the host's `deniedPaths`, ...). It refuses with a reason;
   pass the reason on. Never put secrets in `env`.
4. **Prefer temporary and read-only.** For "let the zone see this folder",
   use `appzones mount ZONE DIR --ro` (gone when the zone stops) before a
   permanent `binds` entry, and `mode = "ro"` unless the zone must write.
   A writable bind can plant things the host runs later (`.git/hooks`,
   `.envrc`, `Makefile`, `package.json` scripts): say so when you add one.
5. **Validate every config edit**: `appzones check` must print `ok`. A
   change applies when the zone restarts, which needs the user's yes
   (rule 1), also through `appzones run --restart`.
6. **Never use sudo** for appzones commands. The CLI calls its one root
   helper itself (for `mount`/`umount`).
7. **If you run inside a zone yourself** (`$ZONE` is set), you see only
   that zone. Do not try to reach the host or other zones; ask the user to
   run host-side commands.

## Look first

```bash
command -v appzones && appzones list     # zones: state, network, dns, devices, apps running, colour
appzones which ZONE                      # every setting in effect and where it came from
```

The config is `~/.config/appzones/zones.toml` (or `$APPZONES_CONFIG`). It is
often a symlink into the user's dotfiles: edit the target (`readlink -f`).
appzones before 0.6 used `~/.config/zones/zones.toml` and
`~/.local/share/zones`; 0.6 reads the old config (with a warning) and moves
the old state by itself.

## Tasks

**Run an app or a command**

```bash
appzones run ZONE APP [ARGS...]          # an app from [apps] that lists ZONE
appzones run ZONE -- CMD [ARGS...]       # any command
appzones run --wait ZONE -- CMD </dev/null   # scripted: its output and exit code are yours
```

`run` starts the zone if needed. For output you need to read, always use
`--wait` with non-tty stdin (it refuses a tty). `appzones shell ZONE` is
interactive and needs a real terminal: leave it to the user.

**Inspect and debug**

```bash
appzones ps ZONE                         # processes in the zone
appzones logs ZONE                       # its journal (the user can -f)
appzones check --probe ZONE              # isolation probes inside a running zone
```

**Share a folder for a while**

```bash
appzones mount ZONE ~/Downloads --ro --as dl   # /mnt/host/dl inside, until umount or zone stop
appzones mounts ZONE
appzones umount ZONE dl                  # busy: it lists the holder; --lazy detaches now
```

**Add a zone**

```bash
appzones new NAME                        # prints a safe block; review it with the user
appzones new NAME -y                     # appends it to zones.toml
appzones check
```

Pick a unique `colour`. Start minimal (no binds, few devices); add what the
user's apps need.

**Add an app** (a launcher entry "App (zone)" per zone)

```toml
[apps.firefox]
command = ["firefox"]
zones   = ["daily", "untrusted"]
args    = { untrusted = ["--private-window"] }
```

Then `appzones check` and `appzones apps` (rewrites the launcher entries).

**An isolated place for a dev tool or AI agent**

```toml
[zones.agents]
colour   = "#cba6f7"
network  = "internet"                                  # explicit: never inherit "host" from [defaults]
netbird  = false                                       # nor the NetBird mesh
binds    = [{ path = "~/src/my-app", mode = "rw" }]   # this project only
packages = ["nodejs", "git"]                           # nixpkgs attributes on PATH
nix_ld   = true                                        # prebuilt binaries (npm, pip) run
```

The tool's login, tokens and caches stay in the zone's home. It cannot read
`~/.ssh`, other projects, `SSH_AUTH_SOCK`, the LAN or the host's own
services. Rule 4's warning about writable binds applies.

## When something does not work

| Symptom | Why | What to do |
|---|---|---|
| A site or name does not resolve in the zone, but works elsewhere | `dns = "host"` uses the host's resolver, with its ad blocking and blocklists | Check on the host (`getent hosts NAME`). Either allow it in the host's DNS, or set `dns = "public"` for that zone (1.1.1.1, minimal `/etc/hosts`; internal names then do not resolve) |
| LAN, router, `localhost`, the host's services or a tailnet unreachable | By design: `network = "internet"` refuses all of them | For a NetBird mesh: `netbird = true`. `network = "host"` only with the user's yes (rule 2) |
| `zone X cannot see FILE (not inside its binds)` | The file is outside the zone | `appzones mount`, a bind, or the app's file chooser (portal) |
| `refused: ... never enters a zone` | The never-list | Do not work around it (rule 3); tell the user why |
| An app exits at once; `run` prints its status and last output | Often a single-instance GTK or Electron app that cannot own its D-Bus name on the host bus | Use the hint: `bus = "zone"` for that app in `[apps]`, or `--bus zone` once |
| Warning that zones.toml changed for a running zone | The zone runs with the old settings | Restart it, or `appzones run --restart` when no app runs in it: both after the user's yes (rule 1) |
| `required file not found` for a downloaded binary | Generic glibc binaries need a loader | `nix_ld = true` (host needs `programs.nix-ld.enable`) |
| Chromium or Electron apps refuse to start | Their sandbox needs nested user namespaces | `userns = true` (the default) |
| No sound, mic, camera or screen sharing | Not in `devices` | Add `audio`, `mic`, `camera`, `screencast` as needed (rule 2 for `mic`/`camera`) |
| An X11-only app, a Flatpak app, an AppImage | X11-only apps do not run yet; Flatpak apps need the host's Flatpak; AppImages need FUSE | Run Flatpaks on the host; AppImages with `--appimage-extract-and-run` |
| A window has no zone border colour | Borders and tags need Hyprland and `contrib/hyprland/appzones.lua` | See the docs' desktop page |

## Config keys at a glance

Per zone (`[zones.NAME]`; shared defaults in `[defaults]`):

- `colour` `"#rrggbb"` (required, unique) · `network` `internet` | `none` | `host` · `netbird` · `dns` `host` | `public`
- `devices` `gpu` `audio` `mic` `camera` `screencast` `usb` (+ `usb = ["vvvv:pppp"]`) · `links` `portal` | `zone` | `zone:NAME`
- `binds` `[{ path, mode = "rw" | "ro" }]` · `home` · `tmp` `ram` | `disk` · `packages` · `nix_ld` · `env` · `dbus { talk, own }` · `bus` `host` | `zone` | `none`
- `autostart` · `autostart_apps` · `userns` · `machine_id` · `theme` · `clipboard` · `keyring` · `x11` · `force_host`

Per app (`[apps.ID]`): `command`, `zones`, `args`, `desktop`, `bus`.

Unknown keys are errors (`appzones check` suggests the closest one). A file
can say `requires = N` (the config release it needs); an older appzones then
asks for an upgrade.

## Limits to keep in mind

Zones share the user's uid and kernel: a kernel bug reachable from inside
escapes everything. An app can read everything in its own zone, draw windows
that look like anything (the border colour is the defence), and use the
internet as the user. Do not describe a zone as safer than that.
