//! `appzones`: the commands a user types, and the internal ones the server and the zone use.
//! The port of src/zone.py, src/appzones/client.py, mounts.py and upgrade.py, with the same
//! options, outputs, messages and exit statuses. `appzones-helper cli ARGS` (the `appzones`
//! wrapper runs it when appzones is installed with the Rust helper).

use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value as J};
use toml::Value;

use crate::agent;
use crate::config::{self, Config, Zone};
use crate::plan::{self, Host};
use crate::policy::{py_repr, zone_name_ok, RESERVED};
use crate::pyfmt::{close_match, json_dumps_unsorted, py_json};

const HELP: &str = include_str!("help.txt");
const COMMANDS: &[&str] = &["active", "apps", "check", "list", "logs", "mount", "mounts", "new", "ping", "ps", "restart",
    "run", "serve", "shell", "stop", "umount", "up", "which"];
const PALETTE: &[&str] = &["#a6e3a1", "#89b4fa", "#f38ba8", "#f9e2af", "#cba6f7", "#94e2d5", "#fab387", "#f5c2e7",
    "#74c7ec", "#eba0ac", "#b4befe", "#89dceb"];
const START_TIMEOUT: f64 = 30.0;
const ALREADY_SERVED: i32 = 75;
const LOG_MAX: u64 = 2 * 1024 * 1024;
const DBUS_OWN_FAILED: &[&str] = &["Failed to register", "org.freedesktop.DBus.Error.ServiceUnknown",
    "org.freedesktop.DBus.Error.AccessDenied", "Unable to acquire bus name", "Could not acquire name", "RequestName"];

/// `die`: one line on stderr, then the exit status.
fn die_code(msg: &str, code: i32) -> ! {
    eprintln!("appzones: {msg}");
    std::process::exit(code)
}

fn die(msg: &str) -> ! {
    die_code(msg, 1)
}

fn warn(msg: &str) {
    eprintln!("appzones: {msg}");
}

fn sh(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn out(line: &str) {
    let mut o = std::io::stdout().lock();
    let _ = writeln!(o, "{line}");
    let _ = o.flush();
}

pub struct Cli {
    pub host: Host,
    legacy_config: bool,
    /// .state.lock, held shared while this command uses the pre-0.6 state directory (no
    /// move starts under it)
    state_hold: Option<std::fs::File>,
}

/// common.server_command: (subcommand, zone) when ARGV is a zone server of any appzones
/// written in Python (this tree's zone.py or one in the Nix store), for servers from before
/// the lock (0.7) and before 0.6.
fn server_command(argv: &[String], cwd: &str, here: &Path) -> Option<(String, String)> {
    let mut i = 1;
    while i < argv.len() && argv[i].starts_with('-') {
        if argv[i] == "-c" || argv[i] == "-m" {
            return None;
        }
        i += if matches!(argv[i].as_str(), "-X" | "-W" | "-Q") { 2 } else { 1 };
    }
    if i >= argv.len() {
        return None;
    }
    let script = &argv[i];
    let rest = &argv[i + 1..argv.len().min(i + 3)];
    if rest.len() < 2 || !(rest[0] == "serve" || rest[0] == "_supervise") || !script.ends_with("zone.py") {
        return None;
    }
    let path = std::fs::canonicalize(Path::new(cwd).join(script)).unwrap_or_else(|_| Path::new(cwd).join(script));
    let parent = path.parent();
    let installed = parent.and_then(Path::file_name).is_some_and(|n| n == "appzones" || n == "zones")
        && parent.and_then(Path::parent).and_then(Path::file_name).is_some_and(|n| n == "libexec")
        && sh(&path).starts_with("/nix/store/");
    let own = std::fs::canonicalize(here.join("zone.py")).ok();
    // (an appzones from before the lock whose zone.py an upgrade removed, still running it)
    let removed = !path.exists() && path.is_absolute()
        && parent.and_then(Path::file_name).is_some_and(|n| n == "appzones" || n == "zones");
    (own.as_deref() == Some(path.as_path()) || installed || removed).then(|| (rest[0].clone(), rest[1].clone()))
}

fn proc_argv(pid: &str) -> Option<(Vec<String>, String)> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let argv = raw.split(|b| *b == 0).map(|a| String::from_utf8_lossy(a).into_owned()).collect::<Vec<_>>();
    let cwd = std::fs::read_link(format!("/proc/{pid}/cwd")).ok()?;
    Some((argv, sh(&cwd)))
}

fn my_pids() -> Vec<(String, u32)> {
    let me = std::process::id().to_string();
    // Safety: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    std::fs::read_dir("/proc").map(|it| it.filter_map(|e| e.ok()).filter_map(|e| {
        let n = e.file_name().to_string_lossy().into_owned();
        (n.bytes().all(|b| b.is_ascii_digit()) && n != me).then_some(n)
    }).filter_map(|n| std::fs::metadata(format!("/proc/{n}")).ok().filter(|m| m.uid() == uid).map(|_| (n.clone(), uid)))
        .collect()).unwrap_or_default()
}

/// The exit status of our child PID (retried on EINTR); None when there is no such child
/// (fork failed: never waitpid(-1), which would take any child).
fn wait_child(pid: libc::pid_t) -> Option<i32> {
    if pid <= 0 {
        return None;
    }
    let mut status = 0;
    loop {
        // Safety: waiting for our own child into a local.
        if unsafe { libc::waitpid(pid, &mut status, 0) } == pid {
            return Some(status);
        }
        if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return None;
        }
    }
}

fn exited_ok(status: Option<i32>) -> bool {
    status.is_some_and(|st| libc::WIFEXITED(st) && libc::WEXITSTATUS(st) == 0)
}

fn flock_nb(fd: RawFd) -> bool {
    // Safety: flock on a descriptor we own.
    unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

fn open_lock(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new().read(true).write(true).create(true).mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW).open(path)
}

impl Cli {
    pub fn new(host: Host) -> Cli {
        let legacy = config::env("CONFIG").filter(|c| !c.is_empty()).is_none()
            && !host.home.join(".config/appzones/zones.toml").symlink_metadata().is_ok()
            && host.home.join(".config/zones/zones.toml").symlink_metadata().is_ok();
        Cli { host, legacy_config: legacy, state_hold: None }
    }

    fn rt(&self) -> &Path {
        &self.host.rt
    }

    fn config_path(&self) -> String {
        sh(&self.host.config)
    }

    fn load(&self) -> Config {
        config::load(&self.host.config, &self.config_path(), self.host.here.as_os_str().to_owned())
    }

    /// common.load_cfg.
    fn load_cfg(&self, quiet: bool) -> Config {
        let cfg = self.load();
        if !cfg.errors.is_empty() {
            die(&format!("{} is invalid:\n  {}", self.config_path(), cfg.errors.join("\n  ")));
        }
        if !quiet {
            for w in &cfg.warnings {
                warn(&format!("warning: {w}"));
            }
        }
        cfg
    }

    /// common.load_zone (the zone's name checked; the whole config returned).
    fn load_zone(&self, name: &str, quiet: bool) -> Config {
        let cfg = self.load_cfg(quiet);
        if cfg.zone(name).is_none() {
            let names: Vec<&str> = cfg.zones.iter().map(|z| z.name.as_str()).collect();
            die(&format!("no zone {} in {} (zones: {})", py_repr(name), self.config_path(),
                         if names.is_empty() { "none".to_string() } else { names.join(", ") }));
        }
        cfg
    }

    fn answers(&self, name: &str) -> bool {
        agent::answers(self.rt(), name)
    }

    fn log_dir(&self) -> PathBuf {
        match std::env::var_os("XDG_STATE_HOME").filter(|s| s.as_bytes().starts_with(b"/")) {
            Some(s) => PathBuf::from(s).join("appzones"),
            None => self.host.home.join(".local/state/appzones"),
        }
    }

    fn log_path(&self, name: &str) -> PathBuf {
        self.log_dir().join(format!("{name}.log"))
    }

    fn lock_path(&self, name: &str) -> PathBuf {
        if !zone_name_ok(name) {
            die(&format!("not a zone name: {}", py_repr(name)));
        }
        self.rt().join(format!("{name}.lock"))
    }

    fn mkdir_rt(&self) {
        let _ = std::fs::create_dir_all(self.rt());
        let _ = std::fs::set_permissions(self.rt(), std::fs::Permissions::from_mode(0o700));
    }

    // ── systemd ──────────────────────────────────────────────────────────

    fn systemctl(&self, args: &[&str], check: bool) -> (i32, String, String) {
        let o = Command::new("systemctl").arg("--user").args(args).stdin(Stdio::null()).output();
        let (code, so, se) = match o {
            Ok(o) => (o.status.code().unwrap_or(1), String::from_utf8_lossy(&o.stdout).into_owned(),
                      String::from_utf8_lossy(&o.stderr).into_owned()),
            Err(e) if check => die(&format!("systemctl --user {}: {e}", args.join(" "))),
            Err(_) => (127, String::new(), String::new()),
        };
        if check && code != 0 {
            die(&format!("systemctl --user {}: {}", args.join(" "),
                         if se.trim().is_empty() { code.to_string() } else { se.trim().to_string() }));
        }
        (code, so, se)
    }

    fn unit_installed(&self) -> bool {
        let (code, so, _) = self.systemctl(&["show", "-p", "LoadState", "--value", "appzones@probe.service"], false);
        code == 0 && so.trim() == "loaded"
    }

    /// common.supervisor.
    fn supervisor(&self, z: Option<&Zone>) -> String {
        if let Some(s) = z.and_then(|z| z.get("supervisor").as_str()).filter(|s| *s != "auto") {
            return s.to_string();
        }
        let v = config::env("SUPERVISOR").unwrap_or_else(|| "auto".into()).trim().to_lowercase();
        let v = if v == "none" { "spawn".to_string() } else { v };
        if !["auto", "systemd", "spawn"].contains(&v.as_str()) {
            die(&format!("APPZONES_SUPERVISOR={}: use one of auto, systemd, spawn", py_repr(&v)));
        }
        v
    }

    fn use_systemd(&self, z: Option<&Zone>) -> bool {
        let s = self.supervisor(z);
        s == "systemd" || (s == "auto" && self.unit_installed())
    }

    fn unit_runs(&self, name: &str) -> bool {
        let (_, so, _) = self.systemctl(&["is-active", &format!("appzones@{name}.service")], false);
        ["active", "activating", "deactivating", "reloading", "refreshing"].contains(&so.trim())
    }

    fn check_manager_env(&self) {
        if std::env::var_os("WAYLAND_DISPLAY").is_none_or(|w| w.is_empty()) {
            return;
        }
        let (code, so, _) = self.systemctl(&["show-environment"], false);
        if code == 0 && !so.lines().any(|l| l.starts_with("WAYLAND_DISPLAY=")) {
            warn("the systemd user manager has no WAYLAND_DISPLAY, so zones it starts have no Wayland; import it \
                  (`systemctl --user import-environment WAYLAND_DISPLAY`, or start the session with uwsm), or set \
                  APPZONES_SUPERVISOR=spawn");
        }
    }

    // ── servers ──────────────────────────────────────────────────────────

    fn lock_id(&self, name: &str) -> Option<(u64, u64)> {
        let m = std::fs::symlink_metadata(self.lock_path(name)).ok()?;
        Some((m.dev(), m.ino()))
    }

    fn holds(pid: &str, lock: (u64, u64)) -> bool {
        let Ok(it) = std::fs::read_dir(format!("/proc/{pid}/fd")) else { return false };
        it.filter_map(|e| e.ok()).any(|e| std::fs::metadata(e.path()).is_ok_and(|m| (m.dev(), m.ino()) == lock))
    }

