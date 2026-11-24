# Configuration

Zones and apps are declared in `~/.config/appzones/zones.toml`. Every
`appzones` command validates it first; `appzones check` prints every error.

- [Reference](configuration/reference.md): every key of a zone and of an app.
- [Files and folders](configuration/files.md): runtime mounts, `/tmp` on disk.
- [Devices and USB](configuration/devices.md): USB devices for a zone.
- [D-Bus](configuration/dbus.md): what a zone may talk to on the session bus, and a zone's own bus.
- [Generic binaries](configuration/nix-ld.md): prebuilt Linux binaries with `nix_ld`.
- [Project zones](configuration/projects.md): a repository's own zones in `appzones.toml`.

[`examples/minimal.toml`](https://github.com/mirql/appzones/blob/main/examples/minimal.toml) is a first configuration that works on any host;
[`examples/zones.toml`](https://github.com/mirql/appzones/blob/main/examples/zones.toml) shows every key, commented.
