//! appzones-mount-helper: the one step of `appzones mount` / `appzones umount` that needs
//! root, in Rust (the port of zone_mount_helper.py, whose docstring explains the design).
//!
//!   sudo -n appzones-mount-helper mount ZONE NAME SOURCE [--ro]
//!   sudo -n appzones-mount-helper umount ZONE NAME [--lazy]
//!   sudo -n appzones-mount-helper umount-all ZONE
//!
//! Its own executable (the sudo rule allows this file with any arguments: nothing else may
//! be reachable through it), linked against glibc (the caller's user and groups come from
//! NSS: LDAP and sssd too). It reads no environment but SUDO_UID. All unsafe code is in
//! `sys`, one call per function; the rest is safe Rust. The never-list is policy.rs, kept
//! equal to zonecfg's by a differential test.

#![deny(unsafe_code)]

#[path = "policy.rs"]
mod policy;

use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::process::ExitCode;

const USAGE: &str = "    sudo -n appzones-mount-helper mount ZONE NAME SOURCE [--ro]
    sudo -n appzones-mount-helper umount ZONE NAME [--lazy]
    sudo -n appzones-mount-helper umount-all ZONE";

/// The syscalls, each a thin wrapper that returns Result. The only unsafe code.
#[allow(unsafe_code)]
mod sys {
    use std::ffi::{CStr, CString};
    use std::io::{Error, Result};
    use std::os::fd::{FromRawFd, OwnedFd, RawFd};

    fn cstr(p: &[u8]) -> Result<CString> {
        CString::new(p).map_err(|_| Error::from_raw_os_error(libc::EINVAL))
    }

    fn check(rc: libc::c_long) -> Result<libc::c_long> {
        if rc < 0 { Err(Error::last_os_error()) } else { Ok(rc) }
    }

    pub fn geteuid() -> u32 {
        // Safety: no arguments, no failure.
        unsafe { libc::geteuid() }
    }

    /// The caller's user: (gid, name, home), from NSS (a larger buffer on ERANGE).
    pub fn getpwuid(uid: u32) -> Result<(u32, Vec<u8>, Vec<u8>)> {
        let mut size = 16384;
        loop {
            let mut buf = vec![0u8; size];
            // Safety: getpwuid_r fills `pw` with pointers into `buf`, which outlives their use;
            // each pointer is checked before it is read.
            unsafe {
                let mut pw: libc::passwd = std::mem::zeroed();
                let mut out: *mut libc::passwd = std::ptr::null_mut();
                let rc = libc::getpwuid_r(uid, &mut pw, buf.as_mut_ptr().cast(), buf.len(), &mut out);
                if rc == libc::ERANGE && size < (1 << 20) {
                    size *= 4;
                    continue;
                }
                if rc != 0 {
                    return Err(Error::from_raw_os_error(rc));
                }
                if out.is_null() || pw.pw_name.is_null() || pw.pw_dir.is_null() {
                    return Err(Error::from_raw_os_error(libc::ENOENT));
                }
                return Ok((pw.pw_gid, CStr::from_ptr(pw.pw_name).to_bytes().to_vec(),
                           CStr::from_ptr(pw.pw_dir).to_bytes().to_vec()));
            }
        }
    }

    /// The user's groups (getgrouplist), its primary group included.
    pub fn grouplist(name: &[u8], gid: u32) -> Result<Vec<libc::gid_t>> {
        let name = cstr(name)?;
        let mut n: libc::c_int = 64;
        loop {
            let mut groups = vec![0 as libc::gid_t; n as usize];
            let want = n;
            // Safety: getgrouplist writes at most `n` gids into `groups` and sets `n`.
            let rc = unsafe { libc::getgrouplist(name.as_ptr(), gid, groups.as_mut_ptr(), &mut n) };
            if rc >= 0 {
                groups.truncate(n as usize);
                return Ok(groups);
            }
            if n <= want {
                n = want * 2; // not told how many: try a larger buffer
            }
            if n > 65536 {
                return Err(Error::from_raw_os_error(libc::E2BIG));
            }
        }
    }

