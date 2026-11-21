//! `appzones-helper serve --plan FILE --lock-fd L [--notify-fd N]`: the part of `appzones
//! serve` that holds a zone, the port of run_plan in src/appzones/server.py (the reference:
//! the same steps, messages and exit statuses).
//!
//! `appzones serve` (Python) checks the configuration, writes the zone's files and its plan
//! (plan.json in the zone's runtime directory: make_plan), then execs this with the zone's
//! lock and the readiness descriptor. From the plan alone, with no configuration to read,
//! this starts the helpers (zone-wayland, the D-Bus proxy), the zone (pasta, _netsetup,
//! bwrap, zone-init), hands it its server's pidfd, waits for zone-init, says the zone is
//! ready, starts its autostart apps, relays its output and DNS, handles its links and
//! linger, and ends with it: helpers stopped, the zone's runtime mounts detached.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixDatagram, UnixListener, UnixStream};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::host_dns;

const PLAN_VERSION: u64 = 1;
// the marks in a plan's command line (src/appzones/fs.py): NUL is never in an argument
pub const PLAN_IF: &str = "\0if:";
pub const PLAN_END: &str = "\0end";
pub const PLAN_HOST_WAYLAND: &str = "\0host-wayland";
const WAYLAND_NO_PROTOCOL: i32 = 3;
const USAGE: &str = "usage: appzones-helper serve (--plan FILE | --zone NAME) --lock-fd L \
                     [--notify-fd N]";

/// The last stop asked for (SIGTERM or SIGINT), 0 for none.
static STOP: AtomicI32 = AtomicI32::new(0);

extern "C" fn on_stop(sig: libc::c_int) {
    STOP.store(sig, Ordering::SeqCst); // only an atomic store: async-signal-safe
}

fn stop() -> i32 {
    STOP.load(Ordering::SeqCst)
}

/// A stop (SIGTERM, SIGINT) was asked for.
pub fn stopped() -> bool {
    stop() != 0
}

fn say(msg: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{msg}"); // the log or the journal; a closed stdout is no reason to fail
    let _ = out.flush();
}

/// ARGV with each marked group kept when its condition is in HAVE, else dropped, and the
/// host's Wayland socket filled in (fs.resolve_plan_argv).
pub fn resolve_plan_argv(argv: &[String], have: &HashSet<&str>, host_wayland: Option<&str>) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut keep = true;
    for a in argv {
        if let Some(cond) = a.strip_prefix(PLAN_IF) {
            keep = have.contains(cond);
        } else if a == PLAN_END {
            keep = true;
        } else if keep {
            if a == PLAN_HOST_WAYLAND {
                out.push(host_wayland.ok_or("the plan uses the host's Wayland socket, but there is none")?.to_string());
            } else {
                out.push(a.clone());
            }
        }
    }
    Ok(out)
}

/// Only an http(s) URL, short, without line breaks or NUL, leaves a zone as a link
/// (portals.link_ok).
pub fn link_ok(url: &str) -> bool {
    (url.starts_with("http://") || url.starts_with("https://")) && url.chars().count() < 8000
        && !url.contains(['\n', '\r', '\0'])
}

fn host_rt() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        // Safety: getuid has no preconditions.
        _ => PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })),
    }
}

/// The host compositor's socket ($WAYLAND_DISPLAY: a name in $XDG_RUNTIME_DIR, or a path),
/// resolved, when it is a socket (common.host_wayland_socket).
pub fn host_wayland_socket() -> Option<String> {
    host_wayland_socket_for(std::env::var_os("WAYLAND_DISPLAY"), &host_rt())
}

/// host_wayland_socket for DISPLAY in the runtime directory RT.
fn host_wayland_socket_for(display: Option<std::ffi::OsString>, rt: &Path) -> Option<String> {
    let d = display.filter(|d| !d.is_empty())?;
    let p = if d.to_string_lossy().starts_with('/') { PathBuf::from(d) } else { rt.join(d) };
    let real = std::fs::canonicalize(p).ok()?;
    let ok = std::fs::metadata(&real).ok()?.file_type().is_socket();
    ok.then(|| real.to_string_lossy().into_owned())
}

/// Python's returncode of an exit status: the code, or minus the signal.
fn returncode(st: ExitStatus) -> i32 {
    st.code().unwrap_or_else(|| -st.signal().unwrap_or(1))
}

fn term(child: &Child) {
    // Safety: a signal to our own child, by its pid.
    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
}

fn running(child: &mut Child) -> bool {
    matches!(child.try_wait(), Ok(None))
}

/// Wait for CHILD, SIGKILL it when it has not exited after TIMEOUT (server.wait_or_kill).
fn wait_or_kill(child: &mut Child, timeout: Duration) -> i32 {
    let end = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(st)) => return returncode(st),
            Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                return child.wait().map(returncode).unwrap_or(1);
            }
        }
    }
}