    /// Whether PID holds the flock on the file LOCK (dev, ino), not merely has it open (an
    /// `appzones up` between its open and its flock): by the descriptor's own fdinfo,
    /// which lists the locks held through it.
    fn holds_lock(pid: &str, lock: (u64, u64)) -> bool {
        let Ok(it) = std::fs::read_dir(format!("/proc/{pid}/fd")) else { return false };
        it.filter_map(|e| e.ok()).filter(|e| std::fs::metadata(e.path()).is_ok_and(|m| (m.dev(), m.ino()) == lock))
            .any(|e| std::fs::read_to_string(format!("/proc/{pid}/fdinfo/{}", e.file_name().to_string_lossy()))
                .is_ok_and(|t| t.lines().any(|l| l.starts_with("lock:") && l.contains("FLOCK"))))
    }

    fn is_server(&self, pid: &str, name: &str, lock: Option<(u64, u64)>) -> bool {
        // Safety: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        if std::fs::metadata(format!("/proc/{pid}")).map(|m| m.uid()).ok() != Some(uid) {
            return false;
        }
        if let Some(l) = lock.or_else(|| self.lock_id(name)) {
            if Self::holds_lock(pid, l) {
                return true;
            }
        }
        proc_argv(pid).and_then(|(a, c)| server_command(&a, &c, &self.host.here)).is_some_and(|(_, z)| z == name)
    }

    /// $XDG_RUNTIME_DIR/appzones/.state.lock: servers hold it shared, the move of the
    /// pre-0.6 state directory takes it exclusively (close-on-exec: never in a zone).
    fn state_lock(&self) -> Option<std::fs::File> {
        self.mkdir_rt();
        open_lock(&self.rt().join(".state.lock")).ok()
    }

    /// The zones whose lock file a process of this user holds open (a server runs them, or
    /// a client is starting one): by the processes' descriptors, never by taking the lock,
    /// which would make a server starting in that instant give up.
    fn locked_zones(&self) -> Vec<String> {
        let Ok(it) = std::fs::read_dir(self.rt()) else { return vec![] };
        it.filter_map(|e| e.ok()).filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            let zone = n.strip_suffix(".lock").filter(|z| zone_name_ok(z))?.to_string();
            let m = std::fs::symlink_metadata(e.path()).ok().filter(|m| m.is_file())?;
            let lock = (m.dev(), m.ino());
            my_pids().iter().any(|(p, _)| Self::holds(p, lock)).then_some(zone)
        }).collect()
    }

    /// client.server_pids.
    fn server_pids(&self, name: &str) -> Vec<i32> {
        let lock = self.lock_id(name);
        my_pids().into_iter().filter(|(p, _)| self.is_server(p, name, lock)).filter_map(|(p, _)| p.parse().ok()).collect()
    }

    /// SIGINT (stop for good) to zone NAME's servers outside its unit, then until they exit.
    fn signal_servers(&self, name: &str) {
        let mut pfds: Vec<RawFd> = Vec::new();
        for pid in self.server_pids(name) {
            // Safety: pidfd_open of a pid; the descriptor is ours.
            let pfd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as RawFd;
            if pfd < 0 {
                continue;
            }
            if self.is_server(&pid.to_string(), name, None) {
                // Safety: a signal through the pidfd, which pins the process.
                let rc = unsafe { libc::syscall(libc::SYS_pidfd_send_signal, pfd, libc::SIGINT, std::ptr::null::<u8>(), 0) };
                if rc == 0 {
                    pfds.push(pfd);
                    continue;
                }
                let e = std::io::Error::last_os_error();
                if e.raw_os_error() != Some(libc::ESRCH) {
                    let t = e.to_string();
                    warn(&format!("zone {name}: could not signal its server (pid {pid}): {}",
                                  t.split(" (os error").next().unwrap_or(&t)));
                }
            }
            // Safety: closing our pidfd.
            unsafe { libc::close(pfd) };
        }
        let end = Instant::now() + Duration::from_secs(20);
        while !pfds.is_empty() {
            let mut polls: Vec<libc::pollfd> = pfds.iter().map(|f| libc::pollfd { fd: *f, events: libc::POLLIN, revents: 0 }).collect();
            let left = end.saturating_duration_since(Instant::now()).as_millis() as libc::c_int;
            // Safety: poll on our pidfds.
            let n = unsafe { libc::poll(polls.as_mut_ptr(), polls.len() as libc::nfds_t, left) };
            if n <= 0 {
                for f in &pfds {
                    // Safety: closing our pidfds.
                    unsafe { libc::close(*f) };
                }
                die(&format!("zone {name}: its server did not exit within 20 s after SIGINT"));
            }
            for p in polls.iter().filter(|p| p.revents != 0) {
                // Safety: closing a pidfd whose process has exited.
                unsafe { libc::close(p.fd) };
                pfds.retain(|f| *f != p.fd);
            }
        }
    }

    // ── starting ─────────────────────────────────────────────────────────

    fn open_log(&self, name: &str) -> std::io::Result<std::fs::File> {
        std::fs::create_dir_all(self.log_dir())?;
        let path = self.log_path(name);
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > LOG_MAX) {
            let _ = std::fs::rename(&path, path.with_file_name(format!("{name}.log.1")));
        }
        std::fs::OpenOptions::new().append(true).create(true).mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW).open(path)
    }

    fn log_tail(&self, name: &str, since: u64) -> String {
        let Ok(mut f) = std::fs::File::open(self.log_path(name)) else { return String::new() };
        let size = f.metadata().map(|m| m.len()).unwrap_or(0);
        use std::io::Seek;
        let _ = f.seek(std::io::SeekFrom::Start(since.max(size.saturating_sub(16384))));
        let mut b = Vec::new();
        let _ = f.read_to_end(&mut b);
        let text = String::from_utf8_lossy(&b).into_owned();
        let lines: Vec<&str> = crate::policy::split_lines(&text);
        lines[lines.len().saturating_sub(15)..].iter().map(|l| format!("  {l}")).collect::<Vec<_>>().join("\n")
    }

    /// client._start_server: `appzones serve NAME` detached (its own session, cwd /, the
    /// log for output), with the LOCK we hold and a readiness pipe.
    fn start_server(&self, name: &str, lock: &std::fs::File) -> (std::fs::File, u64) {
        if !zone_name_ok(name) {
            die(&format!("not a zone name: {}", py_repr(name)));
        }
        let log = self.open_log(name).unwrap_or_else(|e| {
            let t = e.to_string();
            die(&format!("zone {name}: cannot open its log {}: {}", sh(&self.log_path(name)),
                         t.split(" (os error").next().unwrap_or(&t)))
        });
        let since = log.metadata().map(|m| m.len()).unwrap_or(0);
        let mut p = [0 as RawFd; 2];
        // Safety: pipe2 into our array, close-on-exec (the server gets its end below).
        if unsafe { libc::pipe2(p.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            die(&format!("zone {name}: could not start its server: {}", std::io::Error::last_os_error()));
        }
        // Safety: the two new descriptors are ours.
        let (r, w) = unsafe { (std::fs::File::from_raw_fd(p[0]), std::fs::File::from_raw_fd(p[1])) };
        let exe = std::env::current_exe().unwrap_or_else(|e| die(&format!("own path: {e}")));
        let (lfd, wfd) = (lock.as_raw_fd(), w.as_raw_fd());
        let mut cmd = Command::new(&exe);
        cmd.args(["cli", "serve", name, "--lock-fd", &lfd.to_string(), "--notify-fd", &wfd.to_string()])
            .stdin(Stdio::null()).stdout(log.try_clone().unwrap_or_else(|e| die(&e.to_string())))
            .stderr(log.try_clone().unwrap_or_else(|e| die(&e.to_string()))).current_dir("/")
            .env_remove("NOTIFY_SOCKET").env_remove("LISTEN_FDS").env_remove("LISTEN_PID")
            .env("APPZONES_CONFIG", std::path::absolute(&self.host.config).unwrap_or_else(|_| self.host.config.clone()))
            .env_remove("ZONES_CONFIG");
        for key in ["STATE", "DENIED_PATHS"] {
            if let Some(v) = config::env(key).filter(|v| !v.is_empty()) {
                cmd.env(format!("APPZONES_{key}"), std::path::absolute(&v).unwrap_or_else(|_| PathBuf::from(&v)));
                cmd.env_remove(format!("ZONES_{key}"));
            }
        }
        // Safety: between fork and exec only async-signal-safe calls: a session of its own,
        // and the lock and the pipe's write end kept (at their numbers) for the server.
        unsafe {
            cmd.pre_exec(move || {
                libc::setsid();
                for fd in [lfd, wfd] {
                    let f = libc::fcntl(fd, libc::F_GETFD);
                    libc::fcntl(fd, libc::F_SETFD, f & !libc::FD_CLOEXEC);
                }
                Ok(())
            });
        }
        // an intermediate process starts it and goes, so the server is never our child
        // Safety: fork in a process with no other threads; the child only spawns and exits.
        let pid = unsafe { libc::fork() };
        if pid == 0 {
            let ok = cmd.spawn().is_ok();
            // Safety: leaving the intermediate at once.
            unsafe { libc::_exit(if ok { 0 } else { 1 }) };
        }
        drop(w);
        drop(log);
        if pid < 0 {
            die(&format!("zone {name}: could not start its server: {}", std::io::Error::last_os_error()));
        }
        if !exited_ok(wait_child(pid)) {
            die(&format!("zone {name}: could not start its server"));
        }
        (r, since)
    }

    /// client.claim: start zone NAME's server if nobody runs or starts it.
    fn claim(&self, name: &str, quiet: bool, wait: bool) -> Option<(std::fs::File, u64)> {
        let path = self.lock_path(name);
        self.mkdir_rt();
        let started = Instant::now();
        let mut noted = false;
        let mut limit: Option<f64> = None;
        loop {
            if self.answers(name) {
                return None;
            }
            let lock = open_lock(&path).unwrap_or_else(|e| die(&format!("{}: {e}", sh(&path))));
            if !flock_nb(lock.as_raw_fd()) {
                drop(lock);
                if !wait {
                    return None;
                }
                let limit = *limit.get_or_insert_with(|| {
                    let cfg = self.load_zone(name, true);
                    2.0 * config::duration(cfg.zone(name).expect("checked").get("start_timeout")).unwrap_or(START_TIMEOUT)
                });
                if started.elapsed().as_secs_f64() > limit {
                    let pids = self.server_pids(name);
                    die(&format!("zone {name} is held by a server{} but does not answer; see `appzones logs {name}`",
                                 if pids.is_empty() { String::new() } else {
                                     format!(" (pid {})", pids.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ")) }));
                }
                if !noted && !quiet && started.elapsed() > Duration::from_secs(2) {
                    warn(&format!("waiting for zone {name} to start"));
                    noted = true;
                }
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            if self.answers(name) {
                return None;
            }
            if !quiet {
                warn(&format!("starting zone {name}"));
            }
            return Some(self.start_server(name, &lock));
        }
    }

    /// client.wait_ready: until the server signals readiness, or dies with its log's end.
    fn wait_ready(&self, name: &str, mut r: std::fs::File, since: u64, quiet: bool) {
        let mut note = Some(Instant::now() + Duration::from_secs(3));
        loop {
            let mut pfd = libc::pollfd { fd: r.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            // Safety: poll on the pipe we own.
            let n = unsafe { libc::poll(&mut pfd, 1, 1000) };
            if n > 0 {
                let mut b = [0u8; 1];
                if r.read(&mut b).ok() == Some(1) && b[0] == b'\n' {
                    return;
                }
                die(&format!("zone {name} did not start; the end of its log ({}):\n{}", sh(&self.log_path(name)),
                             self.log_tail(name, since)));
            }
            if !quiet && note.is_some_and(|t| Instant::now() > t) {
                warn(&format!("zone {name} is still starting (Ctrl-C leaves it starting; log: {})",
                              sh(&self.log_path(name))));
                note = None;
            }
        }
    }

    fn spawn(&self, name: &str, quiet: bool) {
        if let Some((r, since)) = self.claim(name, quiet, true) {
            self.wait_ready(name, r, since, quiet);
        }
    }

    /// client.up.
    fn up(&self, name: &str, quiet: bool) {
        if self.answers(name) {
            return;
        }
        let cfg = self.load_zone(name, true);
        let z = cfg.zone(name).expect("checked");
        if !self.use_systemd(Some(z)) {
            self.spawn(name, quiet);
            return;
        }
        if !quiet {
            warn(&format!("starting zone {name}"));
        }
        self.check_manager_env();
        let (code, _, se) = self.systemctl(&["start", &format!("appzones@{name}.service")], false);
        if code != 0 {
            die(&format!("zone {name} failed to start; see `appzones logs {name}`\n{}", se.trim()));
        }
        for _ in 0..200 {
            if self.answers(name) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        die(&format!("zone {name} did not answer within 10 s; see `appzones logs {name}`"));
    }

    fn stop_zone(&self, name: &str) {
        if self.unit_runs(name) {
            self.systemctl(&["stop", &format!("appzones@{name}.service")], true);
        }
        self.signal_servers(name);
        for _ in 0..100 {
            if !self.answers(name) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        die(&format!("zone {name} still answers, but appzones found no server of it to stop; stop it where it runs \
                      (SIGTERM or Ctrl-C)"));
    }

    fn restart_zone(&self, name: &str) {
        if self.unit_runs(name) {
            self.systemctl(&["restart", &format!("appzones@{name}.service")], true);
            for _ in 0..200 {
                if self.answers(name) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            die(&format!("zone {name} did not answer within 10 s of its restart; see `appzones logs {name}`"));
        }
        let outside = !self.server_pids(name).is_empty();
        self.stop_zone(name);
        if outside {
            self.load_zone(name, true);
            self.spawn(name, true);
        } else {
            self.up(name, true);
        }
    }

    /// `up|stop|restart ZONE|--all`: for one zone or every one; a failure goes on with the
    /// others. Each runs in a child of ours, so its die() ends only that one.
    fn each_zone(&self, cmd: &str, rest: &[String]) -> i32 {
        let Some(first) = rest.first() else { die(&format!("usage: appzones {cmd} ZONE|--all")) };
        let every = first == "--all";
        let mut names: Vec<String> = if every {
            self.load_cfg(true).zones.iter().map(|z| z.name.clone()).collect()
        } else {
            vec![first.clone()]
        };
        if every && cmd == "stop" {
            // and zones that still run but left the config (their lock is held)
            for z in self.locked_zones() {
                if !names.contains(&z) {
                    names.push(z);
                }
            }
        }
        let run = |n: &str| match cmd {
            "up" => self.up(n, false),
            "stop" => self.stop_zone(n),
            _ => self.restart_zone(n),
        };
        if !every {
            // one zone: here (its failure is our exit status, as Python's SystemExit was); a
            // zone that left the config can still be stopped
            if cmd == "stop" {
                self.lock_path(first); // (a zone name, or the usual refusal)
            } else {
                self.load_zone(first, true);
            }
            run(first);
            return 0;
        }
        let mut failed = 0;
        for n in names {
            // (stop --all: only zones something runs; a server whose zone-init does not answer counts)
            if cmd == "stop" && every && !self.answers(&n) && self.server_pids(&n).is_empty() && !self.unit_runs(&n) {
                continue;
            }
            if cmd != "stop" {
                self.load_zone(&n, true);
            }
            // each zone in a child, so its failure ends only that one; it dies with us
            // Safety: fork in a process with no other threads; the child runs the command and exits.
            let parent = std::process::id() as libc::pid_t;
            let pid = unsafe { libc::fork() };
            if pid == 0 {
                // Safety: prctl and getppid in the child.
                unsafe {
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0);
                    if libc::getppid() != parent {
                        libc::_exit(1);
                    }
                }
                run(&n);
                let _ = std::io::stdout().flush();
                // Safety: the child's end.
                unsafe { libc::_exit(0) };
            }
            if !exited_ok(wait_child(pid)) {
                failed += 1;
            }
        }
        i32::from(failed > 0)
    }

    // ── run ──────────────────────────────────────────────────────────────

    fn stale(&self, name: &str, z: &Zone) -> bool {
        std::fs::read_to_string(self.rt().join(name).join("config.sha256"))
            .is_ok_and(|h| h.trim() != config::settings_hash(z))
    }

    fn bind_paths(&self, z: &Zone) -> Vec<String> {
        let policy = config::policy(self.host.here.as_os_str().to_owned());
        z.get("binds").as_array().cloned().unwrap_or_default().iter()
            .filter_map(|b| b.as_table().and_then(|t| t.get("path")).and_then(Value::as_str).map(String::from))
            .map(|p| String::from_utf8_lossy(&policy.expand(p.as_bytes())).into_owned()).collect()
    }

    /// client.outside_paths: arguments that name an existing host file the zone cannot see.
    fn outside_paths(&self, z: &Zone, args: &[String]) -> Vec<String> {
        let visible: Vec<String> = self.bind_paths(z).iter().map(|p| plan::rp(p)).collect();
        args.iter().filter(|a| {
            let p = a.strip_prefix("file://").unwrap_or(a);
            if !p.starts_with('/') || p.starts_with("/nix/store") || p.starts_with("/dev/null") || !Path::new(p).exists() {
                return false;
            }
            let r = plan::rp(p);
            !visible.iter().any(|v| r == *v || r.starts_with(&format!("{v}/")) || v == "/")
        }).cloned().collect()
    }

    fn run(&self, name: &str, words: &[String], wait: bool, restart: bool, bus: Option<String>) -> i32 {
        let cfg = self.load_cfg(true);
        let Some(z) = cfg.zone(name) else { die(&format!("no zone {} in {}", py_repr(name), self.config_path())) };
        if words.is_empty() {
            die("usage: appzones run ZONE APP [ARGS...] | appzones run ZONE -- CMD [ARGS...]");
        }
        let (argv, app): (Vec<String>, Option<String>) = if words[0] == "--" {
            let argv = words[1..].to_vec();
            if argv.is_empty() {
                die("nothing to run after --");
            }
            let base = Path::new(&argv[0]).file_name().map(|b| b.to_string_lossy().into_owned()).unwrap_or_default();
            let app = cfg.apps.iter().find(|(_, a)| a.get("command").and_then(Value::as_array).and_then(|c| c.first())
                .and_then(Value::as_str).is_some_and(|c| Path::new(c).file_name().is_some_and(|f| f.to_string_lossy() == base)))
                .map(|(id, _)| id.clone());
            (argv, app)
        } else {
            match config::resolve_app(&cfg, name, &words[0]) {
                Err(e) => die(&format!("{e}; to run it anyway: appzones run {name} -- {}", words.join(" "))),
                Ok(None) => die(&format!("no app {} in [apps]; to run a command: appzones run {name} -- {}",
                                         py_repr(&words[0]), words.join(" "))),
                Ok(Some(mut base)) => {
                    base.extend(words[1..].iter().cloned());
                    (base, Some(words[0].clone()))
                }
            }
        };
        let bus = bus.unwrap_or_else(|| config::app_bus(&cfg, name, app.as_deref()));
        if bus == "none" && !config::truthy(z.get("userns")) {
            die(&format!("--bus none needs userns = true in zone {name} (the app gets its own mount namespace)"));
        }
        let bad = self.outside_paths(z, &argv[1..]);
        if !bad.is_empty() {
            die(&format!("zone {name} cannot see {} (not inside its binds); move the file into a bind or use the app's \
                          file chooser", bad.join(", ")));
        }
        if self.answers(name) && self.stale(name, z) {
            // (a process list that cannot be had is not an idle zone)
            let n = agent::zone_procs(self.rt(), name).map(|p| p.len()).ok();
            let n_text = n.map(|n| n.to_string()).unwrap_or_else(|| "an unknown number of".into());
            if restart && n == Some(0) {
                warn(&format!("zones.toml changed for {name}; restarting it (no app runs in it)"));
                self.restart_zone(name);
            } else if restart {
                warn(&format!("zones.toml changed for {name}, but {n_text} process(es) run in it; not restarting \
                               (`appzones restart {name}` applies it)"));
            } else {
                warn(&format!("zones.toml changed for {name}; `appzones restart {name}` (or `appzones run --restart`) \
                               applies it"));
            }
        }
        self.up(name, wait);
        let cwd = std::env::current_dir().map(|c| sh(&c)).unwrap_or_default();
        let inside = self.bind_paths(z).iter().any(|b| cwd == *b || cwd.starts_with(&format!("{}/", b.trim_end_matches('/'))));
        let mut env = serde_json::Map::new();
        for k in ["TERM", "MOZ_HEADLESS", "COLUMNS", "LINES"] {
            if let Some(v) = std::env::var_os(k) {
                env.insert(k.into(), J::String(v.to_string_lossy().into_owned()));
            }
        }
        let req = json!({"op": "run", "argv": argv, "wait": wait, "cwd": if inside { J::String(cwd) } else { J::Null },
                         "bus": bus, "env": env});
        let s = if wait {
            // Safety: isatty on our stdio.
            if (0..3).any(|f| unsafe { libc::isatty(f) } == 1) {
                die("--wait passes stdin/stdout/stderr into the zone and refuses a tty; use pipes, or `appzones shell`");
            }
            let bad: Vec<&str> = ["stdin", "stdout", "stderr"].iter().enumerate().filter(|(f, _)| !stdio_fd_ok(*f as RawFd))
                .map(|(_, n)| *n).collect();
            if !bad.is_empty() {
                die(&format!("--wait passes only pipes, regular files and /dev/null into the zone; {} is something else (a \
                              directory or socket would hand the zone a way to the host)", bad.join(", ")));
            }
            agent::request(self.rt(), name, &req, &[0, 1, 2], None)
        } else {
            agent::request(self.rt(), name, &req, &[], Some(Duration::from_secs(5)))
        };
        let s = s.unwrap_or_else(|e| die(&e));
        let (first, rest) = agent::readline(&s, Vec::new()).unwrap_or_else(|e| die(&e));
        if first.get("ok") != Some(&J::Bool(true)) {
            die(first.get("error").and_then(J::as_str).unwrap_or("run failed"));
        }
        if !wait {
            drop(s);
            return self.watch(name, first.get("pid").and_then(J::as_i64).unwrap_or(0), &argv, &bus, app.as_deref());
        }
        match agent::readline(&s, rest) {
            Ok((v, _)) => v.get("exit").and_then(J::as_i64).unwrap_or(255) as i32,
            Err(_) => 255,
        }
    }

    /// client.watch: a launched app watched for a few seconds; an error, said.
    fn watch(&self, name: &str, pid: i64, argv: &[String], bus: &str, app: Option<&str>) -> i32 {
        let secs = config::env("RUN_CHECK").filter(|v| !v.is_empty()).map_or(Some(2.5), |v| v.trim().parse::<f64>().ok())
            .unwrap_or(2.5);
        let end = Instant::now() + Duration::from_secs_f64(secs.max(0.0));
        let mut st: Option<J> = None;
        while Instant::now() < end {
            match agent::ask(self.rt(), name, &json!({"op": "status", "pid": pid}), Duration::from_secs(2)) {
                Ok(v) => {
                    let done = v.get("ok") != Some(&J::Bool(true)) || v.get("running") != Some(&J::Bool(true));
                    st = Some(v);
                    if done {
                        break;
                    }
                }
                Err(_) => return 0,
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let Some(st) = st.filter(|s| s.get("ok") == Some(&J::Bool(true)) && s.get("running") != Some(&J::Bool(true))) else {
            return 0;
        };
        let code = st.get("exit").and_then(J::as_i64).unwrap_or(0) as i32;
        let tail: Vec<String> = st.get("tail").and_then(J::as_array).map(|t| t.iter().filter_map(|l| l.as_str().map(String::from))
            .collect()).unwrap_or_default();
        let label = Path::new(&argv[0]).file_name().map(|b| b.to_string_lossy().into_owned()).unwrap_or_default();
        let refused = tail.iter().any(|l| DBUS_OWN_FAILED.iter().any(|h| l.contains(h)));
        if code == 0 && !refused {
            return 0;
        }
        let how = if code < 0 { format!("was killed by signal {}", -code) } else { format!("exited with status {code}") };
        warn(&format!("{label} {how} right after it started in zone {name} (bus {bus}); its last output:"));
        let shown: Vec<String> = if tail.is_empty() { vec!["(nothing)".into()] } else { tail[tail.len().saturating_sub(12)..].to_vec() };
        for l in shown {
            eprintln!("  {l}");
        }
        if bus == "host" && refused {
            let where_ = match app {
                Some(a) => format!("bus = \"zone\" in [apps.{a}]"),
                None => format!("an [apps] entry for it with bus = \"zone\" (command = [\"{label}\"])"),
            };
            warn(&format!("hint: {label} could not own its D-Bus name. A single-instance app needs one, and a zone never owns \
                           names on the host bus. Give it the zone's own bus: add {where_} to {}, or once: appzones run \
                           --bus zone {name} -- {}", self.config_path(), argv.join(" ")));
        }
        eprintln!("  (the whole log: appzones logs {name})");
        if code > 0 { code } else { 1 }
    }

    /// client.shell: a pty in the zone, this terminal raw until it ends.
    fn shell(&self, name: &str) -> i32 {
        // Safety: isatty on stdin.
        if unsafe { libc::isatty(0) } != 1 {
            die("appzones shell needs a terminal (for a command with pipes: appzones run --wait ZONE -- CMD)");
        }
        self.up(name, false);
        let size = || {
            // Safety: TIOCGWINSZ into a winsize.
            let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
            if unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) } == 0 && ws.ws_col > 0 {
                (ws.ws_col as i64, ws.ws_row as i64)
            } else {
                let env = |k: &str, d: i64| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
                (env("COLUMNS", 80), env("LINES", 24))
            }
        };
        let (cols, rows) = size();
        let term = std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".into());
        let s = agent::request(self.rt(), name, &json!({"op": "shell", "rows": rows, "cols": cols, "term": term}), &[], None)
            .unwrap_or_else(|e| die(&e));
        let (first, rest) = agent::readline(&s, Vec::new()).unwrap_or_else(|e| die(&e));
        if first.get("ok") != Some(&J::Bool(true)) {
            die(first.get("error").and_then(J::as_str).unwrap_or("shell failed"));
        }
        let pid = first.get("pid").cloned().unwrap_or(J::Null);
        let mut stdout = std::io::stdout();
        if !rest.is_empty() {
            let _ = stdout.write_all(&rest);
            let _ = stdout.flush();
        }
        static WINCH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        static HANGUP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        // a byte into this pipe wakes poll: a signal just before it is never missed
        static WAKE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);
        extern "C" fn wake() {
            let fd = WAKE.load(std::sync::atomic::Ordering::SeqCst);
            if fd >= 0 {
                // Safety: write(2) is async-signal-safe; one byte from a static.
                unsafe { libc::write(fd, b"!".as_ptr().cast(), 1) };
            }
        }
        extern "C" fn on_winch(_: libc::c_int) {
            WINCH.store(true, std::sync::atomic::Ordering::SeqCst);
            wake();
        }
        extern "C" fn on_hangup(_: libc::c_int) {
            HANGUP.store(true, std::sync::atomic::Ordering::SeqCst);
            wake();
        }
        let mut wake_pipe = [-1; 2];
        // Safety: pipe2 into a local array.
        if unsafe { libc::pipe2(wake_pipe.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) } == 0 {
            WAKE.store(wake_pipe[1], std::sync::atomic::Ordering::SeqCst);
        }
        // Safety: handlers that only store to an atomic (no SA_RESTART: poll returns, and the
        // terminal is restored below on a hangup or a TERM); termios on our terminal.
        let old = unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = on_winch as extern "C" fn(libc::c_int) as libc::sighandler_t;
            libc::sigaction(libc::SIGWINCH, &sa, std::ptr::null_mut());
            sa.sa_sigaction = on_hangup as extern "C" fn(libc::c_int) as libc::sighandler_t;
            libc::sigaction(libc::SIGHUP, &sa, std::ptr::null_mut());
            libc::sigaction(libc::SIGTERM, &sa, std::ptr::null_mut());
            let mut old: libc::termios = std::mem::zeroed();
            libc::tcgetattr(0, &mut old);
            let mut raw = old;
            libc::cfmakeraw(&mut raw);
            libc::tcsetattr(0, libc::TCSANOW, &raw); // not TCSAFLUSH: keep what was typed meanwhile
            old
        };
        let sock = s.as_raw_fd();
        let mut buf = vec![0u8; 65536];
        // typed but not yet taken by zone-init: while there is some, stdin waits and the
        // socket is written when it can take more (never a blocking write: a zone that stops
        // reading cannot freeze this client with the terminal raw)
        let mut pending: Vec<u8> = Vec::new();
        'outer: loop {
            if HANGUP.load(std::sync::atomic::Ordering::SeqCst) {
                break 'outer;
            }
            if WINCH.swap(false, std::sync::atomic::Ordering::SeqCst) {
                let (c, r) = size();
                let _ = agent::ask(self.rt(), name, &json!({"op": "resize", "pid": pid, "rows": r, "cols": c}),
                                   Duration::from_secs(1));
            }
            let out = if pending.is_empty() { 0 } else { libc::POLLOUT };
            let mut fds = [libc::pollfd { fd: if pending.is_empty() { 0 } else { -1 }, events: libc::POLLIN, revents: 0 },
                           libc::pollfd { fd: sock, events: libc::POLLIN | out, revents: 0 },
                           libc::pollfd { fd: wake_pipe[0], events: libc::POLLIN, revents: 0 }];
            // Safety: poll on stdin, our socket and the wake pipe.
            if unsafe { libc::poll(fds.as_mut_ptr(), 3, -1) } < 0 {
                continue; // EINTR (a resize, a hangup)
            }
            if fds[2].revents != 0 {
                let mut drain = [0u8; 64];
                // Safety: read from our own non-blocking pipe into a local buffer.
                while unsafe { libc::read(wake_pipe[0], drain.as_mut_ptr().cast(), drain.len()) } > 0 {}
                continue; // (the flags are looked at first)
            }
            if !pending.is_empty() && fds[1].revents & libc::POLLOUT != 0 {
                // Safety: a send from our buffer, never blocking.
                let n = unsafe { libc::send(sock, pending.as_ptr().cast(), pending.len(), libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL) };
                if n > 0 {
                    pending.drain(..n as usize);
                } else if n < 0 && !matches!(std::io::Error::last_os_error().kind(),
                                             std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted) {
                    break 'outer;
                }
            }
            if fds[1].revents & !libc::POLLOUT != 0 {
                let n = (&s).read(&mut buf).unwrap_or(0);
                if n == 0 {
                    break 'outer;
                }
                let _ = stdout.write_all(&buf[..n]);
                let _ = stdout.flush();
            }
            if fds[0].revents != 0 {
                // Safety: read from our stdin into our buffer.
                let n = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
                if n <= 0 {
                    break 'outer;
                }
                pending.extend_from_slice(&buf[..n as usize]);
            }
        }
        // Safety: the terminal as it was; the wake pipe closed (its writer cleared first).
        unsafe {
            libc::tcsetattr(0, libc::TCSADRAIN, &old);
            WAKE.store(-1, std::sync::atomic::Ordering::SeqCst);
            for fd in wake_pipe.into_iter().filter(|fd| *fd >= 0) {
                libc::close(fd);
            }
        }
        0
    }

    // ── list, which, new, check, apps, autostart, logs ───────────────────

    fn zone_list(&self, as_json: bool) -> i32 {
        let cfg = self.load_cfg(true);
        let mut rows = vec![["ZONE", "STATE", "NETWORK", "DNS", "DEVICES", "APPS", "COLOUR"].map(String::from)];
        let mut zones = Vec::new();
        for z in &cfg.zones {
            let n = &z.name;
            let st = |k: &str| z.get(k).as_str().unwrap_or("").to_string();
            let net = format!("{}{}", st("network"), if config::truthy(z.get("netbird")) { "+netbird" } else { "" });
            let (state, apps_t, apps_j) = if self.answers(n) {
                let (t, j) = match agent::zone_procs(self.rt(), n) {
                    Ok(p) => (p.len().to_string(), json!(p.len())),
                    Err(_) => ("?".to_string(), J::Null),
                };
                (format!("running{}", if self.stale(n, z) { " (stale)" } else { "" }), t, j)
            } else {
                ("stopped".to_string(), "-".to_string(), J::Null)
            };
            let usb: Vec<String> = z.get("usb").as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let devs: Vec<String> = z.get("devices").as_array().map(|a| a.iter().filter_map(|x| x.as_str()).map(|d| {
                if d == "usb" { format!("usb({})", usb.join("+")) } else { d.to_string() }
            }).collect()).unwrap_or_default();
            rows.push([n.clone(), state.clone(), net.clone(), st("dns"), if devs.is_empty() { "-".into() } else { devs.join(",") },
                       apps_t, st("colour")]);
            zones.push(json!({"name": n, "state": state, "network": net, "dns": st("dns"), "devices": devs, "apps": apps_j,
                              "colour": st("colour")}));
        }
        if as_json {
            out(&py_json(&J::Array(zones)));
            return 0;
        }
        let w: Vec<usize> = (0..7).map(|i| rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0)).collect();
        for r in rows {
            let line: Vec<String> = r.iter().enumerate().map(|(i, c)| format!("{c:<width$}", width = w[i])).collect();
            out(line.join("  ").trim_end());
        }
        0
    }

    fn which(&self, name: &str) -> i32 {
        let cfg = self.load_zone(name, false);
        let z = cfg.zone(name).expect("checked");
        for (k, o) in &z.origin {
            out(&format!("{k:15} {:45} ({o})", json_dumps_unsorted(z.get(k))));
        }
        0
    }

    fn new_block(cfg: &Config, name: &str) -> String {
        let used: Vec<String> = cfg.zones.iter().filter_map(|z| z.get("colour").as_str().map(str::to_lowercase)).collect();
        let colour = PALETTE.iter().find(|c| !used.contains(&c.to_string())).map(|c| c.to_string())
            .unwrap_or_else(|| format!("#{}", random_hex(3)));
        format!("\n[zones.{name}]                 # added by `appzones new`: internet only, gpu, nothing from your home\n\
                 colour    = \"{colour}\"\nnetwork   = \"internet\"\ndns       = \"host\"\ndevices   = [\"gpu\"]\nbinds     = []\n")
    }

    fn new_zone(&self, name: &str, write: bool) -> i32 {
        const NEW_FILE: &str = "# ~/.config/appzones/zones.toml (written by `appzones new`; reference: \
                                https://mirql.github.io/appzones/configuration/reference.html)\nversion = 1\n";
        if !zone_name_ok(name) || RESERVED.contains(&name) {
            let mut r: Vec<&str> = RESERVED.to_vec();
            r.sort();
            die(&format!("zone name {} must match {} and not be one of {}", py_repr(name), config::NAME_PATTERN,
                         crate::pyfmt::repr_strs(&r)));
        }
        let empty = || Config { zones: vec![], apps: toml::Table::new(), errors: vec![], warnings: vec![] };
        if !write {
            let cfg = if self.host.config.symlink_metadata().is_ok() { self.load_cfg(true) } else { empty() };
            if cfg.zone(name).is_some() {
                die(&format!("zone {} already exists", py_repr(name)));
            }
            out(Self::new_block(&cfg, name).trim());
            eprintln!("\n(not written; `appzones new {name} -y` appends it to {})", self.config_path());
            return 0;
        }
        let real = PathBuf::from(plan::rp(&self.config_path()));
        let dir = real.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        let _ = std::fs::create_dir_all(&dir);
        let fname = real.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        let lock = std::fs::OpenOptions::new().append(true).create(true).open(dir.join(format!(".{fname}.lock")))
            .unwrap_or_else(|e| die(&e.to_string()));
        // Safety: a blocking flock on our file.
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) };
        let old = if real.exists() { std::fs::read_to_string(&real).unwrap_or_else(|e| die(&e.to_string())) } else { NEW_FILE.into() };
        let shown = sh(&real);
        let cfg = if old.trim().is_empty() { empty() } else { config::load_text(&old, &shown, self.host.here.as_os_str().to_owned()) };
        if !cfg.errors.is_empty() {
            die(&format!("{} is invalid:\n  {}", self.config_path(), cfg.errors.join("\n  ")));
        }
        if cfg.zone(name).is_some() {
            die(&format!("zone {} already exists", py_repr(name)));
        }
        let block = Self::new_block(&cfg, name);
        let text = format!("{old}{}{block}", if old.ends_with('\n') { "" } else { "\n" });
        let cfg2 = config::load_text(&text, &shown, self.host.here.as_os_str().to_owned());
        if !cfg2.errors.is_empty() {
            die(&format!("the new block would make zones.toml invalid; nothing written:\n  {}", cfg2.errors.join("\n  ")));
        }
        let tmp = dir.join(format!(".{fname}.{}", random_hex(4)));
        let mode = std::fs::metadata(&real).ok().map(|m| m.permissions().mode() & 0o7777);
        let r = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)
            .and_then(|mut f| f.write_all(text.as_bytes()))
            .and_then(|_| match mode { Some(m) => std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(m)), None => Ok(()) })
            .and_then(|_| std::fs::rename(&tmp, &real));
        if let Err(e) = r {
            let _ = std::fs::remove_file(&tmp);
            die(&e.to_string());
        }
        out(block.trim());
        eprintln!("{} {}", if old != NEW_FILE { "appended to" } else { "created" }, sh(&real));
        0
    }

    fn check(&self, probe: Option<&str>) -> i32 {
        let cfg = self.load();
        for e in &cfg.errors {
            eprintln!("error: {e}");
        }
        for w in &cfg.warnings {
            eprintln!("warning: {w}");
        }
        if !cfg.errors.is_empty() {
            return 1;
        }
        out(&format!("ok: {}: {} zones ({}), {} apps (this zone is release {})", self.config_path(), cfg.zones.len(),
                     cfg.zones.iter().map(|z| z.name.as_str()).collect::<Vec<_>>().join(", "), cfg.apps.len(), config::RELEASE));
        let Some(probe) = probe else { return 0 };
        let cfg = self.load_zone(probe, true);
        let z = cfg.zone(probe).expect("checked");
        self.up(probe, true);
        let tcp = |h: &str, p: u16| format!("timeout 3 bash -c '</dev/tcp/{h}/{p}' 2>/dev/null");
        let zrt = self.host.zrt_in();
        let mut probes: Vec<(String, i32, String)> = vec![
            ("~/.ssh absent".into(), 1, "ls -d ~/.ssh".into()),
            ("runtime dir: zone sockets only".into(), 0,
             format!("[ -z \"$(ls -A {zrt} | grep -v -x -e bus -e zone-bus -e wayland-0 -e doc -e pipewire-0 -e pulse)\" ]")),
            ("no cgroupfs".into(), 1, "ls /sys/fs/cgroup".into()),
            ("/nix/var absent".into(), 1, "ls -d /nix/var".into()),
            ("capabilities empty".into(), 0, "grep -q 'CapEff:.0000000000000000' /proc/self/status".into()),
            ("no_new_privs".into(), 0, "grep -q 'NoNewPrivs:.1' /proc/self/status".into()),
            ("no host secrets in env".into(), 1, "env | grep -E 'SSH_AUTH|HYPRLAND|TOKEN|SECRET|PASSWORD'".into()),
        ];
        let policy = config::policy(self.host.here.as_os_str().to_owned());
        let home = sh(&self.host.home);
        for p in policy.host_denied_paths().unwrap_or_default() {
            let p = String::from_utf8_lossy(&p).into_owned();
            if Path::new(&p).exists() && !(p == home || p.starts_with(&format!("{home}/"))) {
                probes.push((format!("{p} absent"), 1, format!("ls -d {}", shell_quote(&p))));
            }
        }
        // (the machine-id: the zone's read inside, compared here; the host's never goes in)
        // network refusals: only where the host itself reaches the target (else the probe
        // would pass whatever the zone may do: SKIP, said so)
        let mut skipped: Vec<String> = Vec::new();
        if z.get("network").as_str() == Some("internet") {
            probes.push(("nft flush refused".into(), 1, "nft flush ruleset 2>/dev/null".into()));
            for (title, host, port) in [("LAN 192.168.0.0/16 refused", "192.168.1.1", 53), ("tailnet 100.64.0.0/10 refused",
                                        "100.64.0.1", 8443), ("host loopback refused", "127.0.0.1", 22)] {
                let addr: std::net::SocketAddr = format!("{host}:{port}").parse().expect("a literal address");
                if std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_ok() {
                    probes.push((title.into(), 1, tcp(host, port)));
                } else {
                    skipped.push(format!("{title} (the host does not reach {host}:{port} either)"));
                }
            }
        }
        let exe = std::env::current_exe().unwrap_or_else(|e| die(&e.to_string()));
        let mut fails = 0;
        for (title, expect, cmd) in probes {
            // the probe says it ran first: a refusal counts only from a probe that ran
            let script = format!("echo appzones-probe-ran; {cmd}");
            let o = Command::new(&exe).args(["cli", "run", "--wait", probe, "--", "/bin/bash", "-c", &script])
                .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).output();
            let ran = o.as_ref().is_ok_and(|o| String::from_utf8_lossy(&o.stdout).lines().next() == Some("appzones-probe-ran"));
            // a refusal probe whose tool is not in the zone (126, 127) proves nothing: SKIP
            if ran && expect == 1 && o.as_ref().is_ok_and(|o| matches!(o.status.code(), Some(126 | 127))) {
                skipped.push(format!("{title} (its tool is not in the zone)"));
                continue;
            }
            let ok = ran && o.as_ref().is_ok_and(|o| o.status.success() == (expect == 0));
            fails += i32::from(!ok);
            out(&format!("{}  {title}{}", if ok { "PASS" } else { "FAIL" }, if ran { "" } else { " (the probe did not run)" }));
        }
        let host_id = std::fs::read_to_string("/etc/machine-id").map(|m| m.trim().to_string()).unwrap_or_default();
        let zone_id = Command::new(&exe).args(["cli", "run", "--wait", probe, "--", "/bin/bash", "-c", "cat /etc/machine-id"])
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).output().ok()
            .filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        let ok = match (&zone_id, z.get("machine_id").as_str() == Some("zone")) {
            (Some(id), true) => !id.is_empty() && *id != host_id,
            (Some(id), false) => *id == host_id,
            (None, _) => false,
        };
        fails += i32::from(!ok);
        out(&format!("{}  machine-id is the zone's{}", if ok { "PASS" } else { "FAIL" },
                     if zone_id.is_some() { "" } else { " (the probe did not run)" }));
        for s in skipped {
            out(&format!("SKIP  {s}"));
        }
        i32::from(fails > 0)
    }

    fn find_desktop(&self, name: &str) -> Vec<(String, String)> {
        let mut dirs = vec![std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| sh(&self.host.home.join(".local/share")))];
        let default = if plan::layout() == "nixos" { "/run/current-system/sw/share" } else { "/usr/local/share:/usr/share" };
        dirs.extend(std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| default.into()).split(':').map(String::from));
        for d in dirs {
            let p = Path::new(&d).join("applications").join(name);
            if p.is_file() {
                let text = String::from_utf8_lossy(&std::fs::read(&p).unwrap_or_default()).into_owned();
                let mut entries: Vec<(String, String)> = Vec::new();
                let mut section = String::new();
                for line in crate::policy::split_lines(&text) {
                    if line.starts_with('[') {
                        section = crate::policy::py_strip(line).to_string();
                    } else if section == "[Desktop Entry]" {
                        if let Some((k, v)) = line.split_once('=') {
                            let k = crate::policy::py_strip(k).to_string();
                            if !entries.iter().any(|(e, _)| *e == k) {
                                entries.push((k, crate::policy::py_strip(v).to_string()));
                            }
                        }
                    }
                }
                return entries;
            }
        }
        vec![]
    }

    fn launcher(&self) -> PathBuf {
        self.host.state.join("launcher/share/applications")
    }

    fn apps(&self) -> i32 {
        let cfg = self.load_cfg(true);
        let dir = self.launcher();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            warn(&format!("{}: {e}; no launcher entries", sh(&dir))); // (autostart goes on with its zones)
            return 1;
        }
        // the new entries first (each written whole, then renamed into place), the stale ones
        // removed after: a failed write never leaves launchers missing, and is said
        let mut written: Vec<String> = Vec::new();
        let mut failed: Vec<String> = Vec::new();
        let mut n = 0;
        for (aid, a) in &cfg.apps {
            let host_entry = a.get("desktop").and_then(Value::as_str).map(|d| self.find_desktop(d)).unwrap_or_default();
            let get = |k: &str| host_entry.iter().find(|(e, _)| e == k).map(|(_, v)| v.clone()).filter(|v| !v.is_empty());
            let mut cap = aid.chars();
            let fallback = cap.next().map(|c| c.to_uppercase().collect::<String>() + &cap.as_str().to_lowercase()).unwrap_or_default();
            let name = get("Name").unwrap_or(fallback);
            let cmd0 = a.get("command").and_then(Value::as_array).and_then(|c| c.first()).and_then(Value::as_str).unwrap_or("");
            let icon = get("Icon").unwrap_or_else(|| Path::new(cmd0).file_name().map(|f| f.to_string_lossy().to_lowercase())
                .unwrap_or_default());
            for zn in a.get("zones").and_then(Value::as_array).map(|z| z.iter().filter_map(Value::as_str).collect::<Vec<_>>())
                .unwrap_or_default() {
                let z = cfg.zone(zn).expect("validated");
                let mut lines = vec!["[Desktop Entry]".to_string(), "Type=Application".into(), format!("Name={name} ({zn})"),
                    format!("GenericName={} in zone {zn}", host_entry.iter().find(|(e, _)| e == "GenericName").map(|(_, v)| v.clone())
                        .unwrap_or_else(|| name.clone())),
                    format!("Comment=Runs in app zone {zn} ({}, colour {})", z.get("network").as_str().unwrap_or(""),
                            z.get("colour").as_str().unwrap_or("")),
                    format!("Exec=appzones run {zn} {aid} %U"), format!("Icon={icon}"), "Terminal=false".into(),
                    format!("Keywords=zone;{zn};{aid};"), format!("X-Zone={zn}")];
                for k in ["Categories", "MimeType", "StartupNotify"] {
                    if let Some(v) = get(k) {
                        lines.push(format!("{k}={v}"));
                    }
                }
                let file = format!("appzones-{aid}-{zn}.desktop");
                let tmp = dir.join(format!(".{file}.new"));
                match std::fs::write(&tmp, lines.join("\n") + "\n").and_then(|_| std::fs::rename(&tmp, dir.join(&file))) {
                    Ok(()) => n += 1,
                    Err(e) => {
                        let _ = std::fs::remove_file(&tmp);
                        failed.push(format!("{file}: {e}"));
                    }
                }
                written.push(file);
            }
        }
        if let Ok(it) = std::fs::read_dir(&dir) {
            for e in it.filter_map(|e| e.ok()) {
                let f = e.file_name().to_string_lossy().into_owned();
                if (f.starts_with("appzones-") || f.starts_with("zone-")) && f.ends_with(".desktop") && !written.contains(&f) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        out(&format!("{n} launcher entries in {}", sh(&dir)));
        for f in &failed {
            warn(&format!("not written: {f}"));
        }
        i32::from(!failed.is_empty())
    }

    fn autostart(&self) -> i32 {
        let cfg = self.load_cfg(true);
        let _ = plan::write_colours(&self.host, &cfg);
        self.apps();
        let mut starting = Vec::new();
        for z in &cfg.zones {
            if config::truthy(z.get("autostart")) {
                out(&format!("autostart: zone {}", z.name));
                if self.use_systemd(Some(z)) {
                    self.systemctl(&["start", "--no-block", &format!("appzones@{}.service", z.name)], false);
                } else if let Some(s) = self.claim(&z.name, true, false) {
                    starting.push((z.name.clone(), s));
                }
            }
        }
        for (n, (r, since)) in starting {
            // (in a child: a zone that fails says why, the others still wait)
            // Safety: fork in a process with no other threads; the child dies with us.
            let pid = unsafe { libc::fork() };
            if pid == 0 {
                // Safety: prctl in the child.
                unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) };
                self.wait_ready(&n, r, since, true);
                // Safety: the child's end.
                unsafe { libc::_exit(0) };
            }
            if pid < 0 {
                self.wait_ready(&n, r, since, true); // (no child: here, then)
            } else {
                wait_child(pid);
            }
        }
        0
    }

    fn logs(&self, name: &str, follow: bool) -> i32 {
        let cfg = self.load_zone(name, true);
        let z = cfg.zone(name).expect("checked");
        let err = if !self.unit_runs(name) && (!self.server_pids(name).is_empty() || !self.use_systemd(Some(z))) {
            let mut c = Command::new("tail");
            c.args(["-n", "200"]);
            if follow {
                c.arg("-F");
            }
            c.arg(self.log_path(name)).exec()
        } else {
            let mut c = Command::new("journalctl");
            c.args(["--user", "-u", &format!("appzones@{name}.service"), "-n", "200", "--no-pager"]);
            if follow {
                c.arg("-f");
            }
            c.exec()
        };
        die(&err.to_string())
    }

    // ── mounts ───────────────────────────────────────────────────────────

    fn mounts_file(&self, name: &str) -> PathBuf {
        self.rt().join(name).join("mounts.json")
    }

    fn read_mounts(&self, name: &str) -> serde_json::Map<String, J> {
        std::fs::read(self.mounts_file(name)).ok().and_then(|b| serde_json::from_slice::<J>(&b).ok())
            .and_then(|v| v.as_object().cloned()).unwrap_or_default()
    }

    fn write_mounts(&self, name: &str, m: &serde_json::Map<String, J>) {
        let tmp = self.mounts_file(name).with_extension("tmp");
        let text = serde_json::to_string_pretty(&J::Object(m.clone())).unwrap_or_default();
        if std::fs::write(&tmp, format!("{text}\n")).is_ok() {
            let _ = std::fs::rename(&tmp, self.mounts_file(name));
        }
    }

    fn mount_helper(&self, args: &[&str]) -> (i32, String, String) {
        let sudo = config::env("SUDO").filter(|v| !v.is_empty()).unwrap_or_else(|| {
            if Path::new("/run/wrappers/bin/sudo").exists() { "/run/wrappers/bin/sudo".into() }
            else { plan::which("sudo", None).unwrap_or_else(|| "/usr/bin/sudo".into()) }
        });
        let helper = config::env("MOUNT_HELPER").filter(|v| !v.is_empty())
            .unwrap_or_else(|| sh(&self.host.here.join("appzones-mount-helper")));
        match Command::new(sudo).arg("-n").arg(helper).args(args).stdin(Stdio::null()).output() {
            Ok(o) => (o.status.code().unwrap_or(1), String::from_utf8_lossy(&o.stdout).into_owned(),
                      String::from_utf8_lossy(&o.stderr).into_owned()),
            Err(e) => (1, String::new(), e.to_string()),
        }
    }

    fn helper_failed(&self, r: (i32, String, String), what: &str) -> ! {
        let err = if r.2.trim().is_empty() { r.1.trim().to_string() } else { r.2.trim().to_string() };
        if err.contains("password is required") || err.contains("not allowed") || err.contains("may not run sudo") {
            die(&format!("{what}: sudo refused the mount helper ({}). It needs the NixOS module's sudo rule \
                          (programs.appzones.mountHelper = true, for programs.appzones.user) and a rebuilt system",
                         err.lines().last().unwrap_or("")));
        }
        let e = err.strip_prefix("appzones-mount-helper: ").unwrap_or(&err).to_string();
        die(&format!("{what}: {}", if e.is_empty() { r.0.to_string() } else { e }))
    }

    fn host_mounts(&self, name: &str) -> Vec<String> {
        let pre = format!("{}/", sh(&self.host.state.join(name).join("mnt")));
        let mut v: Vec<String> = Vec::new();
        for line in std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default().lines() {
            let Some(mp) = line.split_whitespace().nth(4) else { continue };
            let mp = crate::plan::mountinfo_path(mp);
            if let Some(rest) = mp.strip_prefix(&pre) {
                if !rest.contains('/') && !v.iter().any(|x| x == rest) {
                    v.push(rest.to_string());
                }
            }
        }
        v
    }

    fn propagation_shared(path: &Path) -> Option<bool> {
        Self::propagation_shared_in(path, Path::new("/proc/self/mountinfo"))
    }

    /// Whether the mount holding PATH (by the mount table INFO) is shared: new mounts reach a running zone.
    fn propagation_shared_in(path: &Path, info: &Path) -> Option<bool> {
        let text = std::fs::read_to_string(info).ok()?;
        let p = sh(path);
        let (mut best, mut shared) = (String::new(), None);
        for line in text.lines() {
            let f: Vec<&str> = line.split(' ').collect();
            if f.len() < 7 {
                continue;
            }
            let point = crate::plan::mountinfo_path(f[4]);
            let inside = p == point || p.starts_with(&format!("{}/", point.trim_end_matches('/')));
            if inside && point.len() >= best.len() {
                let opts: &[&str] = match f.iter().position(|x| *x == "-") { Some(i) => &f[6..i.max(6)], None => &[] };
                shared = Some(opts.iter().any(|o| o.starts_with("shared:")));
                best = point;
            }
        }
        shared
    }

    /// Zone NAME's mounts lock, taken: `mount` and `umount` hold it from their check to
    /// their record, the zone's teardown (`_cleanup`) before it detaches them all, so a
    /// mount is never made after the teardown has run (and mounts.json never loses one).
    fn mounts_lock(&self, name: &str) -> Option<std::fs::File> {
        self.mkdir_rt();
        let f = open_lock(&self.rt().join(format!("{}.mounts.lock", zone_name_ok(name).then_some(name)?))).ok()?;
        // Safety: flock on our own descriptor; the others hold it only briefly.
        (unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } == 0).then_some(f)
    }

    fn mount(&self, name: &str, path: &str, ro: bool, as_name: Option<String>) -> i32 {
        self.load_zone(name, true);
        // the root helper reads no environment: it mounts under ~/.local/share/appzones (or
        // the pre-0.6 directory); a zone whose state is elsewhere would not see the mount
        let default = [self.host.home.join(".local/share/appzones"), self.host.home.join(".local/share/zones")];
        if !default.contains(&self.host.state) {
            die(&format!("appzones mount works with the default state directory only ({} is set by $APPZONES_STATE)",
                         sh(&self.host.state)));
        }
        let _held = self.mounts_lock(name).unwrap_or_else(|| die(&format!("zone {name}: cannot take its mounts lock")));
        if !self.answers(name) {
            die(&format!("zone {name} is not running; a mount lives only while the zone runs (`appzones up {name}` first)"));
        }
        let policy = config::policy(self.host.here.as_os_str().to_owned());
        if let Some(err) = policy.mount_source_error(path.as_bytes()) {
            die(&format!("refused: {err}"));
        }
        let src = plan::rp(&String::from_utf8_lossy(&policy.expand(path.as_bytes())));
        let nm = as_name.unwrap_or_else(|| Path::new(&src).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default());
        if !crate::policy::mount_name_ok(&nm) {
            die(&format!("{} cannot be a mount name (letters, digits, . _ -, not first; at most 64): give one with --as NAME",
                         py_repr(&nm)));
        }
        if self.host_mounts(name).contains(&nm) {
            die(&format!("/mnt/host/{nm} is already mounted in zone {name} (appzones mounts {name})"));
        }
        let target = self.host.state.join(name).join("mnt").join(&nm);
        let _ = std::fs::create_dir(&target);
        let mut args = vec!["mount", name, &nm, &src];
        if ro {
            args.push("--ro");
        }
        let r = self.mount_helper(&args);
        if r.0 != 0 {
            let _ = std::fs::remove_dir(&target);
            self.helper_failed(r, "mount");
        }
        let mut m = self.read_mounts(name);
        m.insert(nm.clone(), json!({"source": src, "mode": if ro { "ro" } else { "rw" }}));
        self.write_mounts(name, &m);
        let inside = agent::zone_request(self.rt(), name, &json!({"op": "mounts"}));
        if inside.as_object().is_some_and(|o| !o.is_empty()) {
            let seen = inside.get("mounts").and_then(J::as_array).is_some_and(|ms| ms.iter()
                .any(|p| p.get(0).and_then(J::as_str) == Some(&format!("/mnt/host/{nm}"))));
            if !seen {
                if Self::propagation_shared(target.parent().unwrap_or(&target)) == Some(false) {
                    warn(&format!("mounted on the host, but zone {name} does not see /mnt/host/{nm}: the host's mounts do not \
                                   pass new mounts on (systemd makes them shared at boot; without it they are private). Run \
                                   `sudo mount --make-rshared /` (at every boot, before zones start), then `appzones restart \
                                   {name}` and mount again"));
                } else {
                    warn(&format!("mounted on the host, but zone {name} does not see /mnt/host/{nm}: `appzones restart {name}` \
                                   and mount again"));
                }
            }
        }
        out(&format!("{src} is /mnt/host/{nm} in zone {name} ({}), until `appzones umount {name} {nm}` or the zone stops",
                     if ro { "read-only" } else { "read-write" }));
        0
    }

    fn umount(&self, name: &str, nm: &str, lazy: bool) -> i32 {
        self.load_zone(name, true);
        let _held = self.mounts_lock(name).unwrap_or_else(|| die(&format!("zone {name}: cannot take its mounts lock")));
        if !self.host_mounts(name).iter().any(|m| m == nm) {
            die(&format!("nothing is mounted at /mnt/host/{nm} in zone {name} (appzones mounts {name})"));
        }
        let mut args = vec!["umount", name, nm];
        if lazy {
            args.push("--lazy");
        }
        let r = self.mount_helper(&args);
        if r.0 == 3 {
            let busy = agent::zone_request(self.rt(), name, &json!({"op": "busy", "path": format!("/mnt/host/{nm}")}));
            let procs = busy.get("procs").and_then(J::as_array).cloned().unwrap_or_default();
            warn(&format!("/mnt/host/{nm} is busy in zone {name}{}",
                          if procs.is_empty() { " (zone-init could not say by what)" } else { ":" }));
            for p in procs {
                let pid = p.get(0).map(|x| x.to_string()).unwrap_or_default();
                let cmd: String = p.get(1).and_then(J::as_str).unwrap_or("").chars().take(60).collect();
                let what = p.get(2).and_then(J::as_str).unwrap_or("");
                eprintln!("  pid {pid} {cmd}: {what}");
            }
            die(&format!("close those (or stop the app), or detach it now: appzones umount --lazy {name} {nm}"));
        }
        if r.0 != 0 {
            self.helper_failed(r, "umount");
        }
        let _ = std::fs::remove_dir(self.host.state.join(name).join("mnt").join(nm));
        let mut m = self.read_mounts(name);
        m.remove(nm);
        if self.rt().join(name).is_dir() {
            self.write_mounts(name, &m);
        }
        out(&format!("/mnt/host/{nm} removed from zone {name}{}",
                     if lazy { " (lazily: open files keep it until they close)" } else { "" }));
        0
    }

    fn mounts(&self, name: &str) -> i32 {
        self.load_zone(name, true);
        let names = self.host_mounts(name);
        if !self.answers(name) {
            if !names.is_empty() {
                out(&format!("zone {name} is not running; left mounted on the host (cleared at its next start): {}",
                             names.join(", ")));
            }
            return 0;
        }
        let m = self.read_mounts(name);
        let inside = agent::zone_request(self.rt(), name, &json!({"op": "mounts"}));
        let modes: Vec<(String, String)> = inside.get("mounts").and_then(J::as_array).map(|ms| ms.iter().filter_map(|p| {
            let path = p.get(0)?.as_str()?;
            Some((path.strip_prefix("/mnt/host/").unwrap_or(path).to_string(), p.get(1)?.as_str()?.to_string()))
        }).collect()).unwrap_or_default();
        let mut rows = vec![["NAME".to_string(), "MODE".into(), "SOURCE".into()]];
        for nm in &names {
            let rec = m.get(nm);
            let mode = modes.iter().find(|(n, _)| n == nm).map(|(_, md)| md.clone())
                .or_else(|| rec.and_then(|r| r.get("mode")).and_then(J::as_str).map(String::from)).unwrap_or_else(|| "?".into());
            let source = rec.and_then(|r| r.get("source")).and_then(J::as_str).unwrap_or("?").to_string();
            rows.push([nm.clone(), mode, source]);
        }
        if rows.len() == 1 {
            out(&format!("nothing mounted in zone {name} (appzones mount {name} DIR)"));
            return 0;
        }
        let w: Vec<usize> = (0..3).map(|i| rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0)).collect();
        for r in rows {
            let line: Vec<String> = r.iter().enumerate().map(|(i, c)| format!("{c:<width$}", width = w[i])).collect();
            out(line.join("  ").trim_end());
        }
        0
    }

    // ── serve (the server's own first part) ──────────────────────────────

    /// server.take_lock: the zone's lock for as long as this process lives.
    fn take_lock(&self, name: &str, lock_fd: Option<RawFd>) -> RawFd {
        let path = self.lock_path(name);
        self.mkdir_rt();
        let fd = match lock_fd {
            None => {
                let f = open_lock(&path).unwrap_or_else(|e| die(&format!("{}: {e}", sh(&path))));
                std::os::fd::IntoRawFd::into_raw_fd(f)
            }
            Some(fd) => {
                // Safety: fstat into a local struct.
                let mut st: libc::stat = unsafe { std::mem::zeroed() };
                let same = unsafe { libc::fstat(fd, &mut st) } == 0 && std::fs::symlink_metadata(&path)
                    .is_ok_and(|m| m.dev() == st.st_dev as u64 && m.ino() == st.st_ino as u64);
                if !same {
                    die(&format!("--lock-fd {fd}: not zone {name}'s lock ({})", sh(&path)));
                }
                fd
            }
        };
        if !flock_nb(fd) {
            let pids = self.server_pids(name);
            die_code(&format!("zone {name} is already served{}", if pids.is_empty() { String::new() } else {
                format!(" (pid {})", pids.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ")) }), ALREADY_SERVED);
        }
        if self.answers(name) {
            die_code(&format!("zone {name} already runs under a server without a lock (from before this version); stop it \
                               first (appzones stop {name})"), ALREADY_SERVED);
        }
        fd
    }

    fn serve(&self, rest: &[String]) -> i32 {
        let Some(name) = rest.first() else { die("usage: appzones serve ZONE") };
        let (mut notify, mut lock): (Option<RawFd>, Option<RawFd>) = (None, None);
        let mut opts = rest[1..].iter();
        while let Some(o) = opts.next() {
            let (key, v) = match o.split_once('=') {
                Some((k, v)) => (k.to_string(), Some(v.to_string())),
                None => (o.clone(), opts.next().cloned()),
            };
            let ok = v.as_ref().is_some_and(|v| v.bytes().all(|b| b.is_ascii_digit()) && !v.is_empty() && v.len() <= 9);
            match (key.as_str(), ok) {
                ("--notify-fd", true) => notify = v.and_then(|v| v.parse().ok()),
                ("--lock-fd", true) => lock = v.and_then(|v| v.parse().ok()),
                _ => die(&format!("usage: appzones serve ZONE [--notify-fd N] [--lock-fd N] (not {})", py_repr(o))),
            }
        }
        if notify.is_some() && notify == lock {
            die("--notify-fd and --lock-fd must be different fds (readiness closes the notify fd)");
        }
        if let Some(fd) = notify {
            if fd < 3 || !notify_fd_ok(fd) {
                die(&format!("--notify-fd {fd}: not a pipe or file above 2, open for writing"));
            }
        }
        let lock = self.take_lock(name, lock);
        // held as long as this server runs (shared: other servers too), taken before the
        // server chooses its state directory: the pre-0.6 move (migrate_state) waits for it
        // (never without it: a server that could not take it does not start)
        let state = self.state_lock().unwrap_or_else(|| die(&format!("{}: cannot open it", sh(&self.rt().join(".state.lock")))));
        loop {
            // Safety: flock on our own descriptor; blocking, as the move holds it only briefly.
            if unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&state), libc::LOCK_SH) } == 0 {
                break;
            }
            let e = std::io::Error::last_os_error();
            if e.kind() != std::io::ErrorKind::Interrupted {
                die(&format!("{}: {e}", sh(&self.rt().join(".state.lock"))));
            }
        }
        let rc = crate::serve::serve_zone(name, lock, notify) as i32;
        drop(state); // (held to here: the server's whole life)
        rc
    }

    // ── upgrading from before 0.6 ────────────────────────────────────────

    fn migrate_state(&mut self) {
        if self.legacy_config {
            warn(&format!("reading {}: since 0.6 it belongs in ~/.config/appzones/zones.toml (mkdir -p ~/.config/appzones \
                           && mv ~/.config/zones/zones.toml ~/.config/appzones/)", self.config_path()));
        }
        let mut running: Vec<String> = my_pids().into_iter().filter_map(|(p, _)| proc_argv(&p))
            .filter(|(a, c)| server_command(a, c, &self.host.here).is_some()).map(|(a, _)| a.join(" ")).collect();
        if running.iter().any(|r| r.contains("/libexec/zones/")) {
            warn("zones started by appzones before 0.6 are running and this version does not see them: `appzones stop \
                  --all`, then start them again");
        }
        if config::env("STATE").is_some_and(|s| !s.is_empty()) {
            return;
        }
        let new = self.host.home.join(".local/share/appzones");
        let legacy = self.host.home.join(".local/share/zones");
        if self.host.state != legacy {
            if legacy.symlink_metadata().is_ok() && new.symlink_metadata().is_ok() {
                warn(&format!("{} is no longer used (since 0.6 {} is): move what you still need from it, then remove it",
                              sh(&legacy), sh(&new)));
            }
            return;
        }
        // no server may start until the move is done (they take it shared first), and none
        // of this version runs when it can be had
        let Some(state_lock) = self.state_lock() else {
            warn(&format!("{}: cannot open it; {} stays where it is", sh(&self.rt().join(".state.lock")), sh(&legacy)));
            return;
        };
        let fd = std::os::fd::AsRawFd::as_raw_fd(&state_lock);
        let share = |fd: RawFd| loop {
            // Safety: flock on our own descriptor (blocking: a move holds it only briefly).
            if unsafe { libc::flock(fd, libc::LOCK_SH) } == 0
                || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                break;
            }
        };
        if !flock_nb(fd) {
            // held: by servers (shared), or by another command moving it now (briefly): once
            // it can be had shared, where the state is is looked at again
            share(fd);
            self.state_hold = Some(state_lock);
            if legacy.symlink_metadata().is_err() && new.is_dir() {
                self.host.state = new; // moved meanwhile
            } else {
                warn(&format!("{} moves to {} once no zone runs (appzones stop --all)", sh(&legacy), sh(&new)));
            }
            return;
        }
        // and every server since the lock (0.7), whatever it is written in: a zone lock held
        running.extend(self.locked_zones());
        let mounted = std::fs::read_to_string("/proc/self/mountinfo").map(|t| t.lines().filter_map(|l| l.split_whitespace().nth(4))
            .map(crate::plan::mountinfo_path).any(|m| m == sh(&legacy) || m.starts_with(&format!("{}/", sh(&legacy))))).unwrap_or(true);
        if !running.is_empty() || mounted {
            warn(&format!("{} moves to {} once no zone runs (appzones stop --all)", sh(&legacy), sh(&new)));
            share(fd); // (still used here: held shared while this command runs)
            self.state_hold = Some(state_lock);
            return;
        }
        match std::fs::rename(&legacy, &new) {
            Err(e) if new.symlink_metadata().is_err() => {
                let t = e.to_string();
                warn(&format!("could not move {} to {}: {}; using it where it is", sh(&legacy), sh(&new),
                              t.split(" (os error").next().unwrap_or(&t)));
                return;
            }
            Err(_) => {}
            Ok(()) => warn(&format!("moved {} to {} (appzones 0.6)", sh(&legacy), sh(&new))),
        }
        self.host.state = new;
    }
}