    pub fn setgroups(groups: &[libc::gid_t]) -> Result<()> {
        // Safety: reads `groups.len()` gids from the slice.
        check(unsafe { libc::setgroups(groups.len(), groups.as_ptr()) } as libc::c_long).map(drop)
    }

    pub fn setegid(gid: u32) -> Result<()> {
        // Safety: a plain id.
        check(unsafe { libc::setegid(gid) } as libc::c_long).map(drop)
    }

    pub fn seteuid(uid: u32) -> Result<()> {
        // Safety: a plain id.
        check(unsafe { libc::seteuid(uid) } as libc::c_long).map(drop)
    }

    /// PATH opened O_PATH|O_DIRECTORY, refusing every symlink on the way (RESOLVE_NO_SYMLINKS:
    /// the last one too, with ELOOP) and magic links.
    pub fn openat2_nosym(path: &[u8]) -> Result<OwnedFd> {
        let p = cstr(path)?;
        // struct open_how (linux/openat2.h): flags, mode, resolve, all u64
        #[repr(C)]
        struct OpenHow {
            flags: u64,
            mode: u64,
            resolve: u64,
        }
        let how = OpenHow {
            flags: (libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC) as u64,
            mode: 0,
            resolve: libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_MAGICLINKS,
        };
        // Safety: openat2 reads the NUL-terminated path and the open_how struct of the given size.
        let fd = check(unsafe {
            libc::syscall(libc::SYS_openat2, libc::AT_FDCWD, p.as_ptr(), &how as *const OpenHow,
                          std::mem::size_of::<OpenHow>())
        })?;
        // Safety: a new descriptor we own.
        Ok(unsafe { OwnedFd::from_raw_fd(fd as RawFd) })
    }

    /// fstatat(fd, ".", 0): search permission on the directory, as the current euid.
    pub fn stat_dot(fd: RawFd) -> Result<()> {
        // Safety: fstatat writes into a local struct; "." is a static C string.
        unsafe {
            let mut st: libc::stat = std::mem::zeroed();
            check(libc::fstatat(fd, c".".as_ptr(), &mut st, 0) as libc::c_long).map(drop)
        }
    }

    /// A detached copy of the mount at FD (open_tree OPEN_TREE_CLONE, not recursive).
    pub fn open_tree_clone(fd: RawFd) -> Result<OwnedFd> {
        // Safety: an empty path with AT_EMPTY_PATH: the descriptor itself.
        let t = check(unsafe {
            libc::syscall(libc::SYS_open_tree, fd, c"".as_ptr(),
                          (libc::OPEN_TREE_CLONE | libc::OPEN_TREE_CLOEXEC | libc::AT_EMPTY_PATH as u32) as libc::c_uint)
        })?;
        // Safety: a new descriptor we own.
        Ok(unsafe { OwnedFd::from_raw_fd(t as RawFd) })
    }

    pub const MOUNT_ATTR_RDONLY: u64 = 0x1;
    pub const MOUNT_ATTR_NOSUID: u64 = 0x2;
    pub const MOUNT_ATTR_NODEV: u64 = 0x4;

    #[repr(C)]
    struct MountAttr {
        attr_set: u64,
        attr_clr: u64,
        propagation: u64,
        userns_fd: u64,
    }

    /// Set ATTRS on the detached tree, before anyone can see it, and make it private: no
    /// longer a peer of the source, so a mount made later under the source never
    /// propagates into it (the zone gets SOURCE, not what is mounted there afterwards).
    pub fn mount_setattr(tree: RawFd, attrs: u64) -> Result<()> {
        let a = MountAttr { attr_set: attrs, attr_clr: 0, propagation: libc::MS_PRIVATE, userns_fd: 0 };
        // Safety: mount_setattr reads the struct of the given size; the path is empty.
        check(unsafe {
            libc::syscall(libc::SYS_mount_setattr, tree, c"".as_ptr(), libc::AT_EMPTY_PATH as libc::c_uint,
                          &a as *const MountAttr, std::mem::size_of::<MountAttr>())
        }).map(drop)
    }

