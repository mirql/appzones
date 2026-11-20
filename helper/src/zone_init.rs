//! zone-init: the agent inside a zone (the port of zone_init.py; its docstring is the
//! protocol, and src/test_zonecfg.py's AgentProtocolTest is the specification).
//!
//! Runs as pid 2 of the zone (bwrap is pid 1 and reaps). Listens on the socket the host
//! made and handed over as fd $ZONE_CTL_FD; ends when its server's pidfd
//! ($ZONE_SERVER_FD) says the server is gone. One JSON request line per connection.
//! One thread runs requests, children and output (a poll loop); the zone's DNS runs in
//! threads of its own (zone_dns). Children are started with std's Command: between fork
//! and exec only async-signal-safe syscalls run (child_setup).

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::{CString, OsStr, OsString};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};

use crate::zone_dns;

const MOZ_APPS: &[&str] = &["firefox", "firefox-esr", "firefox-beta", "firefox-devedition", "firefox-nightly",
                            "librewolf"];
const ENV_ALLOW: &[&str] = &["TERM", "COLUMNS", "LINES", "LANG", "LC_ALL", "MOZ_HEADLESS", "NO_AT_BRIDGE"];
const SHELL: &[&str] = &["/bin/bash", "-i"]; // NixOS zones have /bin too (the system's sw/bin)
const TAIL: usize = 20;
const FINISHED: usize = 64;
// names the zone's own bus does not offer: they would start inside the zone, where they cannot work
const ZBUS_SKIP: &[&str] = &["org.freedesktop.portal.", "org.freedesktop.impl.portal.", "org.freedesktop.Flatpak",
                             "org.flatpak.", "org.freedesktop.systemd1", "org.freedesktop.secrets",
                             "org.gnome.keyring", "org.kde.kwalletd", "org.a11y.", "org.freedesktop.Notifications",
                             "org.kde.StatusNotifierWatcher", "org.kde.plasmashell", "org.kde.KSplash",
                             "org.kde.krunner", "org.kde.LogoutPrompt", "org.kde.Shutdown", "org.freedesktop.impl."];
const ZBUS_CONF: &str = r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <keep_umask/>
  <listen>unix:path={dir}/bus</listen>
  <auth>EXTERNAL</auth>
  <servicedir>{dir}/services</servicedir>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#;

pub fn log(msg: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "[zone-init] {msg}");
    let _ = out.flush();
}

fn errno_text(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.rsplit_once(" (os error") {
        Some((text, _)) => text.to_string(),
        None => s,
    }
}

pub fn moz_remoting_name(zone: &str, app: &str) -> String {
    format!("{}_zone_{}", app.replace('-', "_"), zone.replace('-', "_"))
}

fn basename(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

/// A non-empty list of strings (a string is not an argv: it would run as one program).
pub fn argv_of(v: Option<&Value>) -> Option<Vec<String>> {
    let list = v?.as_array()?;
    if list.is_empty() {
        return None;
    }
    list.iter().map(|a| a.as_str().map(String::from)).collect()
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// A pipe, a regular file or /dev/null: never a tty, a directory (openat on it reaches
/// host files), a socket or another device.
fn stdio_ok(fd: RawFd) -> bool {
    // Safety: fstat and isatty only read the descriptor's state into a local struct.
    unsafe {
        let mut st: libc::stat = std::mem::zeroed();
        if libc::fstat(fd, &mut st) != 0 || libc::isatty(fd) == 1 {
            return false;
        }
        let kind = st.st_mode & libc::S_IFMT;
        kind == libc::S_IFIFO
            || kind == libc::S_IFREG
            || (kind == libc::S_IFCHR && libc::major(st.st_rdev) == 1 && libc::minor(st.st_rdev) == 3)
    }
}

fn set_cloexec(fd: RawFd) {
    // Safety: fcntl on a descriptor we own; only its flags change.
    unsafe {
        let f = libc::fcntl(fd, libc::F_GETFD);
        if f >= 0 {
            libc::fcntl(fd, libc::F_SETFD, f | libc::FD_CLOEXEC);
        }
    }
}

fn set_nonblocking(fd: RawFd, on: bool) {
    // Safety: as set_cloexec, the file status flags.
    unsafe {
        let f = libc::fcntl(fd, libc::F_GETFL);
        if f >= 0 {
            libc::fcntl(fd, libc::F_SETFL, if on { f | libc::O_NONBLOCK } else { f & !libc::O_NONBLOCK });
        }
    }
}

fn pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut p = [0 as RawFd; 2];
    // Safety: pipe2 fills the two-element array with new descriptors, which we own.
    if unsafe { libc::pipe2(p.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { (OwnedFd::from_raw_fd(p[0]), OwnedFd::from_raw_fd(p[1])) })
}

fn set_winsize(fd: RawFd, rows: u16, cols: u16) {
    let ws = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
    // Safety: TIOCSWINSZ reads the winsize struct we pass.
    unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &ws) };
}

fn dim(v: Option<&Value>, default: u16) -> u16 {
    v.and_then(Value::as_u64).and_then(|n| u16::try_from(n).ok()).unwrap_or(default)
}

