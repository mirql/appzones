# Files and folders

A zone sees the host's files only through what you give it: `binds` in the
zone's configuration (permanent, see the [reference](reference.md)), and
`appzones mount` (for a while, below). Both are checked against the same
never-list.

## Runtime mounts

```bash
appzones mount ZONE HOST-DIR [--ro] [--as NAME]   # → /mnt/host/NAME (NAME: HOST-DIR's name)
appzones umount ZONE NAME [--lazy]
appzones mounts ZONE
```

A host folder appears at `/mnt/host/NAME` in a **running** zone without a
restart, and `appzones umount` takes it away again. A mount lasts until
`umount` or until the zone stops or restarts: the zone removes every mount
left at its start and stop (its log says how many; one that could not be detached is
named there). To keep a folder for good, add it to `binds`.

- **Same never-list as `binds`**, on the resolved path, so a symlink cannot
  get around it: a folder that is, contains or is inside a denied path is
  refused (`~/.ssh`, `~`, the keys, agent state, your `deniedPaths`, …). A
  runtime mount also never comes from inside `/proc`, `/sys`, `/dev`,
  `/run` (sockets, `/run/user/1000`), `/etc`, `/nix`, `/boot`, `/root`,
  `/var` or `/tmp`. Only directories.
- **Busy.** If an app in the zone has its working directory, a file or a
  mapping under it, `appzones umount` refuses and lists the processes. `--lazy`
  detaches it at once; files still open keep working until they close.
- Always `nosuid` and `nodev`. With `--ro` the mount is read-only from the
  moment the zone sees it, and the zone cannot remount it writable, not
  even from a nested user namespace.
- **Without systemd** the host's mounts are often private, and a new mount
  does not reach a running zone; `appzones mount` then says so. Run
  `sudo mount --make-rshared /` at every boot, before zones start (systemd
  does the same), then restart the zone and mount again.

**How.** Each zone binds `~/.local/share/appzones/NAME/mnt` read-only at
`/mnt/host`. bwrap makes the zone's mounts slaves of the host's, and
`/home` is normally a shared mount (systemd makes `/` shared), so a mount the host makes under that
directory appears in the zone at once, and an unmount disappears. The zone
cannot create anything in `/mnt/host` itself. A bind mount on the host
needs root, so `appzones` calls one helper, `appzones-mount-helper`, through
`sudo -n`. The NixOS module adds a sudo rule for exactly that file, for
`programs.appzones.user`, without a password (`mountHelper = true`).

**Why sudo, and why it cannot be abused.** The other choice was a
root system service with a socket. That would be one more daemon, and
it would have to find out who is calling (`SO_PEERCRED`) and which mount
namespace they are in. A sudo rule for one file is less code, sudo logs
every call, and the helper always runs in the caller's own namespace.
What the helper accepts:

- **Who.** The caller is `SUDO_UID`, set by sudo. Root itself is refused,
  and so is running it without sudo. A zone cannot run sudo at all:
  `no_new_privs` turns setuid off, and `/run/wrappers` is not in a zone.
- **What.** It opens the source **as the caller** (its uid, gid and
  groups), so the source must be a directory the caller can already reach
  and read. A root-only directory, or one inside a 0700 directory, is
  refused. The never-list is checked on the path of that open directory,
  and the mount is made from the same descriptor (`open_tree`), so nothing
  can be swapped in between. Mounts below the source do not come along,
  and the zone's copy is private: a mount made under the source later (a USB
  stick, a FUSE mount) does not show up in the zone.
- **Where.** Only `<caller's home>/.local/share/appzones/ZONE/mnt/NAME`. ZONE
  and NAME are plain names (no `/`, no `..`). The target is opened with
  `openat2(RESOLVE_NO_SYMLINKS)`, so a symlink anywhere on the path is
  refused. It must be a directory the caller owns and not already a mount
  point. The mount is attached to that descriptor (`move_mount`), and an
  unmount goes through the descriptor of the caller's own `mnt`
  directory, so no path is walked twice.
- `--ro` is set on the detached mount (`mount_setattr`) before it is
  attached, so there is no moment when the zone sees it writable.

So the most a caller can do with the helper is put a directory it can
already read onto a directory it owns. It can do that with no helper at
all by copying, and the never-list still applies. The narrow rule matters
for the zone (which can never use sudo), for the user unit (which calls the
helper at a zone's start and stop), and for a `wheel` that needs a
password.

## /tmp on disk (`tmp = "disk"`)

`/tmp` in the zone is `~/.local/share/appzones/NAME/tmp`, and `/var/tmp` is
`~/.local/share/appzones/NAME/var-tmp`. Both are 0700 and survive `appzones
restart` and reboots, and you can reach them from the host. The zone wrote
everything in them, so do not open links or run files from them on the
host without care.

Cleanup at every zone start: files not read or written for 14 days
(`/tmp`) or 30 days (`/var/tmp`) are removed, then directories that are
that old and empty. Times are atime and mtime; on a `/home` mounted
`noatime` that means "not written". Symlinks are removed
as links and never followed. `ram` (the default) keeps the old behaviour:
a fresh tmpfs at each start, and no `/var/tmp`.