/// CHILD's returncode once it has exited within TIMEOUT, else None (server._exit_status).
fn exit_status(child: &mut Child, timeout: Duration) -> Option<i32> {
    let end = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(st)) => return Some(returncode(st)),
            Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(20)),
            _ => return None,
        }
    }
}

/// One line from FD within TIMEOUT (without its newline), None if not, or on a stop.
fn read_line(fd: RawFd, timeout: Duration) -> Option<Vec<u8>> {
    let end = Instant::now() + timeout;
    let mut buf = Vec::new();
    while !buf.contains(&b'\n') {
        if stop() != 0 {
            return None;
        }
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        let ms = left.as_millis().min(100) as libc::c_int + 1;
        // Safety: poll on one descriptor we own.
        if unsafe { libc::poll(&mut pfd, 1, ms) } <= 0 {
            continue;
        }
        let mut chunk = [0u8; 256];
        // Safety: read into our buffer.
        let n = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), chunk.len()) };
        if n <= 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n as usize]);
    }
    let line = buf.split(|&b| b == b'\n').next().unwrap_or_default();
    Some(line.trim_ascii().to_vec())
}

fn set_cloexec(fd: RawFd) {
    // Safety: flags on a descriptor we were given.
    unsafe {
        let f = libc::fcntl(fd, libc::F_GETFD);
        if f >= 0 {
            libc::fcntl(fd, libc::F_SETFD, f | libc::FD_CLOEXEC);
        }
    }
}

/// Send DATA with descriptor FD over the stream SOCK (SCM_RIGHTS).
fn send_fd(sock: &UnixStream, data: &[u8], fd: RawFd) -> std::io::Result<()> {
    let mut iov = libc::iovec { iov_base: data.as_ptr() as *mut _, iov_len: data.len() };
    let mut cmsg = [0u64; 4]; // CMSG_SPACE(sizeof(int)), aligned
    // Safety: a msghdr over our buffers with one SCM_RIGHTS message for one descriptor.
    let n = unsafe {
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cmsg.as_mut_ptr().cast();
        msg.msg_controllen = libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) as _;
        let c = libc::CMSG_FIRSTHDR(&msg);
        (*c).cmsg_level = libc::SOL_SOCKET;
        (*c).cmsg_type = libc::SCM_RIGHTS;
        (*c).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32) as _;
        std::ptr::write_unaligned(libc::CMSG_DATA(c).cast::<RawFd>(), fd);
        loop {
            let n = libc::sendmsg(sock.as_raw_fd(), &msg, libc::MSG_NOSIGNAL);
            if n >= 0 || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                break n;
            }
        }
    };
    if n < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut w = sock;
    w.write_all(&data[n as usize..]) // (the rest, without the descriptor again)
}

/// One request to zone-init (the agent protocol, a JSON line each way); None when it does
/// not answer.
fn agent(zrt: &Path, req: &Value, timeout: Duration) -> Option<Value> {
    let mut s = connect_now(&zrt.join("agent.sock"))?;
    s.set_read_timeout(Some(timeout)).ok()?;
    s.set_write_timeout(Some(timeout)).ok()?;
    s.write_all(format!("{req}\n").as_bytes()).ok()?;
    // one line, within TIMEOUT in all and 8 MiB: a zone-init that drips bytes or never ends
    // its line does not hold the server
    let end = Instant::now() + timeout;
    let mut buf = Vec::new();
    while !buf.contains(&b'\n') {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() || buf.len() > 8 << 20 {
            return None;
        }
        s.set_read_timeout(Some(left)).ok()?;
        let mut chunk = [0u8; 65536];
        match s.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
    }
    let line = String::from_utf8_lossy(&buf[..buf.iter().position(|b| *b == b'\n').unwrap_or(buf.len())]).into_owned();
    if line.trim().is_empty() {
        return Some(json!({}));
    }
    serde_json::from_str(line.trim()).ok()
}

/// A connection to the unix socket at PATH without waiting: a full queue (a stalled
/// zone-init) is no answer, never a block.
fn connect_now(path: &Path) -> Option<UnixStream> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    // Safety: a sockaddr_un filled within its size, a socket we own (from_raw_fd below or
    // closed on failure).
    unsafe {
        let mut sa: libc::sockaddr_un = std::mem::zeroed();
        if bytes.len() >= sa.sun_path.len() {
            return None;
        }
        sa.sun_family = libc::AF_UNIX as libc::sa_family_t;
        for (d, b) in sa.sun_path.iter_mut().zip(bytes) {
            *d = *b as libc::c_char;
        }
        let fd = crate::sys::unix_stream_nonblock();
        if fd < 0 {
            return None;
        }
        let len = (std::mem::size_of::<libc::sa_family_t>() + bytes.len() + 1) as libc::socklen_t;
        if libc::connect(fd, (&sa as *const libc::sockaddr_un).cast(), len) != 0 {
            libc::close(fd);
            return None;
        }
        let s = <UnixStream as std::os::fd::FromRawFd>::from_raw_fd(fd);
        s.set_nonblocking(false).ok()?;
        Some(s)
    }
}