/// In the child, between fork and exec (async-signal-safe syscalls only): an empty
/// signal mask, a session of its own, the controlling tty (fd 0, for a shell), and every
/// descriptor above 2 close-on-exec, as Python's close_fds did.
fn child_setup(ctty: bool) -> std::io::Result<()> {
    // Safety: each call is a plain syscall on this process's own state.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigprocmask(libc::SIG_SETMASK, &set, std::ptr::null_mut());
        if libc::setsid() < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if ctty && libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // a child never gets zone-init's descriptors: if this fails (a kernel before 5.11),
        // the spawn fails
        if libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, libc::CLOSE_RANGE_CLOEXEC as u32) < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

fn reply(conn: &mut UnixStream, v: &Value) {
    let mut s = v.to_string();
    s.push('\n');
    let _ = conn.write_all(s.as_bytes());
}

fn fail(conn: &mut UnixStream, error: impl Into<String>) {
    reply(conn, &json!({"ok": false, "error": error.into()}));
}

/// A request: its bytes (up to the first newline, and what came after it) and the
/// descriptors sent with it (SCM_RIGHTS, close-on-exec at once).
fn recv_request(conn: &UnixStream) -> Option<(Vec<u8>, Vec<OwnedFd>)> {
    let mut buf = vec![0u8; 65536];
    let mut cmsg = [0u64; 8]; // room for three descriptors, aligned
    let mut iov = libc::iovec { iov_base: buf.as_mut_ptr().cast(), iov_len: buf.len() };
    // Safety: a zeroed msghdr pointing at our buffers; recvmsg fills them; the descriptors
    // it returns in SCM_RIGHTS are new and ours.
    let mut fds = Vec::new();
    let n = unsafe {
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cmsg.as_mut_ptr().cast();
        msg.msg_controllen = std::mem::size_of_val(&cmsg) as _;
        let n = libc::recvmsg(conn.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC);
        if n >= 0 {
            let mut c = libc::CMSG_FIRSTHDR(&msg);
            while !c.is_null() {
                if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                    let data = libc::CMSG_DATA(c).cast::<RawFd>();
                    let count = ((*c).cmsg_len as usize - libc::CMSG_LEN(0) as usize) / std::mem::size_of::<RawFd>();
                    for i in 0..count {
                        fds.push(OwnedFd::from_raw_fd(std::ptr::read_unaligned(data.add(i))));
                    }
                }
                c = libc::CMSG_NXTHDR(&msg, c);
            }
        }
        n
    };
    if n < 0 {
        return None;
    }
    buf.truncate(n as usize);
    // a request split across reads: on until a newline, EOF or 64 KiB, all within the one
    // deadline (a client cannot hold the loop up by sending a byte now and then)
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut reader = conn;
    while n > 0 && !buf.contains(&b'\n') && buf.len() < 65536 {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() || conn.set_read_timeout(Some(left)).is_err() {
            break;
        }
        let mut more = [0u8; 4096];
        match reader.read(&mut more) {
            Ok(0) | Err(_) => break,
            Ok(k) => buf.extend_from_slice(&more[..k]),
        }
    }
    Some((buf, fds))
}

struct Output {
    _pipe: OwnedFd, // held: the pipe closes when the output is forgotten
    pid: i32,
    label: String,
    part: Vec<u8>,
}

struct Shell {
    conn: UnixStream,
    master: OwnedFd,
}

#[derive(Clone, Copy)]
enum Tag {
    Listener,
    Signals,
    Out(RawFd),
    ShellIn(i32),
    ShellOut(i32),
}

type Env = HashMap<OsString, OsString>;

/// A variable as text (lossy: for zone-init's own use; children get the bytes).
fn var(env: &Env, k: &str) -> Option<String> {
    env.get(OsStr::new(k)).map(|v| v.to_string_lossy().into_owned())
}

struct Init {
    zone: String,
    zrt: String,
    // the environment as the host gave it: bytes, passed on unchanged (as Python's os.environ)
    env: Env,
    waiting: HashMap<i32, UnixStream>,
    shells: HashMap<i32, Shell>,
    outputs: HashMap<RawFd, Output>,
    tails: HashMap<i32, VecDeque<String>>,
    running: HashSet<i32>,
    finished: VecDeque<(i32, i32, Vec<String>)>,
    zbus: Option<i32>,
}

impl Init {
    fn zbus_dir(&self) -> String {
        format!("{}/zone-bus", self.zrt)
    }

