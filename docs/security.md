# Security and limits

## Honest limits

1. **Same uid, same kernel.** A kernel bug reachable from inside escapes
   everything, and an escape lands as your user, with your files, keys and
   sudo. Namespaces, nested user namespaces, the GPU driver and nft in the
   namespace all widen what the kernel exposes. A seccomp filter is
   planned; a separate uid or a microVM back end is later.
2. **A compromised app in a zone can still:**
   - read and write everything in its zone: its home, its binds
     (`~/Downloads` in `daily`), and the data of every other app in that
     zone;
   - read the clipboard by mapping a window (Hyprland focuses new
     windows), and set it at any time;
   - draw windows that look like anything. The border colour is the
     defence, so learn the colours;
   - send notifications that look like system ones, and see when you are
     idle;
   - with `audio` (and `mic`): record the microphone and other apps' sound,
     for now;
   - with `gpu`: talk to the GPU driver;
   - use the internet as the host (same public IP), and with `netbird` the
     mesh as the host;
   - reach a network the host joined after the zone started, until the
     zone restarts (the deny list is computed at start);
   - resolve internal DNS names (with `dns = "host"`);
   - ask for a screencast or a file. That is your dialog, every time.
3. **Logins inside zones use the basic store.** Browsers and Electron apps
   keep cookies and passwords under a fixed key, protected only by disk
   encryption.
4. **X11-only apps do not start** until `x11 = "own"` is built.
5. **Flatpak apps do not run in a zone** (they need the host's Flatpak
   service). Run them on the host: they have their own sandbox.
6. **AppImages need `--appimage-extract-and-run`** (FUSE needs setuid,
   which `no_new_privs` disables).
7. **Telegram's tray icon, Slack's screen share and the KDE file
   dialog** are not covered by the VM test.
8. **An app on the zone's own bus has no portals** (see [D-Bus](configuration/dbus.md)): no
   notifications, tray or screen sharing from it.

## Reporting a vulnerability

Found a way out of a zone, a never-list bypass, or a problem in the mount
helper? Please **do not open a public issue**: report it privately through
[Security → Report a vulnerability](https://github.com/mirql/appzones/security/advisories/new).
[SECURITY.md](https://github.com/mirql/appzones/blob/main/SECURITY.md) says what is in scope.