/// zone-init answers (agent.ping: a non-empty answer).
fn ping(zrt: &Path) -> bool {
    agent(zrt, &json!({"op": "ping"}), Duration::from_secs(1)).and_then(|v| v.as_object().map(|o| !o.is_empty())) == Some(true)
}

/// Whether anything runs in the zone besides bwrap, zone-init and its own helpers; None
/// when zone-init does not answer (server.apps_running).
fn apps_running(zrt: &Path) -> Option<bool> {
    let r = agent(zrt, &json!({"op": "ps"}), Duration::from_secs(5))?;
    if r.get("ok") != Some(&Value::Bool(true)) {
        return None;
    }
    let procs = r.get("procs")?.as_array()?;
    let mut skip = vec![json!(1), r.get("init").cloned().unwrap_or(Value::Null)];
    skip.extend(r.get("helpers").and_then(Value::as_array).cloned().unwrap_or_default());
    Some(procs.iter().any(|p| p.get(0).map(|pid| !skip.contains(pid)).unwrap_or(false)))
}

/// systemd's readiness ($NOTIFY_SOCKET), quietly when there is none.
fn sd_notify(msg: &str) {
    let Some(addr) = std::env::var_os("NOTIFY_SOCKET").filter(|a| !a.is_empty()) else { return };
    let Ok(s) = UnixDatagram::unbound() else { return };
    let a = addr.to_string_lossy();
    if let Some(name) = a.strip_prefix('@') {
        // (an abstract socket: Linux only, as systemd)
        #[cfg(target_os = "linux")]
        {
            use std::os::linux::net::SocketAddrExt;
            if let Ok(sa) = std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes()) {
                let _ = s.send_to_addr(msg.as_bytes(), &sa);
            }
        }
        let _ = name;
    } else {
        let _ = s.send_to(msg.as_bytes(), &*a);
    }
}

fn str_list(v: &Value) -> Option<Vec<String>> {
    v.as_array()?.iter().map(|x| x.as_str().map(String::from)).collect()
}

fn spawn(argv: &[String]) -> Command {
    let mut c = Command::new(&argv[0]);
    c.args(&argv[1..]);
    c
}

struct Run {
    /// _cleanup: runtime mounts end with the zone, also when its start is aborted
    cleanup: Option<Vec<String>>,
    name: String,
    zrt: PathBuf,
    fp: PathBuf,
    timeout: Duration,
    helpers: Vec<Child>,
    child: Option<Child>,
    relay_done: Option<mpsc::Receiver<()>>,
}

impl Run {
    fn stop_all(&mut self) {
        if let Some(c) = self.child.as_mut() {
            if running(c) {
                term(c);
            }
        }
        for h in self.helpers.iter_mut() {
            if running(h) {
                term(h);
            }
        }
    }

    /// SIGINT (`appzones stop`, Ctrl-C) ends the zone for good: 0. SIGTERM: 143.
    fn stop_code(&self) -> u8 {
        if stop() == libc::SIGINT { 0 } else { (128 + stop()) as u8 }
    }

    /// The zone's last lines in our log: the relay ends when the zone's writers have, or
    /// after a moment.
    fn wait_relay(&mut self) {
        if let Some(rx) = self.relay_done.take() {
            let _ = rx.recv_timeout(Duration::from_secs(2));
        }
    }

    /// End a start that failed, or was stopped (then it is not a failure).
    fn abort(&mut self, msg: &str) -> u8 {
        self.stop_all();
        if let Some(c) = self.child.as_mut() {
            wait_or_kill(c, Duration::from_secs(10));
        }
        self.wait_relay();
        let _ = std::fs::remove_dir_all(&self.fp);
        if let Some(cleanup) = &self.cleanup {
            let _ = spawn(cleanup).stdin(Stdio::null()).status(); // (a mount made while it started)
        }
        if stop() != 0 {
            say(&format!("[zone] {}: stopped while starting", self.name));
            return self.stop_code();
        }
        say(&format!("[zone] {}: {msg}; stopping", self.name));
        1
    }