    fn handle(&mut self, mut conn: UnixStream) {
        // a client that connects and says nothing cannot hold zone-init up
        let _ = conn.set_read_timeout(Some(Duration::from_secs(5)));
        let Some((data, fds)) = recv_request(&conn) else { return };
        let _ = conn.set_read_timeout(None);
        let (line, rest) = match data.iter().position(|&b| b == b'\n') {
            Some(i) => (&data[..i], data[i + 1..].to_vec()),
            None => (&data[..], Vec::new()),
        };
        let req = match std::str::from_utf8(line).ok().and_then(|s| serde_json::from_str::<Value>(s).ok()) {
            Some(v @ Value::Object(_)) => v,
            _ => return fail(&mut conn, "bad request"),
        };
        match req.get("op").and_then(Value::as_str) {
            Some("ping") => reply(&mut conn, &json!({"ok": true, "zone": self.zone, "pid": std::process::id()})),
            Some("ps") => {
                let helpers: Vec<i32> = self.zbus.into_iter().collect(); // not apps (linger)
                reply(&mut conn, &json!({"ok": true, "procs": procs(), "init": std::process::id(),
                                         "helpers": helpers}));
            }
            Some("run") => self.op_run(conn, &req, fds),
            Some("shell") => {
                drop(fds);
                self.op_shell(conn, &req, rest);
            }
            Some("status") => self.op_status(&mut conn, &req),
            Some("mounts") => reply(&mut conn, &json!({"ok": true, "mounts": host_mounts()})),
            Some("busy") => match req.get("path").and_then(Value::as_str) {
                Some(p) if p.starts_with("/mnt/host/") => reply(&mut conn, &json!({"ok": true, "procs": busy(p)})),
                _ => fail(&mut conn, "busy: path must be under /mnt/host/"),
            },
            Some("resize") => {
                let pid = req.get("pid").and_then(Value::as_i64).and_then(|p| i32::try_from(p).ok());
                let ok = match pid.and_then(|p| self.shells.get(&p).map(|s| (p, s))) {
                    Some((p, sh)) => {
                        set_winsize(sh.master.as_raw_fd(), dim(req.get("rows"), 24), dim(req.get("cols"), 80));
                        // Safety: a signal to our own child, which the shells table holds
                        unsafe { libc::kill(p, libc::SIGWINCH) };
                        true
                    }
                    None => false,
                };
                reply(&mut conn, &json!({"ok": ok}));
            }
            _ => {
                let op = match req.get("op") {
                    None | Some(Value::Null) => "None".to_string(),
                    Some(Value::String(s)) => format!("'{s}'"),
                    Some(v) => v.to_string(),
                };
                fail(&mut conn, format!("unknown op {op}"));
            }
        }
    }

    fn child_env(&self, req: &Value) -> Env {
        let mut env = self.env.clone();
        if let Some(extra) = req.get("env").and_then(Value::as_object) {
            for (k, v) in extra {
                if let (true, Some(v)) = (ENV_ALLOW.contains(&k.as_str()), v.as_str()) {
                    env.insert(k.into(), v.into());
                }
            }
        }
        env
    }

    fn cwd(&self, req: &Value) -> String {
        let home = var(&self.env, "HOME").filter(|h| !h.is_empty()).unwrap_or_else(|| "/".into());
        let want = req.get("cwd").and_then(Value::as_str).filter(|c| !c.is_empty()).unwrap_or(&home);
        if Path::new(want).is_dir() { want.to_string() } else { home }
    }

