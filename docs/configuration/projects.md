# Project zones

A repository can carry its own zones in an `appzones.toml` at its root, so a team shares one
setup: "this repo's agent may reach only these hosts". Each person trusts the file once on
their machine. Nothing in it is used before that.

```toml
# appzones.toml, committed in the repository
version  = 1
requires = 8
default  = "agent"                 # the zone `@` means (needed when there are several)

[zones.agent]
network  = "internet"
allow    = ["api.anthropic.com", "github.com", "*.githubusercontent.com"]

[zones.dev]
network  = "internet"
checkout = "rw"                    # the project folder writable (default: read-only)
```

```bash
cd ~/src/my-app
appzones trust                     # shows what it asks for; trust it with a typed y
appzones run @ -- claude           # the default zone (agent)
appzones shell @dev                # a zone by name
appzones list                      # project zones show as my-app/agent
appzones untrust                   # forget it, and stop its zones
```

## What a project zone is

- **The project folder**, bound at its real path, read-only unless the zone says
  `checkout = "rw"`. A command started from a folder inside it starts in the same folder.
- **Its own home** (kept per project and zone), and nothing else of yours.
- **No desktop**: no windows, sound or devices, no D-Bus (no portals, notifications or
  keyring). It is for dev tools, package managers and AI agents.
- **No network** unless it says `network = "internet"`, with or without an `allow` list.
- **A colour of its own**, never one of your zones' colours.

## What a project file may say

| key | values |
|---|---|
| `network` | `"none"` (the default), `"internet"` |
| `allow` | host names, `*.domain` (see the [reference](reference.md)) |
| `checkout` | `"ro"` (the default), `"rw"`: the project folder |
| `binds` | relative paths inside the project, `ro` (the default) or `rw`; they may not lead outside it (`..`, `/`, `~`, a symlink out are refused) |
| `env` | extra environment (the never-list applies) |
| `packages`, `tmp`, `linger`, `start_timeout` | as in your own zones |

Everything else is yours alone: a project file can never ask for the host's network, D-Bus,
devices, Wayland, a home of its choosing, a colour, autostart, or a folder outside the project.
At the top level it has `version`, `requires` (8 or more: an older `appzones` then says to
update) and `default`.

## Trust

`appzones trust` (in the project, or `appzones trust DIR`) shows what the file asks for,
**the widenings first** (the internet, a read-write folder, packages built on your host),
then the rest, and asks at the terminal. It never takes a yes from a pipe or a script, so an
agent cannot trust a project for you.

A yes copies the file into `~/.local/state/appzones/projects/` (on the never-list: no zone can
change it). The zones run from that copy, never from the repository, whose file a zone with
the folder writable could rewrite. When the repository's file differs from the copy, `@`
refuses to start a zone and `appzones trust` shows what changed. `stop`, `ps` and `logs`
still work.

Trust is yours and this machine's: it never travels with the repository. Moving the project
needs trusting again.

**What trust does not cover.** A zone with the project folder read-write can change files you
later run on your host: `.git/hooks`, `.git/config` (an `fsmonitor` or a credential helper),
`.envrc`, `Makefile`, `package.json` scripts. Look at what changed before you run them, or keep
the checkout read-only and copy results out.

## Finding the project

From the current folder up, the first folder with an `appzones.toml` is the project. The
search stops at a folder holding `.git` (the repository's root), and never looks at your home
itself or above it: a project lives inside your home.

## Your own additions

What differs on your machine (a folder of yours, an extra host, an environment variable) goes
in **your** `zones.toml`, never in the repository. It is your file, so it needs no trust, and
it may use any zone key (not `colour`, `autostart` or `autostart_apps`: project zones never
start at login):

```toml
requires = 9                       # additions are release 9

[projects."~/src/my-app".zones.dev]
binds = [{ path = "~/datasets", mode = "ro" }]          # added to the project's own binds
env   = { API_URL = "http://localhost:8080" }            # merged with the project's env
allow = ["api.anthropic.com", "internal.example.com"]   # any other key replaces the project's value
```

The path is the project's folder (absolute or `~/`). `appzones which @dev` marks what comes from
you with `(yours)`. The never-list applies to your binds as everywhere; a project zone still has
no host D-Bus.

## Your own zones.toml

```toml
requires = 8                       # [projects] is release 8: raise your file's requires to 8
[projects]
enabled = false                    # ignore every project file (the default: true)
```

`appzones up --all` and `restart --all` are for your own zones; `stop --all` stops project
zones too. A host zone may not be named like a project zone's internal name (`p` and 10 hex
digits).