fn random_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    let _ = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b));
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// shlex.quote.
fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c)) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

/// A pipe, a regular file or /dev/null (client.stdio_fd_ok).
fn stdio_fd_ok(fd: RawFd) -> bool {
    // Safety: fstat into a local struct.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut st) } != 0 {
        return false;
    }
    let t = st.st_mode & libc::S_IFMT;
    t == libc::S_IFIFO || t == libc::S_IFREG
        || (t == libc::S_IFCHR && libc::major(st.st_rdev) == 1 && libc::minor(st.st_rdev) == 3)
}

/// server.notify_fd_ok: open for writing, and a pipe, FIFO, file or character device.
fn notify_fd_ok(fd: RawFd) -> bool {
    // Safety: fcntl and fstat on a descriptor number we were given.
    let (flags, mode) = unsafe {
        let mut st: libc::stat = std::mem::zeroed();
        if libc::fstat(fd, &mut st) != 0 {
            return false;
        }
        (libc::fcntl(fd, libc::F_GETFL), st.st_mode & libc::S_IFMT)
    };
    if flags < 0 || !matches!(flags & libc::O_ACCMODE, libc::O_WRONLY | libc::O_RDWR) {
        return false;
    }
    mode == libc::S_IFIFO || mode == libc::S_IFREG || mode == libc::S_IFCHR
}