    fn op_run(&mut self, mut conn: UnixStream, req: &Value, fds: Vec<OwnedFd>) {
        let Some(mut argv) = argv_of(req.get("argv")) else {
            return fail(&mut conn, "argv must be a non-empty list of strings");
        };
        let wait = truthy(req.get("wait"));
        let mut stdio: [Option<OwnedFd>; 3] = [None, None, None];
        if wait {
            if fds.len() != 3 || !fds.iter().all(|f| stdio_ok(f.as_raw_fd())) {
                return fail(&mut conn, "wait needs 3 pipe, regular file or /dev/null fds (never a tty, \
                                        directory or socket)");
            }
            let mut it = fds.into_iter();
            stdio = [it.next(), it.next(), it.next()];
        }
        let mut env = self.child_env(req);
        let label = basename(&argv[0]).to_string();
        if MOZ_APPS.contains(&label.as_str()) {
            env.insert("MOZ_DBUS_APP_NAME".into(), moz_remoting_name(&self.zone, &label).into());
        }
        let bus = match req.get("bus") {
            None | Some(Value::Null) => "host".to_string(),
            Some(Value::String(s)) if s.is_empty() => "host".to_string(),
            Some(Value::String(s)) => s.clone(),
            Some(v) => v.to_string(),
        };
        match bus.as_str() {
            "host" => {}
            "zone" => {
                if let Err(e) = self.ensure_zone_bus() {
                    return fail(&mut conn, e);
                }
                env.insert("DBUS_SESSION_BUS_ADDRESS".into(), format!("unix:path={}/bus", self.zbus_dir()).into());
            }
            "none" => {
                env.remove(OsStr::new("DBUS_SESSION_BUS_ADDRESS"));
                argv = self.no_bus(argv);
            }
            other => return fail(&mut conn, format!("unknown bus '{other}' (host, zone, none)")),
        }
        let mut out_r = None;
        if !wait {
            match pipe() {
                Ok((r, w)) => {
                    stdio[1] = w.try_clone().ok();
                    stdio[2] = Some(w);
                    out_r = Some(r);
                }
                Err(e) => return fail(&mut conn, format!("{}: {}", argv[0], errno_text(&e))),
            }
        }
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..]).env_clear().envs(&env).current_dir(self.cwd(req));
        let [i, o, e] = stdio;
        cmd.stdin(i.map_or(Stdio::null(), Stdio::from));
        cmd.stdout(o.map_or(Stdio::null(), Stdio::from));
        cmd.stderr(e.map_or(Stdio::null(), Stdio::from));
        // Safety: child_setup makes only async-signal-safe syscalls.
        unsafe { cmd.pre_exec(|| child_setup(false)) };
        let spawned = cmd.spawn();
        drop(cmd); // our copies of the child's stdio
        let pid = match spawned {
            Ok(child) => child.id() as i32, // reaped by waitpid(-1) in reap()
            Err(e) => return fail(&mut conn, format!("{}: {}", argv[0], errno_text(&e))),
        };
        if let Some(r) = out_r {
            set_nonblocking(r.as_raw_fd(), true);
            self.outputs.insert(r.as_raw_fd(), Output { _pipe: r, pid, label: label.clone(), part: Vec::new() });
        }
        self.running.insert(pid);
        log(&format!("run {label} pid {pid} (bus {bus})"));
        reply(&mut conn, &json!({"ok": true, "pid": pid}));
        if wait {
            self.waiting.insert(pid, conn);
        }
    }

    fn op_shell(&mut self, mut conn: UnixStream, req: &Value, rest: Vec<u8>) {
        let argv = match req.get("argv") {
            None | Some(Value::Null) => SHELL.iter().map(|s| s.to_string()).collect(),
            v => match argv_of(v) {
                Some(a) => a,
                None => return fail(&mut conn, "argv must be a non-empty list of strings"),
            },
        };
        let mut env = self.child_env(req);
        let term = req.get("term").and_then(Value::as_str).map(String::from)
            .unwrap_or_else(|| var(&env, "TERM").unwrap_or_else(|| "xterm-256color".into()));
        env.insert("TERM".into(), term.into());
        let (mut master, mut slave) = (0 as RawFd, 0 as RawFd);
        // Safety: openpty fills two descriptors, which we own from here on.
        if unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null(),
                                  std::ptr::null()) } != 0 {
            return fail(&mut conn, format!("pty: {}", errno_text(&std::io::Error::last_os_error())));
        }
        let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        set_cloexec(master.as_raw_fd());
        set_cloexec(slave.as_raw_fd());
        // the size before the shell starts: it never reads the default one
        set_winsize(master.as_raw_fd(), dim(req.get("rows"), 24), dim(req.get("cols"), 80));
        let stdio = || slave.try_clone().map(Stdio::from);
        let (Ok(i), Ok(o), Ok(e)) = (stdio(), stdio(), stdio()) else {
            return fail(&mut conn, "pty: no descriptors left");
        };
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..]).env_clear().envs(&env).current_dir(self.cwd(req)).stdin(i).stdout(o).stderr(e);
        // Safety: child_setup makes only async-signal-safe syscalls.
        unsafe { cmd.pre_exec(|| child_setup(true)) };
        let spawned = cmd.spawn();
        drop(cmd);
        drop(slave);
        let pid = match spawned {
            Ok(child) => child.id() as i32,
            Err(e) => return fail(&mut conn, format!("{}: {}", argv[0], errno_text(&e))),
        };
        set_nonblocking(master.as_raw_fd(), true);
        reply(&mut conn, &json!({"ok": true, "pid": pid}));
        if !rest.is_empty() {
            write_all_fd(master.as_raw_fd(), &rest);
        }
        self.shells.insert(pid, Shell { conn, master });
        log(&format!("shell pid {pid}"));
    }

    fn op_status(&mut self, conn: &mut UnixStream, req: &Value) {
        let pid = req.get("pid").and_then(Value::as_i64).and_then(|p| i32::try_from(p).ok());
        if let Some(pid) = pid {
            let fds: Vec<RawFd> = self.outputs.iter().filter(|(_, o)| o.pid == pid).map(|(fd, _)| *fd).collect();
            for fd in fds {
                self.read_output(fd); // whatever it wrote last
            }
            if self.running.contains(&pid) {
                let tail: Vec<String> = self.tails.get(&pid).map(|t| t.iter().cloned().collect()).unwrap_or_default();
                return reply(conn, &json!({"ok": true, "running": true, "exit": null, "tail": tail}));
            }
            if let Some((_, code, tail)) = self.finished.iter().rev().find(|f| f.0 == pid) {
                return reply(conn, &json!({"ok": true, "running": false, "exit": code, "tail": tail}));
            }
        }
        let shown = req.get("pid").map_or("None".to_string(), Value::to_string);
        fail(conn, format!("no process {shown} started by zone-init"));
    }

    /// One read from an app's output pipe: whole lines go to the log and the tail. False
    /// when there was nothing to read; at EOF the pipe is closed and forgotten.
    fn read_output(&mut self, fd: RawFd) -> bool {
        if !self.outputs.contains_key(&fd) {
            return false; // closed already (at EOF, earlier in this pass): the number may be reused
        }
        let mut buf = [0u8; 65536];
        // Safety: read into our buffer from a descriptor the outputs table owns.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock {
            return false;
        }
        let Some(out) = self.outputs.get_mut(&fd) else { return false };
        let (pid, label) = (out.pid, out.label.clone());
        if n <= 0 {
            let part = std::mem::take(&mut out.part);
            self.outputs.remove(&fd);
            if !part.is_empty() {
                self.line(pid, &label, &part);
            }
            return false;
        }
        out.part.extend_from_slice(&buf[..n as usize]);
        // whole lines out, the unfinished rest kept: one pass over the buffer
        let Some(last) = out.part.iter().rposition(|&b| b == b'\n') else { return true };
        let rest = out.part.split_off(last + 1);
        let done = std::mem::replace(&mut out.part, rest);
        for l in done[..last].split(|&b| b == b'\n') {
            self.line(pid, &label, l);
        }
        true
    }

    fn line(&mut self, pid: i32, label: &str, raw: &[u8]) {
        let text = String::from_utf8_lossy(raw).trim_end_matches('\r').to_string();
        let tail = self.tails.entry(pid).or_default();
        if tail.len() == TAIL {
            tail.pop_front();
        }
        tail.push_back(text.clone());
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "[{label} {pid}] {text}");
        let _ = out.flush();
    }

    fn reap(&mut self) {
        loop {
            let mut status = 0;
            // Safety: waitpid writes the status of one of our children.
            let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
            if pid <= 0 {
                return;
            }
            let code = if libc::WIFEXITED(status) {
                libc::WEXITSTATUS(status)
            } else if libc::WIFSIGNALED(status) {
                -libc::WTERMSIG(status)
            } else {
                continue;
            };
            if self.zbus == Some(pid) {
                self.zbus = None;
                log(&format!("zone bus exited ({code}); it starts again on the next run with bus = zone"));
            }
            if self.running.remove(&pid) {
                let fds: Vec<RawFd> = self.outputs.iter().filter(|(_, o)| o.pid == pid).map(|(fd, _)| *fd).collect();
                for fd in fds {
                    // a bounded drain: an app's children may keep the pipe busy; the
                    // rest is read by the poll loop
                    for _ in 0..16 {
                        if !self.read_output(fd) {
                            break;
                        }
                    }
                }
                let tail = self.tails.remove(&pid).map(Vec::from).unwrap_or_default();
                self.finished.retain(|f| f.0 != pid); // a reused pid: only its newest exit
                self.finished.push_back((pid, code, tail));
                while self.finished.len() > FINISHED {
                    self.finished.pop_front();
                }
                if code != 0 {
                    log(&format!("pid {pid} exited with status {code}"));
                }
            }
            if let Some(mut conn) = self.waiting.remove(&pid) {
                reply(&mut conn, &json!({"exit": code}));
            }
            if let Some(mut sh) = self.shells.remove(&pid) {
                // what the shell wrote last
                let mut buf = [0u8; 65536];
                loop {
                    // Safety: read from the pty master the shells table owned.
                    let n = unsafe { libc::read(sh.master.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
                    if n <= 0 || sh.conn.write_all(&buf[..n as usize]).is_err() {
                        break;
                    }
                }
                // dropping sh closes the connection and the master
            }
        }
    }

    /// Bytes between a shell's connection and its pty.
    fn relay(&mut self, tag: Tag) {
        let (pid, from_conn) = match tag {
            Tag::ShellIn(p) => (p, true),
            Tag::ShellOut(p) => (p, false),
            _ => return,
        };
        let Some(sh) = self.shells.get_mut(&pid) else { return };
        let mut buf = [0u8; 65536];
        let done = if from_conn {
            match sh.conn.read(&mut buf) {
                Ok(0) | Err(_) => true,
                Ok(n) => !write_all_fd(sh.master.as_raw_fd(), &buf[..n]),
            }
        } else {
            // Safety: read from the pty master the shells table owns.
            let n = unsafe { libc::read(sh.master.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock {
                false
            } else {
                n <= 0 || sh.conn.write_all(&buf[..n as usize]).is_err() // EIO: the shell side closed
            }
        };
        if done {
            self.shells.remove(&pid); // the shell gets SIGHUP if it still runs
        }
    }

    fn ensure_zone_bus(&mut self) -> Result<(), String> {
        match self.zbus {
            Some(pid) if alive(pid) => return Ok(()),
            Some(_) => self.zbus = None, // exited, not reaped yet: start another
            None => {}
        }
        let daemon = var(&self.env, "ZONE_DBUS_DAEMON").filter(|d| !d.is_empty())
            .or_else(|| which("dbus-daemon", &var(&self.env, "PATH").unwrap_or_default()))
            .ok_or("zone bus: no dbus-daemon on this host (install dbus) for bus = \"zone\"")?;
        let dir = self.zbus_dir();
        let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir);
        let _ = std::fs::remove_file(format!("{dir}/bus"));
        std::fs::write(format!("{dir}/session.conf"), ZBUS_CONF.replace("{dir}", &dir))
            .map_err(|e| format!("zone bus: {dir}: {}", errno_text(&e)))?;
        self.service_links(&dir);
        let (r, w) = pipe().map_err(|e| format!("zone bus: {}", errno_text(&e)))?;
        let wfd = w.as_raw_fd();
        let mut cmd = Command::new(&daemon);
        cmd.args([format!("--config-file={dir}/session.conf"), "--nofork".into(), "--nopidfile".into(),
                  "--print-address=3".into()])
            .env_clear().envs(&self.env).env("DBUS_SESSION_BUS_ADDRESS", format!("unix:path={dir}/bus"))
            .stdin(Stdio::null());
        // Safety: child_setup and dup2/fcntl are async-signal-safe; the write end goes to fd 3.
        unsafe {
            cmd.pre_exec(move || {
                child_setup(false)?;
                if wfd == 3 {
                    libc::fcntl(3, libc::F_SETFD, 0);
                } else if libc::dup2(wfd, 3) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            })
        };
        let spawned = cmd.spawn();
        drop(cmd);
        drop(w);
        let pid = match spawned {
            Ok(child) => child.id() as i32,
            Err(e) => return Err(format!("zone bus: {daemon}: {}", errno_text(&e))),
        };
        let mut pfd = libc::pollfd { fd: r.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        // Safety: poll on one descriptor we own, then a read into our buffer.
        let mut line = [0u8; 4096];
        let n = unsafe {
            if libc::poll(&mut pfd, 1, 5000) == 1 { libc::read(r.as_raw_fd(), line.as_mut_ptr().cast(), line.len()) } else { 0 }
        };
        let addr = if n > 0 { String::from_utf8_lossy(&line[..n as usize]).trim().to_string() } else { String::new() };
        if addr.is_empty() {
            // never a second daemon next to one that is still starting
            // Safety: SIGKILL and a wait for our own child.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
                libc::waitpid(pid, std::ptr::null_mut(), 0);
            }
            return Err("zone bus: dbus-daemon did not start within 5 s".into());
        }
        log(&format!("zone bus up ({addr}), pid {pid}"));
        self.zbus = Some(pid);
        Ok(())
    }

    /// Link the activatable services the zone may start on its own bus.
    fn service_links(&self, dir: &str) {
        let out = format!("{dir}/services");
        let _ = std::fs::create_dir_all(&out);
        let dirs = var(&self.env, "XDG_DATA_DIRS").unwrap_or_else(|| "/usr/share".into());
        for d in dirs.split(':') {
            let sd = Path::new(d).join("dbus-1/services");
            let Ok(entries) = std::fs::read_dir(&sd) else { continue };
            let mut names: Vec<String> = entries.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
            names.sort();
            for n in names {
                let src = sd.join(&n);
                let Ok(text) = std::fs::read(&src) else { continue };
                let text = String::from_utf8_lossy(&text);
                let name = text.lines().filter_map(|l| l.strip_prefix("Name=")).last().map(str::trim).unwrap_or("");
                if !n.ends_with(".service") || name.is_empty() || ZBUS_SKIP.iter().any(|s| name.starts_with(s)) {
                    continue;
                }
                let dst = Path::new(&out).join(&n);
                if dst.symlink_metadata().is_err() {
                    // the first data dir wins, as for the host bus
                    if let Ok(real) = std::fs::canonicalize(&src) {
                        let _ = std::os::unix::fs::symlink(real, dst);
                    }
                }
            }
        }
    }

    /// ARGV wrapped so the app sees no bus socket at all: a nested bwrap whose
    /// $XDG_RUNTIME_DIR holds the zone's other sockets, not bus and not zone-bus.
    fn no_bus(&self, argv: Vec<String>) -> Vec<String> {
        let bwrap = var(&self.env, "ZONE_BWRAP").filter(|b| !b.is_empty()).unwrap_or_else(|| "bwrap".into());
        let zrt = &self.zrt;
        let mut a = vec![bwrap, "--dev-bind".into(), "/".into(), "/".into(), "--perms".into(), "0700".into(),
                         "--tmpfs".into(), zrt.clone()];
        let mut names: Vec<String> = std::fs::read_dir(zrt).map(|it| {
            it.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()
        }).unwrap_or_default();
        names.sort();
        for n in names.iter().filter(|n| *n != "bus" && *n != "zone-bus") {
            a.extend(["--bind".into(), format!("{zrt}/{n}"), format!("{zrt}/{n}")]);
        }
        a.extend(["--tmpfs".into(), "/run/zone".into()]);
        for n in ["bin", "links.sock"] {
            let p = format!("/run/zone/{n}");
            if Path::new(&p).symlink_metadata().is_ok() {
                a.extend(["--bind".into(), p.clone(), p]);
            }
        }
        a.extend(["--remount-ro".into(), "/run/zone".into(), "--unsetenv".into(), "DBUS_SESSION_BUS_ADDRESS".into(),
                  "--".into()]);
        a.extend(argv);
        a
    }
}

/// Whether child PID has not exited yet (a zombie counts as exited); it is not reaped.
fn alive(pid: i32) -> bool {
    // Safety: waitid with WNOWAIT only looks at our child's state, into a local siginfo.
    unsafe {
        let mut info: libc::siginfo_t = std::mem::zeroed();
        let r = libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, libc::WEXITED | libc::WNOHANG | libc::WNOWAIT);
        r == 0 && info.si_pid() == 0
    }
}

/// All of DATA to FD, waiting while the pty is full; false on an error.
fn write_all_fd(fd: RawFd, mut data: &[u8]) -> bool {
    while !data.is_empty() {
        // Safety: write from our slice to a descriptor we hold.
        let n = unsafe { libc::write(fd, data.as_ptr().cast(), data.len()) };
        if n > 0 {
            data = &data[n as usize..];
        } else if n < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock {
            let mut p = libc::pollfd { fd, events: libc::POLLOUT, revents: 0 };
            // Safety: poll on one descriptor.
            unsafe { libc::poll(&mut p, 1, 1000) };
        } else {
            return false;
        }
    }
    true
}

fn which(name: &str, path: &str) -> Option<String> {
    path.split(':').filter(|d| !d.is_empty()).map(|d| format!("{d}/{name}")).find(|p| {
        std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
    })
}

fn cmdline(pid: &str) -> Option<String> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let joined: Vec<u8> = raw.iter().map(|&b| if b == 0 { b' ' } else { b }).collect();
    Some(String::from_utf8_lossy(&joined).trim().to_string())
}

