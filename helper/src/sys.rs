//! The system calls appzones needs that Linux and macOS spell differently, in one place
//! (design/macos.md). Each one says what it does where the system has no direct match.

use std::os::fd::RawFd;

/// A new unix stream socket, non-blocking and close-on-exec; -1 on failure (errno set).
pub fn unix_stream_nonblock() -> RawFd {
    #[cfg(target_os = "linux")]
    // Safety: socket(2), no pointers.
    unsafe {
        libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC, 0)
    }
    #[cfg(not(target_os = "linux"))]
    // Safety: socket(2) and fcntl(2) on the new descriptor, closed again on failure.
    unsafe {
        let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
        if fd >= 0 && !(set_cloexec_nonblock(fd, true)) {
            libc::close(fd);
            return -1;
        }
        fd
    }
}

#[cfg(not(target_os = "linux"))]
/// FD_CLOEXEC on FD, and O_NONBLOCK when NONBLOCK.
unsafe fn set_cloexec_nonblock(fd: RawFd, nonblock: bool) -> bool {
    // Safety: fcntl on a descriptor the caller owns.
    unsafe {
        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) == 0
            && (!nonblock || libc::fcntl(fd, libc::F_SETFL, libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK) == 0)
    }
}

/// A pipe into FDS, close-on-exec (and non-blocking with NONBLOCK); false on failure.
/// (On macOS the flags are set right after pipe(2): appzones starts no other thread that
/// could fork in between when it makes one.)
pub fn pipe(fds: &mut [RawFd; 2], nonblock: bool) -> bool {
    #[cfg(target_os = "linux")]
    // Safety: pipe2 into the caller's array.
    unsafe {
        libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | if nonblock { libc::O_NONBLOCK } else { 0 }) == 0
    }
    #[cfg(not(target_os = "linux"))]
    // Safety: pipe into the caller's array, then fcntl on the two new descriptors.
    unsafe {
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return false;
        }
        if !(set_cloexec_nonblock(fds[0], nonblock) && set_cloexec_nonblock(fds[1], nonblock)) {
            libc::close(fds[0]);
            libc::close(fds[1]);
            return false;
        }
        true
    }
}

/// In a child: SIGKILL when the parent dies (Linux: PR_SET_PDEATHSIG). macOS has no such
/// thing: nothing here, and the callers' getppid() check after it still catches a parent
/// that died before the fork returned. (Async-signal-safe: for pre_exec and after fork.)
pub fn die_with_parent() {
    #[cfg(target_os = "linux")]
    // Safety: prctl, no pointers.
    unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0);
    }
}

/// A pidfd of PID (Linux), which pins the process: a signal through it never reaches a
/// process that took the pid over. None where there are none (macOS) or on failure.
pub fn pidfd_open(pid: libc::pid_t) -> Option<RawFd> {
    #[cfg(target_os = "linux")]
    {
        // Safety: pidfd_open of a pid; the descriptor is the caller's.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as RawFd;
        (fd >= 0).then_some(fd)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}

/// SIG to the process of the pidfd PFD.
#[cfg(target_os = "linux")]
pub fn pidfd_signal(pfd: RawFd, sig: libc::c_int) -> std::io::Result<()> {
    // Safety: a signal through the caller's pidfd.
    let rc = unsafe { libc::syscall(libc::SYS_pidfd_send_signal, pfd, sig, std::ptr::null::<u8>(), 0) };
    if rc == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
}

/// The path of the open descriptor FD, byte for byte; None when it has no path any more
/// (unlinked) or it is not absolute. Linux: /proc/self/fd; macOS: F_GETPATH, which gives no
/// "(deleted)" mark: there the path counts only while it still leads to this very file
/// (the same device and inode), so a removed or replaced one never passes.
pub fn fd_path(fd: RawFd) -> Option<Vec<u8>> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let t = std::fs::read_link(format!("/proc/self/fd/{fd}")).ok()?;
        let t = t.as_os_str().as_bytes().to_vec();
        (!t.ends_with(b" (deleted)") && t.starts_with(b"/")).then_some(t)
    }
    #[cfg(not(target_os = "linux"))]
    {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::MetadataExt;
        let mut buf = vec![0u8; libc::PATH_MAX as usize];
        // Safety: F_GETPATH writes at most PATH_MAX bytes into our buffer; fstat into a local.
        let (rc, held) = unsafe {
            let rc = libc::fcntl(fd, libc::F_GETPATH, buf.as_mut_ptr());
            let mut st: libc::stat = std::mem::zeroed();
            let held = (libc::fstat(fd, &mut st) == 0).then_some((st.st_dev as u64, st.st_ino as u64, st.st_nlink as u64));
            (rc, held)
        };
        let (dev, ino, links) = held.filter(|_| rc != -1)?;
        let n = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
        buf.truncate(n);
        let at = std::fs::symlink_metadata(std::ffi::OsStr::from_bytes(&buf)).ok()?;
        (links > 0 && buf.starts_with(b"/") && at.dev() == dev && at.ino() == ino).then_some(buf)
    }
}

/// Whether the character device RDEV is /dev/null (its numbers differ between systems).
pub fn is_dev_null(rdev: libc::dev_t) -> bool {
    use std::os::unix::fs::MetadataExt;
    #[allow(clippy::unnecessary_cast)] // (dev_t is u64 on Linux, i32 on macOS)
    let rdev = rdev as u64;
    std::fs::metadata("/dev/null").is_ok_and(|m| m.rdev() == rdev)
}

/// "MAJOR:MINOR" of a device number, as sysfs writes it.
pub fn major_minor(rdev: u64) -> String {
    let d = rdev as libc::dev_t;
    format!("{}:{}", libc::major(d), libc::minor(d))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    #[test]
    fn pipes_and_sockets_close_on_exec() {
        let cloexec = |fd: RawFd| unsafe { libc::fcntl(fd, libc::F_GETFD) } & libc::FD_CLOEXEC != 0;
        let nonblock = |fd: RawFd| unsafe { libc::fcntl(fd, libc::F_GETFL) } & libc::O_NONBLOCK != 0;
        let mut p = [-1; 2];
        assert!(pipe(&mut p, true));
        assert!(cloexec(p[0]) && cloexec(p[1]) && nonblock(p[0]) && nonblock(p[1]));
        let s = unix_stream_nonblock();
        assert!(s >= 0 && cloexec(s) && nonblock(s));
        for fd in [p[0], p[1], s] {
            // Safety: our own descriptors.
            unsafe { libc::close(fd) };
        }
    }

    #[test]
    fn descriptor_paths() {
        let dir = std::fs::canonicalize(std::env::temp_dir()).unwrap().join(format!("az-sys-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("f");
        std::fs::write(&f, "x").unwrap();
        let file = std::fs::File::open(&f).unwrap();
        use std::os::unix::ffi::OsStrExt;
        assert_eq!(fd_path(file.as_raw_fd()).as_deref(), Some(f.as_os_str().as_bytes()));
        std::fs::remove_file(&f).unwrap(); // unlinked: no path any more
        assert_eq!(fd_path(file.as_raw_fd()), None);
        let _ = std::fs::remove_dir_all(&dir);
        use std::os::unix::fs::MetadataExt;
        assert!(is_dev_null(std::fs::metadata("/dev/null").unwrap().rdev() as libc::dev_t));
    }
}