    /// Give _netsetup a pidfd of this server over a unix socket (server.offer_pidfd).
    fn offer_pidfd(&mut self) -> bool {
        let path = self.zrt.join("srv.sock");
        let _ = std::fs::remove_file(&path);
        let Ok(ls) = UnixListener::bind(&path) else { return false };
        let _ = ls.set_nonblocking(true);
        let end = Instant::now() + self.timeout;
        let mut ok = false;
        while Instant::now() < end && stop() == 0 && self.child.as_mut().map(running).unwrap_or(false) {
            match ls.accept() {
                Ok((conn, _)) => {
                    let _ = conn.set_nonblocking(false);
                    // (ours, closed below)
                    let me = crate::sys::pidfd_open(std::process::id() as libc::pid_t).unwrap_or(-1);
                    if me < 0 {
                        say(&format!("[zone] {}: handing over the server's pidfd failed: {}", self.name,
                                     std::io::Error::last_os_error()));
                        break;
                    }
                    match send_fd(&conn, b"pidfd", me) {
                        Ok(()) => ok = true,
                        Err(e) => say(&format!("[zone] {}: handing over the server's pidfd failed: {e}", self.name)),
                    }
                    // Safety: closing our copy.
                    unsafe { libc::close(me) };
                    break;
                }
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
        let _ = std::fs::remove_file(&path);
        ok
    }
}

/// Copy what the zone writes to its log socket to our stdout, until it closes it; signals
/// the end on DONE (server._relay_output).
fn relay_output(listener: UnixListener, timeout: Duration, done: mpsc::Sender<()>) {
    let _ = listener.set_nonblocking(true);
    let end = Instant::now() + timeout;
    let conn = loop {
        match listener.accept() {
            Ok((c, _)) => break Some(c),
            Err(_) if Instant::now() < end => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => break None, // no zone came (it failed to start: the server says why)
        }
    };
    drop(listener);
    if let Some(mut conn) = conn {
        let _ = conn.set_nonblocking(false);
        let mut buf = [0u8; 65536];
        while let Ok(n) = conn.read(&mut buf) {
            if n == 0 {
                break;
            }
            let mut data = &buf[..n];
            while !data.is_empty() {
                // Safety: write to our own stdout.
                let w = unsafe { libc::write(1, data.as_ptr().cast(), data.len()) };
                if w <= 0 {
                    break;
                }
                data = &data[w as usize..];
            }
        }
    }
    let _ = done.send(());
}

/// One link from the zone's link socket: COMMAND + [url] opens it (`appzones _link`).
/// Links being opened now: each in its own thread, so the server goes on supervising (and
/// stops when asked) while a browser takes its time; at most this many at once.
static LINKS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
const MAX_LINKS: usize = 4;

fn handle_link(listener: &UnixListener, command: &[String]) {
    use std::sync::atomic::Ordering;
    let Ok((mut conn, _)) = listener.accept() else { return };
    if LINKS.fetch_add(1, Ordering::SeqCst) >= MAX_LINKS {
        LINKS.fetch_sub(1, Ordering::SeqCst);
        let _ = conn.set_nonblocking(false);
        let _ = conn.set_write_timeout(Some(Duration::from_secs(1)));
        let _ = conn.write_all(b"no\n");
        return;
    }
    let command = command.to_vec();
    std::thread::spawn(move || {
        open_link(conn, &command);
        LINKS.fetch_sub(1, Ordering::SeqCst);
    });
}

fn open_link(mut conn: UnixStream, command: &[String]) {
    let _ = conn.set_nonblocking(false);
    let _ = conn.set_read_timeout(Some(Duration::from_secs(2)));
    let mut buf = vec![0u8; 8192];
    let Ok(n) = conn.read(&mut buf) else { return };
    let url = String::from_utf8_lossy(&buf[..n]).trim().to_string();
    let mut ok = false;
    if link_ok(&url) {
        if let Ok(mut c) = spawn(command).arg(&url).stdin(Stdio::null()).spawn() {
            ok = exit_status(&mut c, Duration::from_secs(30)) == Some(0);
            if !ok {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
    }
    let _ = conn.write_all(if ok { b"ok\n" } else { b"no\n" });
}

/// Where the plan comes from: a file `appzones serve` wrote, or made here for a zone.
enum Source {
    File(PathBuf),
    Zone { name: String },
}

/// The arguments, raw: the plan's path byte for byte, the rest must be text.
fn parse_args(args: &[std::ffi::OsString]) -> Result<(Source, RawFd, Option<RawFd>), String> {
    let (mut plan, mut zone, mut lock, mut notify) = (None, None, None, None);
    let mut it = args.iter();
    let fd = |v: &std::ffi::OsString| v.to_str().and_then(|v| v.parse::<RawFd>().ok()).ok_or(USAGE);
    while let Some(a) = it.next() {
        let v = it.next().ok_or(USAGE)?;
        match a.to_str() {
            Some("--plan") => plan = Some(PathBuf::from(v)),
            Some("--zone") => zone = Some(v.to_str().ok_or(USAGE)?.to_string()),
            Some("--lock-fd") => lock = Some(fd(v)?),
            Some("--notify-fd") => notify = Some(fd(v)?),
            _ => return Err(USAGE.into()),
        }
    }
    let source = match (plan, zone) {
        (Some(p), None) => Source::File(p),
        (None, Some(name)) => Source::Zone { name },
        _ => return Err(USAGE.into()),
    };
    Ok((source, lock.ok_or(USAGE)?, notify))
}

pub fn main(args: &[std::ffi::OsString]) -> u8 {
    let (source, lock_fd, notify_fd) = match parse_args(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    serve_from(source, lock_fd, notify_fd)
}

/// `appzones serve NAME` in Rust (cli.rs): the lock taken, the plan made and run here.
pub fn serve_zone(name: &str, lock_fd: RawFd, notify_fd: Option<RawFd>) -> u8 {
    serve_from(Source::Zone { name: name.into() }, lock_fd, notify_fd)
}

fn serve_from(source: Source, lock_fd: RawFd, notify_fd: Option<RawFd>) -> u8 {
    // the lock is held as long as this process lives; neither it nor the readiness fd
    // goes into the zone or a helper
    set_cloexec(lock_fd);
    if let Some(fd) = notify_fd {
        set_cloexec(fd);
    }
    // Safety: handlers that only store to an atomic.
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_stop as extern "C" fn(libc::c_int) as libc::sighandler_t;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(libc::SIGTERM, &sa, std::ptr::null_mut());
        libc::sigaction(libc::SIGINT, &sa, std::ptr::null_mut());
        // `appzones serve` blocked both across the exec, so a stop in that instant waits:
        // now that it is handled, let it through (a pending one sets STOP at once)
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::sigprocmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
    }
    let plan_path = match source {
        Source::File(p) => p,
        Source::Zone { name } => {
            // the first part of `appzones serve`, here (plan.rs): a stop while it runs is
            // not a failure, as before any helper started
            let host = crate::plan::Host::new(crate::plan::exe_dir());
            let made = crate::plan::make_plan(&host, &name);
            if stop() != 0 {
                say(&format!("[zone] {name}: stopped while starting"));
                return if stop() == libc::SIGINT { 0 } else { (128 + stop()) as u8 };
            }
            match made.and_then(|p| crate::plan::write_plan(&p)) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("appzones: {e}");
                    return 1;
                }
            }
        }
    };
    let plan: Value = match std::fs::read(&plan_path).ok().and_then(|b| serde_json::from_slice(&b).ok()) {
        Some(p) => p,
        None => {
            say(&format!("[zone] cannot read the plan {}", plan_path.display()));
            return 1;
        }
    };
    if plan.get("version").and_then(Value::as_u64) != Some(PLAN_VERSION) {
        say(&format!("[zone] {}: a plan of another version (appzones and its helper do not match)", plan_path.display()));
        return 1;
    }
    match run(&plan, notify_fd) {
        Ok(code) => code,
        Err(e) => {
            say(&format!("[zone] {}: {e}", plan_path.display()));
            1
        }
    }
}

fn run(plan: &Value, mut notify_fd: Option<RawFd>) -> Result<u8, String> {
    let s = |k: &str| plan.get(k).and_then(Value::as_str).map(String::from).ok_or(format!("the plan has no {k}"));
    let name = s("name")?;
    let zrt = PathBuf::from(s("zrt")?);
    let fp = PathBuf::from(s("flatpak_dir")?);
    let timeout_s = plan.get("start_timeout").and_then(Value::as_f64).ok_or("the plan has no start_timeout")?;
    let timeout = Duration::from_secs_f64(timeout_s);
    let argv_marked = plan.get("argv").and_then(str_list).ok_or("the plan has no argv")?;
    let mut r = Run { cleanup: plan.get("cleanup").and_then(str_list), name: name.clone(), zrt: zrt.clone(), fp, timeout, helpers: Vec::new(), child: None, relay_done: None };
    if stop() != 0 {
        return Ok(r.abort(""));
    }
    let links = plan.get("links").filter(|l| !l.is_null());
    let links_srv = match links {
        Some(l) => {
            let path = l.get("socket").and_then(Value::as_str).ok_or("the plan's links have no socket")?;
            let ls = UnixListener::bind(path).map_err(|e| format!("{path}: {e}"))?;
            ls.set_nonblocking(true).map_err(|e| e.to_string())?;
            Some((ls, l.get("command").and_then(str_list).ok_or("the plan's links have no command")?))
        }
        None => None,
    };
    let mut have: HashSet<&str> = HashSet::new();
    let mut host_wl = None;
    let wl = plan.get("wayland").ok_or("the plan has no wayland")?;
    let mode = wl.get("mode").and_then(Value::as_str).unwrap_or("zone");
    if let Some(helper) = wl.get("helper").and_then(str_list) {
        let mut h = spawn(&helper).stdout(Stdio::piped()).env("ZONE_WAYLAND_PARENT", std::process::id().to_string())
            .spawn().map_err(|e| format!("zone-wayland: {e}"))?;
        let out = h.stdout.take();
        r.helpers.push(h);
        let ready = out.as_ref().and_then(|o| read_line(o.as_raw_fd(), timeout)).as_deref() == Some(b"ready".as_slice());
        if ready && zrt.join("wayland-0").exists() {
            have.insert("wayland-0");
        }
        if ready && mode == "host" {
            say(&format!("[zone] {name}: wayland = \"host\", but the compositor has a security context: the zone \
                          gets its own (the host's socket is only for compositors without one)"));
        }
        if !ready {
            let h = r.helpers.last_mut().expect("just pushed");
            let rc = exit_status(h, Duration::from_secs(2));
            if rc != Some(WAYLAND_NO_PROTOCOL) {
                let msg = match rc {
                    Some(rc) => format!("zone-wayland exited with status {rc} (its message is above)"),
                    None => format!("zone-wayland did not become ready within {timeout_s:.0} s"),
                };
                return Ok(r.abort(&msg));
            }
            r.helpers.pop();
        }
        if !ready && mode == "host" {
            host_wl = host_wayland_socket();
        }
        if host_wl.is_some() {
            have.insert("wayland-host");
            say(&format!("[zone] {name}: wayland = \"host\": the compositor has no security context, so the zone's \
                          apps get the host's Wayland socket"));
        } else if !ready {
            // less, never more: no Wayland socket in the zone, and never the host's
            say(&format!("[zone] {name}: the compositor has no security-context protocol (wp_security_context_v1), \
                          so the zone starts without a Wayland socket (desktop apps will not open windows). \
                          Compositors with it: Hyprland, sway 1.9 and later, others built on wlroots 0.17 or later, \
                          KDE Plasma 6. To give this zone's apps the host's socket instead (GNOME): wayland = \"host\""));
        }
    }
    if let Some(proxy) = plan.get("dbus_proxy").and_then(str_list) {
        let server = std::process::id() as libc::pid_t;
        let mut c = spawn(&proxy);
        // Safety: between fork and exec only async-signal-safe calls (prctl, getppid,
        // _exit): it gets SIGKILL when this server dies, and exits if it died already.
        unsafe {
            c.pre_exec(move || {
                crate::sys::die_with_parent();
                if libc::getppid() != server {
                    libc::_exit(1);
                }
                Ok(())
            });
        }
        r.helpers.push(c.spawn().map_err(|e| format!("xdg-dbus-proxy: {e}"))?);
        let end = Instant::now() + timeout;
        while !zrt.join("bus").exists() {
            let alive = r.helpers.last_mut().map(running).unwrap_or(false);
            if Instant::now() > end || !alive || stop() != 0 {
                return Ok(r.abort("xdg-dbus-proxy did not create the zone's bus"));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    if zrt.join("bus").exists() {
        have.insert("bus");
    }
    let argv = resolve_plan_argv(&argv_marked, &have, host_wl.as_deref())?;
    let mut log_sock = None;
    if plan.get("log_socket") == Some(&Value::Bool(true)) {
        // the zone's output through a socket, not through pasta (see run_plan)
        let path = zrt.join("log.sock");
        let _ = std::fs::remove_file(&path);
        let ls = UnixListener::bind(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
        log_sock = Some(ls);
    }
    if stop() != 0 {
        return Ok(r.abort(""));
    }
    if let Some(up) = plan.get("dns_upstreams").and_then(str_list) {
        host_dns::serve(&zrt.join("ctl").join("dns.sock"), up).map_err(|e| format!("the zone's DNS: {e}"))?;
    }
    let mut cmd = spawn(&argv);
    cmd.env("ZONE_START_TIMEOUT", format!("{timeout_s:?}"));
    // the zone dies with this server, from outside it: pasta (or bwrap) gets SIGKILL, and
    // _netsetup passes the same on to bwrap (a zone process can stop zone-init, whose
    // pidfd watch is the other way the zone ends)
    let server = std::process::id() as libc::pid_t;
    // Safety: between fork and exec only async-signal-safe calls (prctl, getppid, _exit).
    unsafe {
        cmd.pre_exec(move || {
            crate::sys::die_with_parent();
            if libc::getppid() != server {
                libc::_exit(1);
            }
            Ok(())
        });
    }
    if log_sock.is_some() {
        cmd.env("ZONE_LOG_SOCK", zrt.join("log.sock"));
    }
    r.child = Some(cmd.spawn().map_err(|e| format!("{}: {e}", argv[0]))?);
    if let Some(ls) = log_sock {
        let (tx, rx) = mpsc::channel();
        r.relay_done = Some(rx);
        std::thread::spawn(move || relay_output(ls, timeout, tx));
    }
    if !r.offer_pidfd() {
        return Ok(r.abort("_netsetup did not take the server's pidfd"));
    }
    // ready: zone-init answers; then the autostart apps
    let end = Instant::now() + timeout;
    let mut answered = false;
    while Instant::now() < end && stop() == 0 && r.child.as_mut().map(running).unwrap_or(false) {
        if ping(&zrt) {
            answered = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if stop() != 0 {
        return Ok(r.abort(""));
    }
    let alive = r.child.as_mut().map(running).unwrap_or(false);
    if alive && !answered {
        return Ok(r.abort(&format!("zone-init did not answer within {timeout_s:.0} s")));
    }
    if alive {
        sd_notify(&format!("READY=1\nSTATUS=zone {name} running"));
        if let Some(fd) = notify_fd.take() {
            // Safety: one newline to the readiness descriptor we were given, then closed.
            if unsafe { libc::write(fd, b"\n".as_ptr().cast(), 1) } != 1 {
                say(&format!("[zone] {name}: could not write readiness to fd {fd}: {}", std::io::Error::last_os_error()));
            }
            // Safety: closing it.
            unsafe { libc::close(fd) };
        }
        let devices: Vec<String> = plan.get("devices").and_then(str_list).unwrap_or_default();
        say(&format!("[zone] {name} ready (network={}, dns={}, devices={})", s("network")?, s("dns")?,
                     if devices.is_empty() { "-".into() } else { devices.join(",") }));
        for a in plan.get("autostart").and_then(Value::as_array).cloned().unwrap_or_default() {
            if stop() != 0 {
                break; // a stop: no more apps, on to the shutdown
            }
            let app = a.get("app").and_then(Value::as_str).unwrap_or("?");
            let req = json!({"op": "run", "argv": a.get("argv"), "env": {}, "cwd": null, "bus": a.get("bus")});
            match agent(&zrt, &req, Duration::from_secs(5)) {
                Some(v) if v.get("ok") == Some(&Value::Bool(true)) => {
                    say(&format!("[zone] autostart {app}: {}", v.get("pid").unwrap_or(&Value::Null)));
                }
                Some(v) => say(&format!("[zone] autostart {app} failed: {}",
                                        v.get("error").and_then(Value::as_str).unwrap_or("run failed"))),
                None => say(&format!("[zone] autostart {app} failed: zone-init did not answer")),
            }
        }
    }
    // wait for the zone; a dead helper takes the zone down
    let linger = plan.get("linger").and_then(Value::as_f64).filter(|l| *l > 0.0);
    let linger_text = plan.get("linger_text").and_then(Value::as_str).unwrap_or("").to_string();
    let mut idle_since: Option<Instant> = None;
    let mut next_check = Instant::now();
    let rc: i32 = loop {
        let child = r.child.as_mut().expect("started");
        if let Ok(Some(st)) = child.try_wait() {
            break returncode(st);
        }
        if stop() != 0 {
            r.stop_all();
            break wait_or_kill(r.child.as_mut().expect("started"), Duration::from_secs(10));
        }
        if let Some(linger) = linger {
            let now = Instant::now();
            if now >= next_check {
                next_check = now + Duration::from_secs_f64((linger / 3.0).min(10.0));
                if apps_running(&zrt) != Some(false) {
                    idle_since = None; // an app, or no answer: not idle
                } else if idle_since.is_none() {
                    idle_since = Some(now);
                } else if now.duration_since(idle_since.expect("set")).as_secs_f64() >= linger {
                    say(&format!("[zone] {name}: no app for {linger_text} (linger); stopping"));
                    let child = r.child.as_mut().expect("started");
                    term(child);
                    wait_or_kill(child, Duration::from_secs(10));
                    break 0; // ended as configured: not a failure, so no restart
                }
            }
        }
        if r.helpers.iter_mut().any(|h| !running(h)) {
            say("[zone] a helper (zone-wayland or xdg-dbus-proxy) exited; stopping the zone");
            let child = r.child.as_mut().expect("started");
            term(child);
            let rc = wait_or_kill(child, Duration::from_secs(10));
            break if rc == 0 { 1 } else { rc };
        }
        let ready = links_srv.as_ref().map(|(ls, _)| {
            let mut pfd = libc::pollfd { fd: ls.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            // Safety: poll on our listening socket.
            unsafe { libc::poll(&mut pfd, 1, 500) > 0 }
        });
        match (ready, links_srv.as_ref()) {
            (Some(true), Some((ls, command))) => handle_link(ls, command),
            (None, _) => std::thread::sleep(Duration::from_millis(500)),
            _ => {}
        }
    };
    r.stop_all();
    r.wait_relay();
    let _ = std::fs::remove_dir_all(&r.fp);
    if let Some(cleanup) = plan.get("cleanup").and_then(str_list) {
        let _ = spawn(&cleanup).stdin(Stdio::null()).status(); // runtime mounts end with the zone
    }
    // a stop that was asked for is not a failure: a process manager restarts only failures
    Ok(if stop() != 0 { r.stop_code() } else { rc as u8 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn plan_argv() {
        let i = |c: &str| format!("{PLAN_IF}{c}");
        let argv: Vec<String> = vec!["bwrap".into(), i("wayland-0"), "--bind".into(), "/z/wl".into(), "/w0".into(),
                                     PLAN_END.into(), i("wayland-host"), "--bind".into(), PLAN_HOST_WAYLAND.into(),
                                     "/w0".into(), PLAN_END.into(), i("bus"), "--bind".into(), "/z/bus".into(),
                                     "/b".into(), PLAN_END.into(), "--".into(), "zone-init".into()];
        let none = HashSet::new();
        assert_eq!(resolve_plan_argv(&argv, &none, None).unwrap(), v(&["bwrap", "--", "zone-init"]));
        let both: HashSet<&str> = ["wayland-0", "bus"].into();
        assert_eq!(resolve_plan_argv(&argv, &both, None).unwrap(),
                   v(&["bwrap", "--bind", "/z/wl", "/w0", "--bind", "/z/bus", "/b", "--", "zone-init"]));
        let host: HashSet<&str> = ["wayland-host"].into();
        assert_eq!(resolve_plan_argv(&argv, &host, Some("/run/user/1000/wayland-1")).unwrap(),
                   v(&["bwrap", "--bind", "/run/user/1000/wayland-1", "/w0", "--", "zone-init"]));
        assert!(resolve_plan_argv(&argv, &host, None).is_err(), "the host's socket asked for, none given");
    }

    #[test]
    fn wayland_socket() {
        let t = std::env::temp_dir().join(format!("az-wl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        let t = std::fs::canonicalize(&t).unwrap();
        let _l = std::os::unix::net::UnixListener::bind(t.join("wayland-5")).unwrap();
        std::fs::write(t.join("plain"), "").unwrap();
        let d = |x: &str| Some(std::ffi::OsString::from(x));
        let want = Some(t.join("wayland-5").to_string_lossy().into_owned());
        assert_eq!(host_wayland_socket_for(d("wayland-5"), &t), want);
        assert_eq!(host_wayland_socket_for(d(&t.join("wayland-5").to_string_lossy()), &t), want); // a path too
        assert_eq!(host_wayland_socket_for(d("wayland-404"), &t), None);
        assert_eq!(host_wayland_socket_for(d("plain"), &t), None); // not a socket
        assert_eq!(host_wayland_socket_for(None, &t), None);
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn apps_running_skips_the_helpers() {
        // bwrap (1), zone-init and its helpers are not apps; no reply: not known
        let t = std::env::temp_dir().join(format!("az-apps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        assert_eq!(apps_running(&t), None, "nothing listening");
        let l = std::os::unix::net::UnixListener::bind(t.join("agent.sock")).unwrap();
        let replies = [
            r#"{"ok": true, "procs": [[1, "bwrap"], [2, "zone-init"], [5, "dbus-daemon"]], "init": 2, "helpers": [5]}"#,
            r#"{"ok": true, "procs": [[1, "bwrap"], [2, "zone-init"], [7, "firefox"]], "init": 2}"#,
            "",
        ];
        let server = std::thread::spawn(move || {
            for r in replies {
                let (mut c, _) = l.accept().unwrap();
                let mut line = String::new();
                BufReader::new(c.try_clone().unwrap()).read_line(&mut line).unwrap();
                assert!(line.contains("\"ps\""), "{line}");
                if !r.is_empty() {
                    c.write_all(format!("{r}\n").as_bytes()).unwrap();
                }
            }
        });
        assert_eq!(apps_running(&t), Some(false));
        assert_eq!(apps_running(&t), Some(true));
        assert_eq!(apps_running(&t), None, "end of file without a reply: not idle");
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn links() {
        for (url, ok) in [("https://example.com/a", true), ("http://x", true), ("file:///etc/passwd", false),
                          ("https://x\nrm", false), ("https://x\0", false), ("javascript:alert(1)", false)] {
            assert_eq!(link_ok(url), ok, "{url}");
        }
        assert!(!link_ok(&format!("https://{}", "a".repeat(8000))));
    }

    #[test]
    fn returncodes() {
        assert_eq!(returncode(ExitStatus::from_raw(0)), 0);
        assert_eq!(returncode(ExitStatus::from_raw(3 << 8)), 3);
        assert_eq!(returncode(ExitStatus::from_raw(libc::SIGTERM)), -libc::SIGTERM);
        // a zone killed by SIGTERM: Python's sys.exit(-15) is status 241, as here
        assert_eq!(-libc::SIGTERM as u8, 241);
    }

    #[test]
    fn args() {
        use std::os::unix::ffi::OsStringExt;
        let o = |a: &[&str]| a.iter().map(std::ffi::OsString::from).collect::<Vec<_>>();
        assert!(parse_args(&o(&["--plan", "/p", "--lock-fd", "4"])).is_ok());
        assert!(parse_args(&o(&["--zone", "z", "--lock-fd", "4"])).is_ok());
        assert!(parse_args(&o(&["--zone", "z", "--plan", "/p", "--lock-fd", "4"])).is_err(), "both sources");
        assert!(parse_args(&o(&["--zone", "z", "--python", "/py", "--lock-fd", "4"])).is_err(), "no Python any more");
        assert_eq!(parse_args(&o(&["--plan", "/p", "--lock-fd", "4", "--notify-fd", "6"])).unwrap().2, Some(6));
        assert!(parse_args(&o(&["--plan", "/p"])).is_err(), "no lock");
        assert!(parse_args(&o(&["--plan", "/p", "--lock-fd", "x"])).is_err());
        assert!(parse_args(&o(&["--other", "1"])).is_err());
        // a plan path that is not UTF-8 stays byte for byte
        let raw = std::ffi::OsString::from_vec(b"/run/caf\xe9/plan.json".to_vec());
        let got = parse_args(&[std::ffi::OsString::from("--plan"), raw.clone(), "--lock-fd".into(), "4".into()]).unwrap();
        match got.0 {
            Source::File(p) => assert_eq!(p.as_os_str(), raw.as_os_str()),
            Source::Zone { .. } => panic!("not a plan file"),
        }
    }
}