fn pids() -> Vec<i32> {
    let mut out: Vec<i32> = std::fs::read_dir("/proc").map(|it| {
        it.flatten().filter_map(|e| e.file_name().to_str().and_then(|s| s.parse().ok())).collect()
    }).unwrap_or_default();
    out.sort_unstable();
    out
}

fn procs() -> Vec<Value> {
    pids().into_iter().filter_map(|p| cmdline(&p.to_string()).map(|c| json!([p, c]))).collect()
}

/// [[path, "ro"|"rw"], ...] for the runtime mounts under /mnt/host, as this zone sees them.
fn host_mounts() -> Vec<Value> {
    let text = std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    text.lines().filter_map(|line| {
        let f: Vec<&str> = line.split_whitespace().collect();
        let mp = f.get(4)?.replace("\\040", " ");
        let ro = f.get(5)?.split(',').any(|o| o == "ro");
        mp.starts_with("/mnt/host/").then(|| json!([mp, if ro { "ro" } else { "rw" }]))
    }).collect()
}

/// The path of a /proc/PID/maps line: what follows the fifth field (it may hold spaces).
fn maps_path(line: &str) -> &str {
    let mut rest = line;
    for _ in 0..5 {
        rest = rest.trim_start();
        match rest.find(char::is_whitespace) {
            Some(i) => rest = &rest[i..],
            None => return "",
        }
    }
    rest.trim()
}