    /// Attach the detached TREE onto the directory descriptor TARGET.
    pub fn move_mount(tree: RawFd, target: RawFd) -> Result<()> {
        const F_EMPTY: libc::c_uint = 0x04;
        const T_EMPTY: libc::c_uint = 0x40;
        // Safety: both paths empty: the two descriptors themselves.
        check(unsafe { libc::syscall(libc::SYS_move_mount, tree, c"".as_ptr(), target, c"".as_ptr(), F_EMPTY | T_EMPTY) })
            .map(drop)
    }

    pub fn umount2(path: &[u8], flags: libc::c_int) -> Result<()> {
        let p = cstr(path)?;
        // Safety: a NUL-terminated path.
        check(unsafe { libc::umount2(p.as_ptr(), flags) } as libc::c_long).map(drop)
    }
}

fn die(msg: impl AsRef<str>) -> ! {
    eprintln!("appzones-mount-helper: {}", msg.as_ref());
    std::process::exit(1)
}

fn usage() -> ! {
    eprintln!("appzones-mount-helper: usage:\n{USAGE}");
    std::process::exit(2)
}

fn show(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn strerror(e: &std::io::Error) -> String {
    let s = e.to_string();
    s.rsplit_once(" (os error").map_or(s.clone(), |(t, _)| t.to_string())
}

/// The path of descriptor FD, as the kernel sees it now (never a guess: dies otherwise).
fn fd_path(fd: RawFd) -> Vec<u8> {
    std::fs::read_link(format!("/proc/self/fd/{fd}")).map(|p| p.into_os_string().into_vec())
        .unwrap_or_else(|e| die(format!("the path of descriptor {fd}: {}", strerror(&e))))
}

/// /proc/self/mountinfo's mount points (\ooo escapes undone): sudo keeps the caller's
/// mount namespace. Dies when it cannot read them (never an empty list).
fn mount_points() -> Vec<Vec<u8>> {
    let text = std::fs::read("/proc/self/mountinfo")
        .unwrap_or_else(|e| die(format!("/proc/self/mountinfo: {}", strerror(&e))));
    text.split(|&b| b == b'\n').filter_map(|line| {
        let field = line.split(|&b| b == b' ').filter(|f| !f.is_empty()).nth(4)?;
        Some(unescape(field))
    }).collect()
}

fn unescape(f: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(f.len());
    let mut i = 0;
    while i < f.len() {
        if f[i] == b'\\' && i + 3 < f.len() && f[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            out.push(((f[i + 1] - b'0') << 6) | ((f[i + 2] - b'0') << 3) | (f[i + 3] - b'0'));
            i += 4;
        } else {
            out.push(f[i]);
            i += 1;
        }
    }
    out
}

/// appzones' code: the directory this executable sits in, which is where zonecfg.py and
/// zone.py are too (the package and `make install` put them side by side).
fn code_dir() -> OsString {
    let exe = std::env::current_exe().unwrap_or_else(|e| die(format!("own path: {}", strerror(&e))));
    exe.parent().map(|p| p.as_os_str().to_owned()).unwrap_or_else(|| die("own directory unknown"))
}

struct Caller {
    uid: u32,
    gid: u32,
    name: Vec<u8>,
    home: Vec<u8>,
}

impl Caller {
    fn new() -> Caller {
        if sys::geteuid() != 0 {
            die("run through sudo (sudo -n appzones-mount-helper ...); `appzones mount` does");
        }
        let uid: u32 = match std::env::var("SUDO_UID").ok().and_then(|v| v.parse().ok()) {
            Some(u) => u,
            None => die("no SUDO_UID: only through sudo, never as root itself"),
        };
        if uid == 0 {
            die("refused for root: root mounts by hand");
        }
        let (gid, name, dir) = sys::getpwuid(uid).unwrap_or_else(|e| die(format!("uid {uid}: {}", strerror(&e))));
        let home = policy::realpath(&dir).unwrap_or_else(|| die(format!("{}: cannot resolve the home", show(&dir))));
        Caller { uid, gid, name, home }
    }

    fn policy(&self) -> policy::Policy {
        policy::Policy { home: OsString::from_vec(self.home.clone()), code_dir: code_dir(), denied_file: None, state_dir: None, extra: vec![] }
    }

    fn mnt_dir(&self, zone: &str) -> Vec<u8> {
        if !policy::zone_name_ok(zone) || policy::RESERVED.contains(&zone) {
            die(format!("zone {}: not a zone name", policy::py_repr(zone)));
        }
        // the caller's state directory, chosen as appzones chooses it; either way
        // open_owned refuses a symlink anywhere on the path and anything but a directory
        // of the caller's at exactly that path
        let lexists = |p: &[u8]| std::fs::symlink_metadata(OsStr::from_bytes(p)).is_ok();
        let mut base = [self.home.as_slice(), b"/.local/share/appzones"].concat();
        let legacy = [self.home.as_slice(), b"/.local/share/zones"].concat();
        if !lexists(&base) && lexists(&legacy) {
            base = legacy;
        }
        [base.as_slice(), b"/", zone.as_bytes(), b"/mnt"].concat()
    }

    /// PATH, opened without following any symlink; must be the caller's directory.
    fn open_owned(&self, path: &[u8]) -> OwnedFd {
        let fd = sys::openat2_nosym(path).unwrap_or_else(|e| {
            let loop_ = if e.raw_os_error() == Some(libc::ELOOP) { " (a symlink on the way: refused)" } else { "" };
            die(format!("{}: {}{loop_}", show(path), strerror(&e)))
        });
        let ok = File::from(fd.try_clone().unwrap_or_else(|e| die(strerror(&e)))).metadata()
            .map(|m| m.is_dir() && m.uid() == self.uid).unwrap_or(false);
        if !ok || fd_path(fd.as_raw_fd()) != path {
            die(format!("{}: not a directory of {}'s at exactly that path", show(path), show(&self.name)));
        }
        fd
    }

    /// SOURCE opened with the caller's own rights: it must be able to reach and read it.
    fn open_as_caller(&self, source: &[u8]) -> OwnedFd {
        let groups = sys::grouplist(&self.name, self.gid).unwrap_or_else(|e| die(format!("groups: {}", strerror(&e))));
        let drop_to_caller = || -> std::io::Result<()> {
            sys::setgroups(&groups)?;
            sys::setegid(self.gid)?;
            sys::seteuid(self.uid)
        };
        let opened = drop_to_caller().and_then(|_| {
            let f = OpenOptions::new().read(true).custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
                .open(OsStr::from_bytes(source))?;
            sys::stat_dot(f.as_raw_fd())?; // search permission on it, too
            Ok(OwnedFd::from(f))
        });
        // back to root (euid first, then the groups it allows), whatever happened
        let back = sys::seteuid(0).and_then(|_| sys::setegid(0)).and_then(|_| sys::setgroups(&[]));
        if let Err(e) = back {
            die(format!("could not become root again: {}", strerror(&e)));
        }
        opened.unwrap_or_else(|e| die(format!("{}: {} (as {})", show(source), strerror(&e), show(&self.name))))
    }
}

fn do_mount(c: &Caller, zone: &str, name: &str, source: &[u8], ro: bool) -> u8 {
    let mnt = c.mnt_dir(zone);
    if !policy::mount_name_ok(name) {
        die(format!("name {}: letters, digits, . _ - (not first), at most 64", policy::py_repr(name)));
    }
    if !source.starts_with(b"/") {
        die(format!("{}: must be an absolute path", show(source)));
    }
    // never named (as the CLI refuses it too): /proc/PID/root and its kind reach into other
    // mount namespaces, whose paths read back as this one's
    if let Some(d) = policy::NEVER_NAMED.iter().find(|d| source == d.as_bytes() || source.starts_with(format!("{d}/").as_bytes())) {
        die(format!("refused: {}: is inside {d}, which never enters a zone", show(source)));
    }
    let target = [mnt.as_slice(), b"/", name.as_bytes()].concat();
    let _pfd = c.open_owned(&mnt);
    if mount_points().contains(&target) {
        die(format!("{}: already mounted (appzones umount {zone} {name} first)", show(&target)));
    }
    let tfd = c.open_owned(&target);
    let sfd = c.open_as_caller(source);
    let real = fd_path(sfd.as_raw_fd());
    if real.ends_with(b" (deleted)") {
        die(format!("{}: deleted", show(source)));
    }
    // the never-list on the path of the open directory as it is, not walked again (a
    // rename in between cannot change what is checked), and its type from the descriptor
    let meta = File::from(sfd.try_clone().unwrap_or_else(|e| die(strerror(&e)))).metadata()
        .unwrap_or_else(|e| die(format!("{}: {}", show(&real), strerror(&e))));
    if let Some(err) = c.policy().mount_source_error_resolved(&real, &meta) {
        die(format!("refused: {err}"));
    }
    let tree = sys::open_tree_clone(sfd.as_raw_fd()).unwrap_or_else(|e| die(format!("open_tree: {}", strerror(&e))));
    let attrs = sys::MOUNT_ATTR_NOSUID | sys::MOUNT_ATTR_NODEV | if ro { sys::MOUNT_ATTR_RDONLY } else { 0 };
    sys::mount_setattr(tree.as_raw_fd(), attrs).unwrap_or_else(|e| die(format!("mount_setattr: {}", strerror(&e))));
    sys::move_mount(tree.as_raw_fd(), tfd.as_raw_fd()).unwrap_or_else(|e| die(format!("move_mount: {}", strerror(&e))));
    // where it went: the target directory as it is now (the caller can rename it, or a
    // directory above it, between the check and the mount); anywhere else, undone at once
    let landed = fd_path(tree.as_raw_fd());
    if landed != target {
        let undo = sys::umount2(format!("/proc/self/fd/{}", tree.as_raw_fd()).as_bytes(), libc::MNT_DETACH);
        die(format!("{}: moved while it was mounted (to {}); {}", show(&target), show(&landed),
                    if undo.is_ok() { "detached again" } else { "COULD NOT DETACH IT: umount it by hand" }));
    }
    // mounted: from here nothing may fail (a closed stdout is not a failure)
    let _ = writeln!(std::io::stdout(), "mounted {} at {} ({})", show(&real), show(&target), if ro { "ro" } else { "rw" });
    0
}

/// Through the caller-owned mnt directory's descriptor: the path is never walked again.
fn umount_one(pfd: RawFd, target: &[u8], name: &[u8], lazy: bool) -> u8 {
    let path = [format!("/proc/self/fd/{pfd}/").as_bytes(), name].concat();
    let flags = libc::UMOUNT_NOFOLLOW | if lazy { libc::MNT_DETACH } else { 0 };
    match sys::umount2(&path, flags) {
        Ok(()) => 0,
        Err(e) if e.raw_os_error() == Some(libc::EBUSY) => {
            eprintln!("{}: busy", show(target));
            3
        }
        Err(e) => die(format!("umount {}: {}", show(target), strerror(&e))),
    }
}

fn do_umount(c: &Caller, zone: &str, name: &str, lazy: bool) -> u8 {
    let mnt = c.mnt_dir(zone);
    if !policy::mount_name_ok(name) {
        die(format!("name {}: not a mount name", policy::py_repr(name)));
    }
    let target = [mnt.as_slice(), b"/", name.as_bytes()].concat();
    let pfd = c.open_owned(&mnt);
    if !mount_points().contains(&target) {
        die(format!("{}: not mounted", show(&target)));
    }
    let rc = umount_one(pfd.as_raw_fd(), &target, name.as_bytes(), lazy);
    if rc == 0 {
        let note = if lazy { " (lazily: files still open keep it until they close)" } else { "" };
        let _ = writeln!(std::io::stdout(), "unmounted {}{note}", show(&target)); // (a closed stdout: not a failure)
    }
    rc
}

fn do_umount_all(c: &Caller, zone: &str) -> u8 {
    let mnt = c.mnt_dir(zone);
    if !std::path::Path::new(OsStr::from_bytes(&mnt)).is_dir() {
        return 0;
    }
    let pfd = c.open_owned(&mnt);
    let prefix = [mnt.as_slice(), b"/"].concat();
    // a name with one trailing newline is one older versions allowed: detached too
    let mounted = || -> Vec<Vec<u8>> {
        mount_points().into_iter()
            .filter_map(|m| m.strip_prefix(prefix.as_slice()).map(<[u8]>::to_vec))
            .filter(|x| {
                let base = x.strip_suffix(b"\n").unwrap_or(x);
                std::str::from_utf8(base).map(policy::mount_name_ok).unwrap_or(false)
            })
            .collect()
    };
    let mut n = 0;
    for _ in 0..256 {
        // a stacked mount shows up again under the same name
        let names = mounted();
        if names.is_empty() {
            break;
        }
        for x in names {
            umount_one(pfd.as_raw_fd(), &[prefix.as_slice(), &x].concat(), &x, true);
            n += 1;
        }
    }
    let left: Vec<String> = mounted().iter().map(|x| show(x)).collect();
    let still = if left.is_empty() { String::new() } else { format!("; still mounted: {}", left.join(", ")) };
    let _ = writeln!(std::io::stdout(), "{n} mount(s) under {} detached{still}", show(&mnt));
    u8::from(!left.is_empty())
}

fn main() -> ExitCode {
    let argv: Vec<OsString> = std::env::args_os().skip(1).collect();
    if argv.is_empty() {
        usage();
    }
    let c = Caller::new();
    let text = |a: &OsString| a.to_str().map(String::from);
    let cmd = text(&argv[0]).unwrap_or_default();
    let rest = &argv[1..];
    let flags: Vec<String> = rest.iter().filter(|a| a.as_bytes().starts_with(b"--")).map(|a| show(a.as_bytes())).collect();
    let args: Vec<&OsString> = rest.iter().filter(|a| !a.as_bytes().starts_with(b"--")).collect();
    let only = |allowed: &[&str]| flags.iter().all(|f| allowed.contains(&f.as_str()));
    // zone and mount names are ASCII by their rules; anything else is refused by them
    let name = |a: &OsString| text(a).unwrap_or_else(|| show(a.as_bytes()));
    let rc = match (cmd.as_str(), args.len()) {
        ("mount", 3) if only(&["--ro"]) => {
            do_mount(&c, &name(args[0]), &name(args[1]), args[2].as_bytes(), flags.iter().any(|f| f == "--ro"))
        }
        ("umount", 2) if only(&["--lazy"]) => do_umount(&c, &name(args[0]), &name(args[1]), flags.iter().any(|f| f == "--lazy")),
        ("umount-all", 1) if flags.is_empty() => do_umount_all(&c, &name(args[0])),
        _ => usage(),
    };
    ExitCode::from(rc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_dir_is_the_executables() {
        let exe = std::env::current_exe().unwrap();
        assert_eq!(code_dir(), exe.parent().unwrap().as_os_str());
    }

    #[test]
    fn mnt_dir_as_appzones_chooses() {
        // ~/.local/share/appzones; the pre-0.6 ~/.local/share/zones only while the new one is not
        // there (a symlink counts as there: chosen, and then refused by open_owned)
        let t = std::env::temp_dir().join(format!("az-mnt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        let c = Caller { uid: 0, gid: 0, name: b"alice".to_vec(), home: t.as_os_str().as_bytes().to_vec() };
        let (new, old) = (t.join(".local/share/appzones"), t.join(".local/share/zones"));
        let mnt = |base: &std::path::Path| [base.as_os_str().as_bytes(), b"/ta/mnt"].concat();
        assert_eq!(c.mnt_dir("ta"), mnt(&new));
        std::fs::create_dir_all(&old).unwrap();
        assert_eq!(c.mnt_dir("ta"), mnt(&old));
        std::os::unix::fs::symlink(&old, &new).unwrap();
        assert_eq!(c.mnt_dir("ta"), mnt(&new));
        std::fs::remove_file(&new).unwrap();
        std::fs::create_dir(&new).unwrap();
        assert_eq!(c.mnt_dir("ta"), mnt(&new));
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn mountinfo_escapes() {
        assert_eq!(unescape(br"/home/u/my\040dir"), b"/home/u/my dir");
        assert_eq!(unescape(br"/a\134b"), b"/a\\b");
        assert_eq!(unescape(br"/a\04"), br"/a\04");
        assert_eq!(unescape(b"/plain"), b"/plain");
    }
}
