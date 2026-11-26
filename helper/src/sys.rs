//! The system calls appzones needs that Linux and macOS spell differently, in one place
//! (design/macos.md). Each one says what it does where the system has no direct match.

use std::os::fd::RawFd;

/// A new unix stream socket, non-blocking and close-on-exec; -1 on failure (errno set).
pub fn unix_stream_nonblock() -> RawFd {
    #[cfg(target_os = "linux")]
    // Safety: socket(2), no pointers.
    unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
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
            && (!nonblock
                || libc::fcntl(
                    fd,
                    libc::F_SETFL,
                    libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK,
                ) == 0)
    }
}

/// A pipe into FDS, close-on-exec (and non-blocking with NONBLOCK); false on failure.
/// (On macOS the flags are set right after pipe(2): appzones starts no other thread that
/// could fork in between when it makes one.)
pub fn pipe(fds: &mut [RawFd; 2], nonblock: bool) -> bool {
    #[cfg(target_os = "linux")]
    // Safety: pipe2 into the caller's array.
    unsafe {
        libc::pipe2(
            fds.as_mut_ptr(),
            libc::O_CLOEXEC | if nonblock { libc::O_NONBLOCK } else { 0 },
        ) == 0
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
    let rc = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            pfd,
            sig,
            std::ptr::null::<u8>(),
            0,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
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
            let held = (libc::fstat(fd, &mut st) == 0).then_some((
                st.st_dev as u64,
                st.st_ino as u64,
                st.st_nlink as u64,
            ));
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

/// accept(2) of a new connection on the listening socket FD, close-on-exec; -1 on failure.
pub fn accept_cloexec(fd: RawFd) -> RawFd {
    #[cfg(target_os = "linux")]
    // Safety: accept4 returns a new descriptor (or -1); no addresses asked for.
    unsafe {
        libc::accept4(
            fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            libc::SOCK_CLOEXEC,
        )
    }
    #[cfg(not(target_os = "linux"))]
    // Safety: accept, then fcntl on the new descriptor (closed again on failure).
    unsafe {
        let c = libc::accept(fd, std::ptr::null_mut(), std::ptr::null_mut());
        if c >= 0 && !set_cloexec_nonblock(c, false) {
            libc::close(c);
            return -1;
        }
        c
    }
}

/// The processes of this user other than this one (Linux: /proc; macOS: libproc).
pub fn my_pids() -> Vec<libc::pid_t> {
    // Safety: getuid and getpid have no preconditions.
    let (uid, me) = unsafe { (libc::getuid(), libc::getpid()) };
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::read_dir("/proc")
            .map(|it| {
                it.filter_map(|e| e.ok())
                    .filter_map(|e| {
                        let n: libc::pid_t = e.file_name().to_str()?.parse().ok()?;
                        (n != me && std::fs::metadata(e.path()).ok()?.uid() == uid).then_some(n)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
    #[cfg(not(target_os = "linux"))]
    {
        all_pids()
            .into_iter()
            .filter(|p| *p != me && proc_uid(*p) == Some(uid))
            .collect()
    }
}

/// Every pid on the system (macOS), as libproc lists them.
#[cfg(not(target_os = "linux"))]
fn all_pids() -> Vec<libc::pid_t> {
    // Safety: proc_listallpids into a buffer of the size we say (asked for first, with room
    // for processes started in between).
    unsafe {
        let n = libc::proc_listallpids(std::ptr::null_mut(), 0);
        if n <= 0 {
            return vec![];
        }
        let mut pids = vec![0 as libc::pid_t; n as usize + 64];
        let size = (pids.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int;
        let got = libc::proc_listallpids(pids.as_mut_ptr().cast(), size);
        pids.truncate(got.max(0) as usize);
        pids.retain(|p| *p > 0);
        pids
    }
}

/// The real uid and parent of PID (macOS), None when it is gone or not to be seen.
#[cfg(not(target_os = "linux"))]
fn short_info(pid: libc::pid_t) -> Option<libc::proc_bsdshortinfo> {
    // Safety: proc_pidinfo writes at most the size we pass into a zeroed local.
    unsafe {
        let mut si: libc::proc_bsdshortinfo = std::mem::zeroed();
        let size = std::mem::size_of::<libc::proc_bsdshortinfo>() as libc::c_int;
        (libc::proc_pidinfo(
            pid,
            libc::PROC_PIDT_SHORTBSDINFO,
            0,
            (&mut si as *mut libc::proc_bsdshortinfo).cast(),
            size,
        ) == size)
            .then_some(si)
    }
}

#[cfg(not(target_os = "linux"))]
fn proc_uid(pid: libc::pid_t) -> Option<libc::uid_t> {
    short_info(pid).map(|si| si.pbsi_uid)
}

/// The command line of PID, None when it cannot be read (gone, another user's, or one
/// the sandbox hides). Linux: /proc/PID/cmdline; macOS: sysctl KERN_PROCARGS2.
#[cfg_attr(target_os = "linux", allow(dead_code))] // (macOS: a lock's holder, a zone's ps; Linux: its test)
pub fn proc_argv(pid: libc::pid_t) -> Option<Vec<String>> {
    #[cfg(target_os = "linux")]
    {
        let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        let raw = raw.strip_suffix(b"\0").unwrap_or(&raw);
        Some(
            raw.split(|b| *b == 0)
                .map(|a| String::from_utf8_lossy(a).into_owned())
                .collect(),
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        let mut size: libc::size_t = 0;
        // Safety: sysctl asked for the size first, then into a buffer of that size.
        let buf = unsafe {
            if libc::sysctl(
                mib.as_mut_ptr(),
                3,
                std::ptr::null_mut(),
                &mut size,
                std::ptr::null_mut(),
                0,
            ) != 0
            {
                return None;
            }
            let mut buf = vec![0u8; size];
            if libc::sysctl(
                mib.as_mut_ptr(),
                3,
                buf.as_mut_ptr().cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            ) != 0
            {
                return None;
            }
            buf.truncate(size);
            buf
        };
        procargs2_argv(&buf)
    }
}

/// The argv in a KERN_PROCARGS2 buffer: argc, the executable's path, NULs, then argc
/// strings (the environment after them is not read).
#[cfg_attr(target_os = "linux", allow(dead_code))]
fn procargs2_argv(buf: &[u8]) -> Option<Vec<String>> {
    let argc = i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?);
    let mut rest = &buf[4..];
    let end = rest.iter().position(|b| *b == 0)?; // the executable's path
    rest = &rest[end..];
    let start = rest.iter().position(|b| *b != 0)?;
    rest = &rest[start..];
    let mut argv = Vec::new();
    for _ in 0..argc.max(0) {
        let end = rest.iter().position(|b| *b == 0).unwrap_or(rest.len());
        argv.push(String::from_utf8_lossy(&rest[..end]).into_owned());
        rest = rest.get(end + 1..).unwrap_or(&[]);
    }
    Some(argv)
}

/// Whether process PID has the file FILE (device, inode) open. Linux: /proc/PID/fd; macOS:
/// libproc's list of its descriptors.
pub fn has_open(pid: libc::pid_t, file: (u64, u64)) -> bool {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        let Ok(it) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
            return false;
        };
        it.filter_map(|e| e.ok())
            .any(|e| std::fs::metadata(e.path()).is_ok_and(|m| (m.dev(), m.ino()) == file))
    }
    #[cfg(not(target_os = "linux"))]
    {
        // sys/proc_info.h: PROC_PIDFDVNODEPATHINFO's answer (libc has its second half)
        #[repr(C)]
        struct ProcFileinfo {
            fi_openflags: u32,
            fi_status: u32,
            fi_offset: libc::off_t,
            fi_type: i32,
            fi_guardflags: u32,
        }
        #[repr(C)]
        struct VnodeFdinfoWithPath {
            pfi: ProcFileinfo,
            pvip: libc::vnode_info_path,
        }
        const PROC_PIDFDVNODEPATHINFO: libc::c_int = 2;
        let one = std::mem::size_of::<libc::proc_fdinfo>();
        // Safety: proc_pidinfo asked for the size first, then into a buffer of that size;
        // proc_pidfdinfo writes at most the size we pass into a zeroed local.
        unsafe {
            let size = libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0);
            if size <= 0 {
                return false;
            }
            let mut fds: Vec<libc::proc_fdinfo> = Vec::with_capacity(size as usize / one + 16);
            let got = libc::proc_pidinfo(
                pid,
                libc::PROC_PIDLISTFDS,
                0,
                fds.as_mut_ptr().cast(),
                (fds.capacity() * one) as libc::c_int,
            );
            if got <= 0 {
                return false;
            }
            fds.set_len(got as usize / one);
            fds.iter()
                .filter(|f| f.proc_fdtype == libc::PROX_FDTYPE_VNODE as u32)
                .any(|f| {
                    let mut v: VnodeFdinfoWithPath = std::mem::zeroed();
                    let n = std::mem::size_of::<VnodeFdinfoWithPath>() as libc::c_int;
                    libc::proc_pidfdinfo(
                        pid,
                        f.proc_fd,
                        PROC_PIDFDVNODEPATHINFO,
                        (&mut v as *mut VnodeFdinfoWithPath).cast(),
                        n,
                    ) == n
                        && (
                            v.pvip.vip_vi.vi_stat.vst_dev as u64,
                            v.pvip.vip_vi.vi_stat.vst_ino,
                        ) == file
                })
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod seatbelt_ffi {
    // libsystem_sandbox (part of libSystem): not in Apple's public headers, but stable and
    // what Apple's own tools and others (Chromium, Codex) use to ask about a process
    unsafe extern "C" {
        pub fn sandbox_check(
            pid: libc::pid_t,
            operation: *const libc::c_char,
            filter: libc::c_int,
            ...
        ) -> libc::c_int;
    }
    pub const FILTER_NONE: libc::c_int = 0;
    pub const FILTER_PATH: libc::c_int = 1;
    pub const CHECK_NO_REPORT: libc::c_int = 0x4000_0000;
}

/// Whether PID runs under a Seatbelt profile (macOS); false where there is none.
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub fn sandboxed(pid: libc::pid_t) -> bool {
    #[cfg(target_os = "linux")]
    {
        let _ = pid;
        false
    }
    #[cfg(not(target_os = "linux"))]
    // Safety: sandbox_check with no operation only asks whether PID has a profile.
    unsafe {
        seatbelt_ffi::sandbox_check(pid, std::ptr::null(), seatbelt_ffi::FILTER_NONE) == 1
    }
}

/// Whether PID's sandbox lets it read the existing file or directory PATH (macOS): Some(true)
/// or Some(false), None when it cannot be told (no such process, a path that is not text).
#[cfg(not(target_os = "linux"))]
pub fn sandbox_may_read(pid: libc::pid_t, path: &std::path::Path) -> Option<bool> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // Safety: sandbox_check with NUL-terminated strings we own; a path filter takes one.
    let r = unsafe {
        seatbelt_ffi::sandbox_check(
            pid,
            c"file-read-data".as_ptr(),
            seatbelt_ffi::FILTER_PATH | seatbelt_ffi::CHECK_NO_REPORT,
            c.as_ptr(),
        )
    };
    match r {
        0 => Some(true),
        1 => Some(false),
        _ => None,
    }
}

/// The processes in a zone's Seatbelt sandbox (macOS): sandboxed, allowed to read YES and
/// refused NO, two files side by side with a random name of the zone's own, which only its
/// profile names (one allowed, one refused, by their exact paths): a profile that reads
/// everything (as other tools' do) is allowed both, one that reads nothing of the user's
/// is refused both. Orphans included: a sandbox is inherited by everything a zone process
/// starts, and cannot be left. (Never by paths that other profiles may well treat the
/// same way, as a folder of the home allowed and one in ~/Library refused: Apple's own
/// agents do that.)
#[cfg(not(target_os = "linux"))]
pub fn zone_pids(yes: &std::path::Path, no: &std::path::Path) -> Vec<libc::pid_t> {
    // a mark removed while the zone runs (sandbox_check calls a missing file refused, so its
    // processes would no longer be found): made again at the same path, which the running
    // profile names, so they are
    for m in [yes, no] {
        if std::fs::symlink_metadata(m).is_err() {
            use std::os::unix::fs::OpenOptionsExt;
            let _ = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(m);
        }
    }
    if !(yes.is_file() && no.is_file()) {
        return vec![]; // (not files, and not to be made: nothing can be told, never a guess)
    }
    my_pids()
        .into_iter()
        .filter(|p| {
            sandboxed(*p)
                && sandbox_may_read(*p, yes) == Some(true)
                && sandbox_may_read(*p, no) == Some(false)
        })
        .collect()
}

/// The children of PID (macOS).
#[cfg(not(target_os = "linux"))]
pub fn children(pid: libc::pid_t) -> Vec<libc::pid_t> {
    my_pids()
        .into_iter()
        .filter(|p| short_info(*p).is_some_and(|si| si.pbsi_ppid as libc::pid_t == pid))
        .collect()
}

/// SIGKILL to every process of the zone (zone_pids), until none is left or two seconds
/// passed: a process that forks while it is swept is found in the next pass. The number
/// of processes killed.
#[cfg(not(target_os = "linux"))]
pub fn kill_zone(yes: &std::path::Path, no: &std::path::Path) -> usize {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut killed = std::collections::HashSet::new();
    loop {
        let pids = zone_pids(yes, no);
        if pids.is_empty() || std::time::Instant::now() > end {
            return killed.len();
        }
        for p in pids {
            // Safety: kill(2) of a process just found in the zone's sandbox.
            if unsafe { libc::kill(p, libc::SIGKILL) } == 0 {
                killed.insert(p);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    #[test]
    fn pipes_and_sockets_close_on_exec() {
        let cloexec = |fd: RawFd| unsafe { libc::fcntl(fd, libc::F_GETFD) } & libc::FD_CLOEXEC != 0;
        let nonblock =
            |fd: RawFd| unsafe { libc::fcntl(fd, libc::F_GETFL) } & libc::O_NONBLOCK != 0;
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
    fn procargs2() {
        let mut b = 3i32.to_ne_bytes().to_vec();
        b.extend_from_slice(b"/bin/sh\0\0\0\0sh\0-c\0echo a\0HOME=/x\0");
        assert_eq!(
            procargs2_argv(&b),
            Some(vec!["sh".into(), "-c".into(), "echo a".into()])
        );
        assert_eq!(procargs2_argv(&[1, 0]), None);
    }

    #[test]
    fn my_processes() {
        // a child of ours is listed, this process is not; and its command line is read
        let mut c = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .unwrap();
        let pid = c.id() as libc::pid_t;
        let pids = my_pids();
        assert!(pids.contains(&pid) && !pids.contains(&(std::process::id() as libc::pid_t)));
        let end = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while proc_argv(pid).is_none_or(|a| a.len() != 2) && std::time::Instant::now() < end {
            std::thread::sleep(std::time::Duration::from_millis(10)); // (until it has exec'd)
        }
        assert_eq!(
            proc_argv(pid).map(|a| a[1..].to_vec()),
            Some(vec!["5".to_string()])
        );
        // (as sandboxed as this test: Nix's builds on macOS run under a profile of their own)
        assert_eq!(sandboxed(pid), sandboxed(std::process::id() as libc::pid_t));
        // a file this process holds open is seen, one it does not is not
        let dir = std::env::temp_dir().join(format!("az-open-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let f = std::fs::File::create(dir.join("held")).unwrap();
        std::fs::write(dir.join("other"), "").unwrap();
        use std::os::unix::fs::MetadataExt;
        let id = |p: &str| {
            std::fs::metadata(dir.join(p))
                .map(|m| (m.dev(), m.ino()))
                .unwrap()
        };
        let me = std::process::id() as libc::pid_t;
        assert!(has_open(me, id("held")) && !has_open(me, id("other")));
        drop(f);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = c.kill();
        let _ = c.wait();
    }

    #[test]
    fn descriptor_paths() {
        let dir = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("az-sys-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("f");
        std::fs::write(&f, "x").unwrap();
        let file = std::fs::File::open(&f).unwrap();
        use std::os::unix::ffi::OsStrExt;
        assert_eq!(
            fd_path(file.as_raw_fd()).as_deref(),
            Some(f.as_os_str().as_bytes())
        );
        std::fs::remove_file(&f).unwrap(); // unlinked: no path any more
        assert_eq!(fd_path(file.as_raw_fd()), None);
        let _ = std::fs::remove_dir_all(&dir);
        use std::os::unix::fs::MetadataExt;
        assert!(is_dev_null(
            std::fs::metadata("/dev/null").unwrap().rdev() as libc::dev_t
        ));
    }
}