/// [[pid, cmdline, what], ...]: processes in the zone with a cwd, root, executable, open
/// file or mapped file under PATH.
fn busy(path: &str) -> Vec<Value> {
    let under = |t: &str| t == path || t.starts_with(&format!("{path}/"));
    let mut out = Vec::new();
    for p in pids() {
        let mut hits = Vec::new();
        let mut links = vec![("cwd".to_string(), format!("/proc/{p}/cwd")), ("root".into(), format!("/proc/{p}/root")),
                             ("exe".into(), format!("/proc/{p}/exe"))];
        if let Ok(fds) = std::fs::read_dir(format!("/proc/{p}/fd")) {
            for fd in fds.flatten() {
                let n = fd.file_name().to_string_lossy().into_owned();
                links.push((format!("fd {n}"), format!("/proc/{p}/fd/{n}")));
            }
        }
        for (what, link) in links {
            if let Ok(t) = std::fs::read_link(&link) {
                let t = t.to_string_lossy().into_owned();
                if under(&t) {
                    hits.push(format!("{what} {t}"));
                }
            }
        }
        if let Ok(maps) = std::fs::read_to_string(format!("/proc/{p}/maps")) {
            for line in maps.lines() {
                let t = maps_path(line);
                if !t.is_empty() && under(t) {
                    hits.push(format!("maps {t}"));
                    break;
                }
            }
        }
        if !hits.is_empty() {
            let cmd = cmdline(&p.to_string()).unwrap_or_else(|| "?".into());
            let shown = hits.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
            let more = if hits.len() > 3 { format!(" (+{})", hits.len() - 3) } else { String::new() };
            out.push(json!([p, cmd, format!("{shown}{more}")]));
        }
    }
    out
}

