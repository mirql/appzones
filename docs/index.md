---
title: Home
nav_order: 1
---

# appzones

**Long-lived sandboxes for your desktop apps, dev tools and AI agents on Linux.**
{: .fs-6 .fw-300 }

[Get started](getting-started.html){: .btn .btn-primary .mr-2 } [GitHub](https://github.com/mirql/appzones){: .btn }

[![Release](https://img.shields.io/github/v/release/mirql/appzones?labelColor=181825&color=cba6f7)](https://github.com/mirql/appzones/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-89b4fa?labelColor=181825)](https://github.com/mirql/appzones/blob/main/LICENSE)
[![NixOS flake and module](https://img.shields.io/badge/NixOS-flake%20%2B%20module-89b4fa?logo=nixos&logoColor=white&labelColor=181825)](#install)
[![GitHub stars](https://img.shields.io/github/stars/mirql/appzones?labelColor=181825&color=f9e2af)](https://github.com/mirql/appzones/stargazers)
[![Quality gate](https://sonarcloud.io/api/project_badges/measure?project=mirql_appzones&metric=alert_status)](https://sonarcloud.io/summary/overall?id=mirql_appzones)
[![Security rating](https://sonarcloud.io/api/project_badges/measure?project=mirql_appzones&metric=security_rating)](https://sonarcloud.io/summary/overall?id=mirql_appzones)
[![Python dependencies: 0](https://img.shields.io/badge/Python%20dependencies-0-a6e3a1?logo=python&logoColor=white&labelColor=181825)](https://github.com/mirql/appzones/blob/main/CONTRIBUTING.md)
{: .badges }

An app zone is a place your apps live in: `daily` for messengers, `work` for
Slack and the work browser, `untrusted` for links you don't trust, `dev` for
`npm install`, `agents` for an AI agent with one project. Each zone has its
own home, its own network rules and its own window border colour. You declare
them in one TOML file; they run as systemd user units.

![A link opens in the untrusted zone; its attempts to reach ~/.ssh, ~/Documents, the host's dev server, the other zones and the LAN are refused; the internet is reachable.](https://raw.githubusercontent.com/mirql/appzones/main/assets/concept/concept-dark.svg)

```bash
appzones run daily firefox        # start the zone if needed, launch Firefox in it
appzones shell untrusted          # a shell inside
appzones mount dev ~/src/thing    # share a folder with a running zone, until umount
appzones list                     # zones, state, network, devices, apps running
```

![A terminal: inside the untrusted zone the home is empty, the SSH key is not there, a LAN address is refused and the internet works.](demo-untrusted.gif)

## Install

NixOS with flakes (x86_64, a Wayland compositor with `wp_security_context_v1`;
window borders need Hyprland):

```nix
inputs.appzones.url = "github:mirql/appzones";
imports = [ inputs.appzones.nixosModules.default ];
programs.appzones.enable = true;
```

Then `appzones new daily -y` and `appzones check`. The [install page](getting-started/install.html)
has the details; [recipes](recipes.html) show what zones are good for.

## More

- [The README](https://github.com/mirql/appzones#readme): recipes for browsers, package managers and AI agents, and how appzones compares.
- [The agent skill](https://github.com/mirql/appzones/blob/main/skills/appzones/SKILL.md): `npx skills add mirql/appzones` teaches your coding agent to use appzones.
- [Discussions](https://github.com/mirql/appzones/discussions) for questions and ideas; [good first issues](https://github.com/mirql/appzones/labels/good%20first%20issue) to help.
- Found a way out of a zone? [Report it privately](https://github.com/mirql/appzones/security/advisories/new).
