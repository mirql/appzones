"""zones-mount-helper: the one step of `appzones mount` / `appzones umount` that needs root.

Only through sudo, as the zone CLI runs it (the NixOS module's sudo rule
allows exactly this file, for the zones user, without a password):

    sudo -n zones-mount-helper mount ZONE NAME SOURCE [--ro]
    sudo -n zones-mount-helper umount ZONE NAME [--lazy]
    sudo -n zones-mount-helper umount-all ZONE

The only place it mounts or unmounts is
    <the sudo caller's home>/.local/share/zones/ZONE/mnt/NAME
which a running zone sees as /mnt/host/NAME (zone.py binds .../ZONE/mnt
read-only at /mnt/host; a host mount below it propagates into the zone).

Why a caller cannot abuse it (docs/guide.md "Runtime mounts"):
- who: the caller is SUDO_UID (sudo sets it; the caller cannot). Never root
  itself. A zone cannot run sudo at all: no_new_privs, and no /run/wrappers.
- what is mounted: SOURCE is opened *as the caller* (its uid, gid and
  groups), so it must be a directory the caller can already reach and read;
  the never-list (zonecfg.mount_source_error: the binds' list, is, contains
  or is inside, on the resolved path, plus /proc /sys /dev /run /etc /nix
  /boot /root /var /tmp) is checked on the path of that open directory, and
  the mount is made from the same descriptor (open_tree), so nothing can be
  swapped in between. Non-recursive: mounts below SOURCE do not come along.
  Always nosuid and nodev; ro is set before the mount is attached
  (mount_setattr on the detached tree), so the zone never sees it writable.
- where: ZONE and NAME are plain names (no "/", no ".."); the target is
  opened with openat2(RESOLVE_NO_SYMLINKS), must be a directory owned by the
  caller and not already a mount point, and the mount is attached to that
  descriptor (move_mount). Unmounting goes through the descriptor of the
  caller-owned mnt directory, never a path walked again.
"""
from __future__ import annotations

import ctypes
import errno
import os
import pwd
import re
import stat
import sys
from pathlib import Path

for _k in [k for k in os.environ if k.startswith("ZONES_")]:
    del os.environ[_k]  # never a test override when running as root
sys.path.insert(0, str(Path(__file__).resolve().parent))
import zonecfg  # noqa: E402

libc = ctypes.CDLL(None, use_errno=True)
SYS_OPEN_TREE, SYS_MOVE_MOUNT, SYS_OPENAT2, SYS_MOUNT_SETATTR = 428, 429, 437, 442
AT_FDCWD = -100
AT_EMPTY_PATH = 0x1000
OPEN_TREE_CLONE = 1
OPEN_TREE_CLOEXEC = os.O_CLOEXEC
MOVE_MOUNT_F_EMPTY_PATH, MOVE_MOUNT_T_EMPTY_PATH = 0x04, 0x40
MOUNT_ATTR_RDONLY, MOUNT_ATTR_NOSUID, MOUNT_ATTR_NODEV = 0x1, 0x2, 0x4
RESOLVE_NO_MAGICLINKS, RESOLVE_NO_SYMLINKS = 0x02, 0x04
MNT_DETACH, UMOUNT_NOFOLLOW = 2, 8
USAGE = __doc__.split("\n\n")[1]


class OpenHow(ctypes.Structure):
    _fields_ = [("flags", ctypes.c_uint64), ("mode", ctypes.c_uint64), ("resolve", ctypes.c_uint64)]


class MountAttr(ctypes.Structure):
    _fields_ = [("attr_set", ctypes.c_uint64), ("attr_clr", ctypes.c_uint64), ("propagation", ctypes.c_uint64),
                ("userns_fd", ctypes.c_uint64)]


def die(msg: str, code: int = 1):
    print(f"zones-mount-helper: {msg}", file=sys.stderr)
    sys.exit(code)


def check(rc: int, what: str) -> int:
    if rc < 0:
        e = ctypes.get_errno()
        raise OSError(e, f"{what}: {os.strerror(e)}")
    return rc