/// The descriptor the host passed in $NAME, close-on-exec from now on (never into an
/// app); the variable is removed, so apps do not see it either.
fn take_fd(name: &str) -> Option<RawFd> {
    let raw = std::env::var(name).ok();
    std::env::remove_var(name);
    let fd: RawFd = raw?.parse().ok()?;
    set_cloexec(fd);
    Some(fd)
}

/// Named `zone-init` (comm), then not dumpable: processes in the zone (same uid, no
/// capabilities) cannot ptrace zone-init, read its memory or take its fds, so they cannot
/// use its control socket. Apps it starts are dumpable again after exec.
fn harden_self() -> bool {
    let name = CString::new("zone-init").unwrap_or_default();
    // Safety: prctl with a NUL-terminated name we own, and a plain flag.
    unsafe {
        libc::prctl(libc::PR_SET_NAME, name.as_ptr() as libc::c_ulong, 0, 0, 0);
        libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) == 0
    }
}

/// SIGCHLD and SIGTERM blocked before any thread starts, so every thread has them blocked;
/// a signalfd for each: SIGCHLD for the poll loop, SIGTERM for the shutdown watcher.
fn signal_fds() -> Option<(OwnedFd, OwnedFd)> {
    // Safety: sigsets on the stack, sigprocmask on this thread, new signalfds we own.
    unsafe {
        let mut both: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut both);
        libc::sigaddset(&mut both, libc::SIGCHLD);
        libc::sigaddset(&mut both, libc::SIGTERM);
        if libc::sigprocmask(libc::SIG_BLOCK, &both, std::ptr::null_mut()) != 0 {
            return None;
        }
        let one = |sig: libc::c_int| {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, sig);
            let fd = libc::signalfd(-1, &set, libc::SFD_CLOEXEC | libc::SFD_NONBLOCK);
            (fd >= 0).then(|| OwnedFd::from_raw_fd(fd))
        };
        Some((one(libc::SIGCHLD)?, one(libc::SIGTERM)?))
    }
}

/// Ends the zone at once on SIGTERM or when the server's pidfd fires, in a thread of its
/// own: a shell client that stops reading, or a log nobody reads, never holds it up.
fn watch_shutdown(term: OwnedFd, server: RawFd) -> bool {
    std::thread::Builder::new().name("shutdown".into()).spawn(move || loop {
        let mut fds = [libc::pollfd { fd: term.as_raw_fd(), events: libc::POLLIN, revents: 0 },
                       libc::pollfd { fd: server, events: libc::POLLIN, revents: 0 }];
        // Safety: poll over our array; _exit ends the process without running anything.
        unsafe {
            if libc::poll(fds.as_mut_ptr(), 2, -1) > 0 && (fds[0].revents != 0 || fds[1].revents != 0) {
                libc::_exit(0);
            }
        }
    }).is_ok()
}

