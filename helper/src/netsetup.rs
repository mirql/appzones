//! `appzones _netsetup RULES INFO ZONE -- CMD`: between pasta and bwrap, in the zone's own
//! network namespace (the port of server.netsetup): the zone's output to its server's log
//! socket, the nft deny list loaded, port 53 allowed for zone-init's DNS, bwrap's info
//! file as fd 3, zone-init's control socket made on the host side, the server's pidfd
//! received, every bind pinned (opened and checked here, mounted by descriptor), then exec.

use std::ffi::CString;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::{Duration, Instant};

use crate::plan::{Host, PIN};

fn die(msg: &str) -> ! {
    eprintln!("appzones: {msg}");
    std::process::exit(1)
}

fn inheritable(fd: RawFd) {
    // Safety: flags on a descriptor we own.
    unsafe {
        let f = libc::fcntl(fd, libc::F_GETFD);
        if f >= 0 {
            libc::fcntl(fd, libc::F_SETFD, f & !libc::FD_CLOEXEC);
        }
    }
}

/// zone-init's control socket, made on the host side (a path outside everything bound into
/// the zone) and handed over as an inherited fd.
fn agent_listener(host: &Host, name: &str) -> RawFd {
    let path = crate::agent::agent_path(&host.rt, name).unwrap_or_else(|e| die(&e));
    let _ = std::fs::remove_file(&path);
    // Safety: umask around the bind, so the socket is 0600 from the start.
    let old = unsafe { libc::umask(0o177) };
    let l = UnixListener::bind(&path);
    // Safety: restoring it.
    unsafe { libc::umask(old) };
    let l = l.unwrap_or_else(|e| die(&format!("{}: {e}", path.display())));
    // Safety: listen on our socket with Python's backlog.
    unsafe { libc::listen(l.as_raw_fd(), 16) };
    let fd = l.into_raw_fd();
    inheritable(fd);
    fd
}

/// The server's pidfd (server.offer_pidfd's other side), for zone-init ($ZONE_SERVER_FD).
fn receive_pidfd(host: &Host, name: &str, timeout: f64) -> RawFd {
    let path = host.rt.join(name).join("srv.sock");
    let end = Instant::now() + Duration::from_secs_f64(timeout);
    let s = loop {
        // (never a blocking connect: a full queue would wait past the deadline)
        let left = end.saturating_duration_since(Instant::now()).max(Duration::from_millis(1));
        match crate::agent::connect_within(&path, left) {
            Ok(s) => break s,
            Err(_) if Instant::now() <= end => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => die(&format!("zone {name}: no pidfd from its server; not starting the zone")),
        }
    };
    let _ = s.set_read_timeout(Some(end.saturating_duration_since(Instant::now()).max(Duration::from_millis(1))));
    let bad = || -> ! { die(&format!("zone {name}: no valid pidfd from its server; not starting the zone")) };
    // the peer must be this user
    let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // Safety: SO_PEERCRED into a ucred of the given size.
    let rc = unsafe { libc::getsockopt(s.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED,
                                       (&mut cred as *mut libc::ucred).cast(), &mut len) };
    let mut buf = [0u8; 16];
    let mut iov = libc::iovec { iov_base: buf.as_mut_ptr().cast(), iov_len: buf.len() };
    let mut cmsg = [0u64; 8];
    let mut fds = Vec::new();
    // Safety: recvmsg into our buffers; the descriptors in SCM_RIGHTS are new and ours.
    let n = unsafe {
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cmsg.as_mut_ptr().cast();
        msg.msg_controllen = std::mem::size_of_val(&cmsg) as _;
        let n = libc::recvmsg(s.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC);
        if n >= 0 {
            let mut c = libc::CMSG_FIRSTHDR(&msg);
            while !c.is_null() {
                if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                    let data = libc::CMSG_DATA(c).cast::<RawFd>();
                    let count = ((*c).cmsg_len as usize - libc::CMSG_LEN(0) as usize) / std::mem::size_of::<RawFd>();
                    for i in 0..count {
                        fds.push(std::ptr::read_unaligned(data.add(i)));
                    }
                }
                c = libc::CMSG_NXTHDR(&msg, c);
            }
        }
        n
    };
    // Safety: getuid has no preconditions.
    let me = unsafe { libc::getuid() };
    if rc != 0 || cred.uid != me || n < 0 || &buf[..n.max(0) as usize] != b"pidfd" || fds.len() != 1 {
        for f in fds {
            // Safety: closing descriptors we received.
            unsafe { libc::close(f) };
        }
        bad();
    }
    inheritable(fds[0]);
    fds[0]
}