def openat2_nosym(path: str) -> int:
    # no O_NOFOLLOW: with it a final symlink is ENOTDIR; RESOLVE_NO_SYMLINKS
    # refuses every symlink on the path, the last one too, with ELOOP
    how = OpenHow(os.O_PATH | os.O_DIRECTORY | os.O_CLOEXEC, 0, RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS)
    return check(libc.syscall(ctypes.c_long(SYS_OPENAT2), ctypes.c_int(AT_FDCWD), path.encode(),
                              ctypes.byref(how), ctypes.c_size_t(ctypes.sizeof(how))), f"open {path}")


def fd_path(fd: int) -> str:
    return os.readlink(f"/proc/self/fd/{fd}")


def mount_points() -> list[str]:
    """Mount points in this mount namespace (sudo keeps the caller's)."""
    with open("/proc/self/mountinfo") as f:
        return [re.sub(r"\\([0-7]{3})", lambda m: chr(int(m.group(1), 8)), line.split()[4])
                for line in f if line.strip()]


class Caller:
    def __init__(self):
        if os.geteuid() != 0:
            die("run through sudo (sudo -n zones-mount-helper ...); `appzones mount` does")
        try:
            self.uid = int(os.environ["SUDO_UID"])
        except (KeyError, ValueError):
            die("no SUDO_UID: only through sudo, never as root itself")
        if self.uid == 0:
            die("refused for root: root mounts by hand")
        pw = pwd.getpwuid(self.uid)
        self.gid, self.name = pw.pw_gid, pw.pw_name
        self.home = os.path.realpath(pw.pw_dir)
        zonecfg.HOME = Path(self.home)  # the never-list is the caller's

    def mnt_dir(self, zone: str) -> str:
        if not zonecfg.NAME_RE.match(zone or "") or zone in zonecfg.RESERVED:
            die(f"zone {zone!r}: not a zone name")
        return f"{self.home}/.local/share/zones/{zone}/mnt"

    def open_owned(self, path: str) -> int:
        """path, opened without following any symlink; must be the caller's directory."""
        try:
            fd = openat2_nosym(path)
        except OSError as e:
            die(f"{path}: {e.strerror}" + (" (a symlink on the way: refused)" if e.errno == errno.ELOOP else ""))
        st = os.fstat(fd)
        if not stat.S_ISDIR(st.st_mode) or st.st_uid != self.uid or fd_path(fd) != path:
            die(f"{path}: not a directory of {self.name}'s at exactly that path")
        return fd

    def open_as_caller(self, source: str) -> int:
        """SOURCE opened with the caller's own rights: it must be able to reach and read it."""
        os.setgroups(os.getgrouplist(self.name, self.gid))
        os.setegid(self.gid)
        os.seteuid(self.uid)
        try:
            fd = os.open(source, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
            os.stat(".", dir_fd=fd)  # search permission on it, too
        except OSError as e:
            die(f"{source}: {e.strerror} (as {self.name})")
        finally:
            os.seteuid(0)
            os.setegid(0)
            os.setgroups([])
        return fd


def do_mount(c: Caller, zone: str, name: str, source: str, ro: bool) -> int:
    mnt = c.mnt_dir(zone)
    if not zonecfg.MOUNT_NAME_RE.match(name or ""):
        die(f"name {name!r}: letters, digits, . _ - (not first), at most 64")
    if not source.startswith("/"):
        die(f"{source}: must be an absolute path")
    target = f"{mnt}/{name}"
    pfd = c.open_owned(mnt)
    if target in mount_points():
        die(f"{target}: already mounted (appzones umount {zone} {name} first)")
    tfd = c.open_owned(target)
    sfd = c.open_as_caller(source)
    real = fd_path(sfd)
    if real.endswith(" (deleted)"):
        die(f"{source}: deleted")
    err = zonecfg.mount_source_error(real)
    if err:
        die(f"refused: {err}")
    tree = check(libc.syscall(ctypes.c_long(SYS_OPEN_TREE), ctypes.c_int(sfd), b"",
                              ctypes.c_uint(OPEN_TREE_CLONE | OPEN_TREE_CLOEXEC | AT_EMPTY_PATH)), "open_tree")
    attr = MountAttr(MOUNT_ATTR_NOSUID | MOUNT_ATTR_NODEV | (MOUNT_ATTR_RDONLY if ro else 0), 0, 0, 0)
    check(libc.syscall(ctypes.c_long(SYS_MOUNT_SETATTR), ctypes.c_int(tree), b"", ctypes.c_uint(AT_EMPTY_PATH),
                       ctypes.byref(attr), ctypes.c_size_t(ctypes.sizeof(attr))), "mount_setattr")
    check(libc.syscall(ctypes.c_long(SYS_MOVE_MOUNT), ctypes.c_int(tree), b"", ctypes.c_int(tfd), b"",
                       ctypes.c_uint(MOVE_MOUNT_F_EMPTY_PATH | MOVE_MOUNT_T_EMPTY_PATH)), "move_mount")
    for fd in (tree, sfd, tfd, pfd):
        os.close(fd)
    print(f"mounted {real} at {target} ({'ro' if ro else 'rw'})")
    return 0


def umount_one(pfd: int, target: str, name: str, lazy: bool) -> int:
    # through the caller-owned mnt directory's descriptor: the path is never walked again
    rc = libc.umount2(f"/proc/self/fd/{pfd}/{name}".encode(), UMOUNT_NOFOLLOW | (MNT_DETACH if lazy else 0))
    if rc != 0:
        e = ctypes.get_errno()
        if e == errno.EBUSY:
            print(f"{target}: busy", file=sys.stderr)
            return 3
        die(f"umount {target}: {os.strerror(e)}")
    return 0


def do_umount(c: Caller, zone: str, name: str, lazy: bool) -> int:
    mnt = c.mnt_dir(zone)
    if not zonecfg.MOUNT_NAME_RE.match(name or ""):
        die(f"name {name!r}: not a mount name")
    target = f"{mnt}/{name}"
    pfd = c.open_owned(mnt)
    if target not in mount_points():
        die(f"{target}: not mounted")
    rc = umount_one(pfd, target, name, lazy)
    if rc == 0:
        print(f"unmounted {target}" + (" (lazily: files still open keep it until they close)" if lazy else ""))
    return rc


def do_umount_all(c: Caller, zone: str) -> int:
    mnt = c.mnt_dir(zone)
    if not os.path.isdir(mnt):
        return 0
    pfd = c.open_owned(mnt)
    n = 0
    for _ in range(256):  # a stacked mount shows up again under the same name
        names = [m[len(mnt) + 1:] for m in mount_points() if m.startswith(mnt + "/")]
        names = [x for x in names if zonecfg.MOUNT_NAME_RE.match(x)]
        if not names:
            break
        for x in names:
            umount_one(pfd, f"{mnt}/{x}", x, lazy=True)
            n += 1
    print(f"{n} mount(s) under {mnt} detached")
    return 0


def main(argv: list[str]) -> int:
    if not argv:
        die("usage:\n" + USAGE, 2)
    c = Caller()
    cmd, rest = argv[0], argv[1:]
    flags = {a for a in rest if a.startswith("--")}
    args = [a for a in rest if not a.startswith("--")]
    if cmd == "mount" and len(args) == 3 and flags <= {"--ro"}:
        return do_mount(c, args[0], args[1], args[2], "--ro" in flags)
    if cmd == "umount" and len(args) == 2 and flags <= {"--lazy"}:
        return do_umount(c, args[0], args[1], "--lazy" in flags)
    if cmd == "umount-all" and len(args) == 1 and not flags:
        return do_umount_all(c, args[0])
    die("usage:\n" + USAGE, 2)


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except OSError as e:
        die(str(e))