pub fn main(_args: &[String]) -> u8 {
    if !harden_self() {
        log("could not make zone-init non-dumpable; refusing to run");
        return 1;
    }
    let Some(ctl) = take_fd("ZONE_CTL_FD") else {
        log("no $ZONE_CTL_FD from the host; refusing to run");
        return 1;
    };
    let Some(server) = take_fd("ZONE_SERVER_FD") else {
        log("no $ZONE_SERVER_FD from the host; refusing to run");
        return 1;
    };
    let Some((signals, term)) = signal_fds() else {
        log("could not set up signals; refusing to run");
        return 1;
    };
    if !watch_shutdown(term, server) {
        log("could not start the shutdown watcher; refusing to run");
        return 1;
    }
    // vars_os: a value that is not UTF-8 (a locale's, a user's `env`) must not stop the zone
    let env: Env = std::env::vars_os().collect();
    let zone = var(&env, "ZONE").unwrap_or_else(|| "?".into());
    // Safety: getuid has no preconditions.
    let zrt = var(&env, "XDG_RUNTIME_DIR").unwrap_or_else(|| format!("/run/user/{}", unsafe { libc::getuid() }));
    if let Some(addr) = var(&env, "ZONE_DNS").filter(|a| !a.is_empty()) {
        zone_dns::start(&addr, log);
    }
    let mut init = Init { zone: zone.clone(), zrt, env, waiting: HashMap::new(), shells: HashMap::new(),
                          outputs: HashMap::new(), tails: HashMap::new(), running: HashSet::new(),
                          finished: VecDeque::new(), zbus: None };
    log(&format!("zone {zone} ready (control socket: fd from the host)"));
    loop {
        let mut fds = vec![
            libc::pollfd { fd: ctl, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: signals.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        ];
        let mut tags = vec![Tag::Listener, Tag::Signals];
        for fd in init.outputs.keys() {
            fds.push(libc::pollfd { fd: *fd, events: libc::POLLIN, revents: 0 });
            tags.push(Tag::Out(*fd));
        }
        for (pid, sh) in &init.shells {
            fds.push(libc::pollfd { fd: sh.conn.as_raw_fd(), events: libc::POLLIN, revents: 0 });
            tags.push(Tag::ShellIn(*pid));
            fds.push(libc::pollfd { fd: sh.master.as_raw_fd(), events: libc::POLLIN, revents: 0 });
            tags.push(Tag::ShellOut(*pid));
        }
        // Safety: poll over our own array of pollfds.
        let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) };
        if n < 0 {
            continue; // EINTR
        }
        for (pfd, tag) in fds.iter().zip(tags) {
            if pfd.revents == 0 {
                continue;
            }
            match tag {
                Tag::Listener => {
                    // Safety: accept4 returns a new connected socket we own.
                    let c = unsafe { libc::accept4(ctl, std::ptr::null_mut(), std::ptr::null_mut(), libc::SOCK_CLOEXEC) };
                    if c >= 0 {
                        init.handle(unsafe { UnixStream::from_raw_fd(c) });
                    }
                }
                Tag::Signals => {
                    let mut info = [0u8; 128];
                    // Safety: reads whole signalfd_siginfo records (SIGCHLD) into our buffer.
                    while unsafe { libc::read(signals.as_raw_fd(), info.as_mut_ptr().cast(), info.len()) } == 128 {}
                    init.reap();
                }
                Tag::Out(fd) => {
                    init.read_output(fd);
                }
                Tag::ShellIn(_) | Tag::ShellOut(_) => init.relay(tag),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_must_be_a_list_of_strings() {
        assert_eq!(argv_of(Some(&json!(["sh", "-c", "x"]))), Some(vec!["sh".into(), "-c".into(), "x".into()]));
        for bad in [json!("sh"), json!([]), json!([1]), json!([["sh"]]), json!(0), json!({}), json!(null)] {
            assert_eq!(argv_of(Some(&bad)), None, "{bad}");
        }
        assert_eq!(argv_of(None), None);
    }

    #[test]
    fn moz_names() {
        assert_eq!(moz_remoting_name("work-2", "firefox-esr"), "firefox_esr_zone_work_2");
    }

    #[test]
    fn stdio_kinds() {
        let null = std::fs::File::open("/dev/null").unwrap();
        assert!(stdio_ok(null.as_raw_fd()));
        let (r, _w) = pipe().unwrap();
        assert!(stdio_ok(r.as_raw_fd()));
        let dir = std::fs::File::open("/").unwrap();
        assert!(!stdio_ok(dir.as_raw_fd()));
        let (a, _b) = UnixStream::pair().unwrap();
        assert!(!stdio_ok(a.as_raw_fd()));
    }

    #[test]
    fn maps_paths() {
        assert_eq!(maps_path("7f00-7f01 r--p 00000000 08:01 1234   /mnt/host/d/my file.so"), "/mnt/host/d/my file.so");
        assert_eq!(maps_path("7f00-7f01 rw-p 00000000 00:00 0 "), "");
        assert_eq!(maps_path("7f00-7f01 rw-p 00000000 00:00 0"), "");
    }

    #[test]
    fn errno_text_without_the_code() {
        assert_eq!(errno_text(&std::io::Error::from_raw_os_error(2)), "No such file or directory");
    }
}