/// Every PIN group replaced by --bind-fd/--ro-bind-fd on a descriptor opened and checked
/// here: the policy runs on the object actually opened, and bwrap mounts that object.
fn pin_binds(argv: Vec<String>, code_dir: std::ffi::OsString) -> Vec<String> {
    pin_binds_by(argv, &crate::config::policy(code_dir)).unwrap_or_else(|e| die(&e))
}

/// Each PIN MODE RAW DEST in ARGV: RAW opened (O_PATH), checked on what was opened, and
/// handed to bwrap as its descriptor; the error when one is refused.
fn pin_binds_by(argv: Vec<String>, policy: &crate::policy::Policy) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        if argv[i] != PIN || i + 3 >= argv.len() {
            out.push(argv[i].clone());
            i += 1;
            continue;
        }
        let (mode, raw, dest) = (&argv[i + 1], &argv[i + 2], &argv[i + 3]);
        i += 4;
        let expanded = policy.expand(raw.as_bytes());
        let c = CString::new(expanded).map_err(|_| format!("bind {raw}: a NUL byte; refusing to start the zone"))?;
        // Safety: open a NUL-terminated path, O_PATH: nothing is read through it.
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
        if fd < 0 {
            let e = std::io::Error::last_os_error();
            let text = e.to_string();
            let msg = text.split(" (os error").next().unwrap_or(&text).to_string();
            return Err(format!("bind {raw}: {msg}; refusing to start the zone"));
        }
        // Safety: a descriptor we just opened.
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        let real = std::fs::read_link(format!("/proc/self/fd/{fd}")).ok()
            .map(|t| t.as_os_str().as_bytes().to_vec())
            .filter(|t| !t.ends_with(b" (deleted)") && t.starts_with(b"/"));
        let Some(real) = real else {
            return Err(format!("bind {raw}: was removed or replaced while the zone started; refusing to start the zone"));
        };
        let meta = file.metadata().map_err(|e| format!("bind {raw}: {e}; refusing to start the zone"))?;
        let err = policy.policy_error(&real, raw.as_bytes())
            .or_else(|| crate::policy::Policy::kind_error(&meta, raw));
        if let Some(err) = err {
            return Err(format!("bind {err}; refusing to start the zone"));
        }
        let fd = file.into_raw_fd();
        inheritable(fd);
        out.extend([if mode == "rw" { "--bind-fd".to_string() } else { "--ro-bind-fd".to_string() }, fd.to_string(),
                    dest.clone()]);
    }
    Ok(out)
}