/// `appzones` itself: argv without the program's name.
pub fn main(args: &[OsString]) -> i32 {
    let argv: Vec<String> = args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
    if argv.is_empty() || matches!(argv[0].as_str(), "-h" | "--help" | "help") {
        println!("{HELP}");
        return if argv.is_empty() { 2 } else { 0 };
    }
    if argv == ["--version"] || argv == ["-V"] {
        out(&format!("appzones {} (config release {})", env!("CARGO_PKG_VERSION"), config::RELEASE));
        return 0;
    }
    let mut cli = Cli::new(Host::new(crate::plan::exe_dir()));
    let (cmd, rest) = (argv[0].as_str(), &argv[1..]);
    if cmd == "_autostart" || (COMMANDS.contains(&cmd) && cmd != "active" && cmd != "serve") {
        cli.migrate_state();
    }
    let one = || -> String {
        rest.first().cloned().unwrap_or_else(|| die(&format!("usage: appzones {cmd} ZONE")))
    };
    match cmd {
        "serve" | "_supervise" => cli.serve(rest),
        "_netsetup" => {
            let i = rest.iter().position(|a| a == "--").unwrap_or_else(|| die("usage: appzones _netsetup RULES INFO ZONE -- CMD"));
            if i < 3 {
                die("usage: appzones _netsetup RULES INFO ZONE -- CMD");
            }
            crate::netsetup::main(&cli.host, &rest[0], &rest[1], &rest[2], rest[i + 1..].to_vec())
        }
        "_autostart" => cli.autostart(),
        "_sway-marks" => crate::active::sway_hook(),
        "_link" => {
            if rest.len() != 2 {
                die("usage: appzones _link ZONE URL");
            }
            cli.open_link(&rest[0], &rest[1])
        }
        "_cleanup" => {
            let n = one();
            let zdir = cli.host.state.join(&n);
            let _held = cli.mounts_lock(&n); // (without it, too: detaching is never the wrong way)
            match plan::prepare_mnt(&cli.host, &n, &zdir) {
                Ok(true) => 0,
                Ok(false) => 1,
                Err(e) => die(&e),
            }
        }
        "list" => cli.zone_list(rest.iter().any(|a| a == "--json")),
        "which" => cli.which(&one()),
        "up" | "stop" | "restart" => cli.each_zone(cmd, rest),
        "ping" => {
            let r = agent::ping(cli.rt(), &one());
            out(&py_json(r.as_ref().unwrap_or(&J::Null)));
            i32::from(!r.is_some_and(|v| v.as_object().is_none_or(|o| !o.is_empty())))
        }
        "ps" => {
            let n = one();
            match agent::zone_procs(cli.rt(), &n) {
                Ok(procs) => {
                    for p in procs {
                        let pid = p.get(0).map(|x| x.to_string()).unwrap_or_default();
                        let c = p.get(1).map(|x| x.as_str().map(String::from).unwrap_or_else(|| x.to_string())).unwrap_or_default();
                        out(&format!("{pid} {c}"));
                    }
                    0
                }
                Err(_) => die(&format!("zone {n} is not running (start it: appzones up {n})")),
            }
        }
        "logs" => {
            let follow = rest.iter().any(|a| a == "-f" || a == "--follow");
            let rest2: Vec<&String> = rest.iter().filter(|a| *a != "-f" && *a != "--follow").collect();
            let Some(n) = rest2.first() else { die("usage: appzones logs ZONE") };
            cli.logs(n, follow)
        }
        "shell" => cli.shell(&one()),
        "run" => {
            let (mut wait, mut restart, mut bus): (bool, bool, Option<String>) = (false, false, None);
            let mut take = |ws: &mut Vec<String>| {
                while let Some(f) = ws.first().cloned() {
                    if f == "--wait" {
                        wait = true;
                    } else if f == "--restart" {
                        restart = true;
                    } else if f == "--bus" || f.starts_with("--bus=") {
                        ws.remove(0);
                        let v = if let Some(v) = f.strip_prefix("--bus=") { v.to_string() }
                                else if ws.is_empty() { String::new() } else { ws.remove(0) };
                        if !config::BUSES.contains(&v.as_str()) {
                            die(&format!("--bus must be one of {}", config::BUSES.join(", ")));
                        }
                        bus = Some(v);
                        continue;
                    } else {
                        break;
                    }
                    ws.remove(0);
                }
            };
            let mut r = rest.to_vec();
            take(&mut r);
            if r.is_empty() {
                die("usage: appzones run [--restart] [--bus host|zone|none] [--wait] ZONE APP|-- CMD...");
            }
            let name = r.remove(0);
            take(&mut r);
            if wait && r.first().map(String::as_str) != Some("--") {
                die("--wait runs a command: appzones run --wait ZONE -- CMD...");
            }
            cli.run(&name, &r, wait, restart, bus)
        }
        "new" => cli.new_zone(&one(), rest[1..].iter().any(|a| a == "-y")),
        "check" => {
            let mut r: Vec<&String> = rest.iter().collect();
            if r.first().map(|a| a.as_str()) == Some("--config-only") {
                r.remove(0);
            }
            let probe = if r.first().map(|a| a.as_str()) == Some("--probe") { r.get(1).map(|a| a.as_str()) } else { None };
            cli.check(probe)
        }
        "apps" => cli.apps(),
        "active" => crate::active::main(rest.iter().any(|a| a == "--follow"), rest.iter().any(|a| a == "--json"), &cli.host.host_rt),
        "mount" | "umount" | "mounts" => {
            let (mut ro, mut lazy, mut as_name): (bool, bool, Option<String>) = (false, false, None);
            let mut words = Vec::new();
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                if a == "--as" || a.starts_with("--as=") {
                    as_name = if let Some(v) = a.strip_prefix("--as=") { Some(v.to_string()) } else { it.next().cloned() };
                    if as_name.as_deref().is_none_or(str::is_empty) {
                        die("--as needs a NAME");
                    }
                } else if a == "--ro" {
                    ro = true;
                } else if a == "--lazy" {
                    lazy = true;
                } else if a.starts_with("--") {
                    die(&format!("zone {cmd}: unknown option {a}"));
                } else {
                    words.push(a.clone());
                }
            }
            match (cmd, words.len()) {
                ("mount", 2) if !lazy => cli.mount(&words[0], &words[1], ro, as_name),
                ("umount", 2) if !ro && as_name.is_none() => cli.umount(&words[0], &words[1], lazy),
                ("mounts", 1) => cli.mounts(&words[0]),
                _ => die("usage: appzones mount ZONE HOST-DIR [--ro] [--as NAME] | appzones umount ZONE NAME [--lazy] | \
                          appzones mounts ZONE"),
            }
        }
        _ => {
            let near = close_match(cmd, COMMANDS.iter().copied(), 0.75);
            die_code(&format!("unknown command {}{}; see appzones --help", py_repr(cmd),
                              near.map(|n| format!(" (did you mean {}?)", py_repr(n))).unwrap_or_default()), 2)
        }
    }
}

