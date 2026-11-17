# Security policy

appzones is a sandbox. Reports of ways out of it are very welcome.

## Reporting a vulnerability

Please **do not open a public issue.** Report it privately through
GitHub: **Security → Report a vulnerability** on this repository
(private vulnerability reporting).

Include what you can: the `zones.toml` (or the relevant zone), the commands
you ran, what the zone could reach that it should not, and your appzones
version, kernel and compositor. We will acknowledge the report, keep you
updated, and credit you in the advisory unless you prefer otherwise.

## In scope

- reading or writing host files outside a zone's home and binds, or
  getting around the never-list (symlinks, races, mount tricks);
- abusing `zones-mount-helper` (the root step of `zone mount`), or the
  sudo rule the module installs;
- reaching what the network deny list should refuse (LAN, host loopback,
  tailnets, another zone) with `network = "internet"`;
- reaching D-Bus names or portal methods the proxy should refuse, or
  starting a host program from inside a zone;
- Wayland: using privileged protocols the security context should hide
  (screencopy, data-control, virtual input, ...), or faking a window's zone
  tag or border colour;
- one zone reaching another zone's files, bus names or sockets.

## Known limits (not vulnerabilities by themselves)

These are documented in the guide's "Honest limits": zones share your uid
and kernel (a kernel bug escapes everything), an app can read everything in
its own zone, draw windows that look like anything, read the clipboard
through a focused window, and use the internet as the host. A report that
shows one of these is worse than documented is still welcome.

## Supported versions

Only the latest release on `main` gets security fixes.