pub fn main(host: &Host, rules: &str, info: &str, name: &str, argv: Vec<String>) -> ! {
    if let Some(log) = std::env::var_os("ZONE_LOG_SOCK") {
        std::env::remove_var("ZONE_LOG_SOCK");
        if let Ok(s) = UnixStream::connect(&log) {
            // Safety: our stdout and stderr become the server's log socket.
            unsafe {
                libc::dup2(s.as_raw_fd(), 1);
                libc::dup2(s.as_raw_fd(), 2);
            }
        }
    }
    let timeout = std::env::var("ZONE_START_TIMEOUT").ok().and_then(|t| t.parse::<f64>().ok()).unwrap_or(30.0);
    std::env::remove_var("ZONE_START_TIMEOUT");
    if rules != "-" {
        let nft = host.tool("nft").unwrap_or_else(|e| die(&e));
        let ok = std::process::Command::new(nft).args(["-f", rules]).status().is_ok_and(|s| s.success());
        if !ok {
            die("nft rules failed to load; refusing to start the zone without them");
        }
        // zone-init, without privileges, answers the zone's DNS on port 53
        if let Err(e) = std::fs::write("/proc/sys/net/ipv4/ip_unprivileged_port_start", "53") {
            let text = e.to_string();
            let msg = text.split(" (os error").next().unwrap_or(&text);
            let mut out = std::io::stdout();
            let _ = writeln!(out, "[zone] {name}: cannot allow port 53 in the zone's network ({msg}); names will not resolve");
            let _ = out.flush();
        }
    }
    let c = CString::new(info).unwrap_or_else(|_| die("a NUL byte in the info path"));
    // Safety: open a NUL-terminated path; the descriptor becomes fd 3.
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_CLOEXEC, 0o600) };
    if fd < 0 {
        die(&format!("{info}: {}", std::io::Error::last_os_error()));
    }
    if fd != 3 {
        // Safety: fd 3 is ours to replace (nothing else of ours is there).
        unsafe {
            libc::dup2(fd, 3);
            libc::close(fd);
        }
    }
    inheritable(3);
    let ctl = agent_listener(host, name);
    let srv = receive_pidfd(host, name, timeout);
    let mut argv = argv;
    if let Some(i) = argv.iter().position(|a| a == "--") {
        let extra = ["--setenv".to_string(), "ZONE_CTL_FD".into(), ctl.to_string(), "--setenv".into(),
                     "ZONE_SERVER_FD".into(), srv.to_string()];
        argv.splice(i..i, extra);
    }
    let argv = pin_binds(argv, host.here.as_os_str().to_owned());
    let Some(first) = argv.first() else { die("nothing to run") };
    // bwrap gets SIGKILL when its parent (pasta, or the server) dies: the zone ends with
    // its server however zone-init fares (this survives the exec)
    // Safety: prctl and getppid, no memory involved.
    unsafe {
        let parent = libc::getppid();
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0);
        if libc::getppid() != parent {
            die("the zone's parent ended while it started");
        }
    }
    let err = std::os::unix::process::CommandExt::exec(std::process::Command::new(first).args(&argv[1..]));
    die(&format!("{first}: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins() {
        // the last check, on the descriptor handed to bwrap
        let t = std::env::temp_dir().join(format!("az-pin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        let h = t.join("home");
        for d in [".ssh", "Docs/proj"] {
            std::fs::create_dir_all(h.join(d)).unwrap();
        }
        std::fs::write(t.join("denied-paths"), "").unwrap();
        let h = std::fs::canonicalize(&h).unwrap();
        let policy = crate::policy::Policy { home: h.clone().into(), code_dir: t.join("code").into(),
                                             denied_file: Some(t.join("denied-paths").into()), state_dir: None, extra: vec![] };
        let a = |x: &[&str]| x.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let out = pin_binds_by(a(&["bwrap", PIN, "ro", "~/Docs/proj", "/home/x/Docs/proj", "--", "true"]), &policy).unwrap();
        assert_eq!(out[..2], a(&["bwrap", "--ro-bind-fd"]));
        assert_eq!(std::fs::read_link(format!("/proc/self/fd/{}", out[2])).unwrap(), h.join("Docs/proj"));
        assert_eq!(out[3..], a(&["/home/x/Docs/proj", "--", "true"]));
        // Safety: the descriptor pin_binds opened for us.
        unsafe { libc::close(out[2].parse().unwrap()) };
        // a symlink swapped in after validation: refused on what was opened
        std::fs::remove_dir(h.join("Docs/proj")).unwrap();
        std::os::unix::fs::symlink(h.join(".ssh"), h.join("Docs/proj")).unwrap();
        let err = pin_binds_by(a(&["bwrap", PIN, "rw", "~/Docs/proj", "/x"]), &policy).unwrap_err();
        assert!(err.contains(".ssh") && err.ends_with("refusing to start the zone"), "{err}");
        let err = pin_binds_by(a(&["bwrap", PIN, "rw", "~/Docs/gone", "/x"]), &policy).unwrap_err();
        assert!(err.contains("No such file"), "{err}");
        let _ = std::fs::remove_dir_all(&t);
    }
}
