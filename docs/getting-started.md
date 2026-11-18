# Getting started

An app zone is a long-lived sandbox that desktop apps run in. Each
zone has its own home directory, its own network rules and its own
colour. Apps in one zone cannot see the host home, other zones, or the
host's sockets. Zones are declared in one TOML file and run as systemd
user units.

The example configuration (`examples/zones.toml`) declares three:

| zone | colour | for | network | sound |
|---|---|---|---|---|
| `daily` | green `#a6e3a1` | messengers, personal browsing, media | internet | yes |
| `work` | blue `#89b4fa` | work browser, Slack, VS Code | internet + NetBird mesh | yes |
| `untrusted` | red `#f38ba8` | links and files you do not trust | internet, public DNS | no |

Every zone starts with an empty home. No app data, profile or login is
copied in from the host: you sign in to each app again inside the zone.
Only your look is copied at each start (GTK/Qt settings, fonts, icons,
cursor; `theme = false` turns it off). Host apps, their profiles and their
launcher entries stay as they are.

- [Install](getting-started/install.md): the NixOS module, kernel modules, NVIDIA.
- [First zones](getting-started/first-zones.md): run apps, launcher entries, change a zone.
- [Upgrading](getting-started/upgrading.md): from 0.5.