impl Cli {
    /// `appzones _link TARGET URL`: URL in zone TARGET's browser, started in the background.
    fn open_link(&self, target: &str, url: &str) -> i32 {
        if !crate::serve::link_ok(url) || !zone_name_ok(target) {
            return 1;
        }
        let cfg = self.load();
        let app = if cfg.errors.is_empty() {
            cfg.apps.iter().find(|(_, a)| {
                let cmd = a.get("command").and_then(Value::as_array).and_then(|c| c.first()).and_then(Value::as_str).unwrap_or("");
                let base = Path::new(cmd).file_name().map(|f| f.to_string_lossy().to_lowercase()).unwrap_or_default();
                a.get("zones").and_then(Value::as_array).is_some_and(|z| z.iter().any(|x| x.as_str() == Some(target)))
                    && ["firefox", "chromium", "chrome", "librewolf", "brave"].iter().any(|b| base.contains(b))
            }).map(|(id, _)| id.clone())
        } else {
            None
        };
        let Some(app) = app else {
            out(&format!("[zone] links: no browser declared for zone {target}"));
            return 1;
        };
        // Safety: fork in a process with no other threads; the child runs the browser and exits.
        if unsafe { libc::fork() } == 0 {
            // Safety: a session of its own, stdin from /dev/null.
            unsafe {
                libc::setsid();
                let n = libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY);
                libc::dup2(n, 0);
            }
            let code = self.run(target, &[app, url.to_string()], false, false, None);
            std::process::exit(code);
        }
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    fn tmp(tag: &str) -> PathBuf {
        let t = std::env::temp_dir().join(format!("az-cli-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        std::fs::canonicalize(&t).unwrap()
    }

    #[test]
    fn servers_from_before_the_lock() {
        // only the process's own script counts; an appzones installed in the store counts too
        let here = tmp("server-command");
        std::fs::write(here.join("zone.py"), "").unwrap();
        let me = sh(&here.join("zone.py"));
        let a = |x: &[&str]| x.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let pair = |a: &str, b: &str| Some((a.to_string(), b.to_string()));
        for (argv, want) in [
            (a(&["python3", "-B", &me, "serve", "aa"]), pair("serve", "aa")),
            (a(&["python3", &me, "_supervise", "bb", "--x"]), pair("_supervise", "bb")),
            (a(&["python3", "-c", "x", &me, "serve", "aa"]), None), // a script after -c CODE is not ours
            (a(&["python3", "/home/u/tools/zone.py", "serve", "aa"]), None), // another zone.py
            (a(&["python3", &me, "run", "--wait", "td", "--", "python3", &me, "serve", "td"]), None),
            (a(&["python3", "/nix/store/abc-appzones-0.6.0/libexec/appzones/zone.py", "_supervise", "cc"]),
             pair("_supervise", "cc")),
            (a(&["python3", "/nix/store/abc-x/libexec/other/zone.py", "serve", "cc"]), None),
            (a(&["python3", &me, "serve"]), None),
            (a(&["python3", "-X", "dev", &me, "serve", "aa"]), pair("serve", "aa")), // an option with a value
            (a(&["python3", "-m", "x", &me, "serve", "aa"]), None),
            // a removed zone.py of an installed appzones (an upgrade while it ran)
            (a(&["python3", "/usr/lib/appzones/zone.py", "serve", "dd"]), pair("serve", "dd")),
            (a(&["python3", "/usr/local/lib/zones/zone.py", "_supervise", "ee"]), pair("_supervise", "ee")),
        ] {
            assert_eq!(server_command(&argv, "/", &here), want, "{argv:?}");
        }
        // a relative path, from the process's cwd
        assert_eq!(server_command(&a(&["python3", "zone.py", "serve", "aa"]), &sh(&here), &here), pair("serve", "aa"));
        let _ = std::fs::remove_dir_all(&here);
    }

    #[test]
    fn a_server_holds_the_lock() {
        // opened is not held: only the process with the flock counts as the zone's server
        let t = tmp("holds");
        let path = t.join("z.lock");
        let f = std::fs::File::create(&path).unwrap();
        let m = std::fs::metadata(&path).unwrap();
        let me = std::process::id().to_string();
        assert!(Cli::holds(&me, (m.dev(), m.ino())));
        assert!(!Cli::holds_lock(&me, (m.dev(), m.ino())), "open, not locked");
        assert!(flock_nb(f.as_raw_fd()));
        assert!(Cli::holds_lock(&me, (m.dev(), m.ino())), "locked");
        drop(f);
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn mount_propagation() {
        let t = tmp("mountinfo");
        let info = t.join("mountinfo");
        std::fs::write(&info, "22 1 0:21 / / rw,relatime shared:1 - ext4 /dev/vda rw\n\
            30 22 0:25 / /home rw,relatime - ext4 /dev/vdb rw\n\
            31 30 0:26 / /home/alice/my\\040dir rw shared:9 - ext4 /dev/vdc rw\n\
            32 30 0:27 / /home/alice/.local/share/appzones/z/mnt/d rw shared:12 - ext4 /dev/vdd rw\n").unwrap();
        let p = |x: &str| Cli::propagation_shared_in(Path::new(x), &info);
        assert_eq!(p("/usr/lib"), Some(true)); // under /, shared
        assert_eq!(p("/home/alice/.local/share/appzones/z/mnt/d"), Some(true));
        assert_eq!(p("/home/alice/.local/share/appzones/z/mnt"), Some(false)); // the private /home
        assert_eq!(p("/home/alice/my dir/x"), Some(true));
        assert_eq!(p("/homeX/a"), Some(true)); // /homeX is not under /home
        assert_eq!(Cli::propagation_shared_in(Path::new("/x"), &t.join("missing")), None);
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn stdio_fds() {
        let t = tmp("stdio");
        let mut fds = [0; 2];
        // Safety: pipe into a local array.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let f = std::fs::File::create(t.join("f")).unwrap();
        let d = std::fs::File::open(&t).unwrap();
        let null = std::fs::File::open("/dev/null").unwrap();
        let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
        for fd in [fds[0], fds[1], f.as_raw_fd(), null.as_raw_fd()] {
            assert!(stdio_fd_ok(fd), "{fd}");
        }
        assert!(!stdio_fd_ok(d.as_raw_fd()), "a directory");
        assert!(!stdio_fd_ok(a.as_raw_fd()), "a socket");
        // --notify-fd: open for writing; the read end is refused, and no such fd
        assert!(notify_fd_ok(fds[1]));
        assert!(!notify_fd_ok(fds[0]));
        assert!(!notify_fd_ok(i32::MAX));
        // Safety: closing our own pipe.
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
        let _ = std::fs::remove_dir_all(&t);
    }
}
