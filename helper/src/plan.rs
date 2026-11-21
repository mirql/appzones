//! A zone's plan, made in Rust: the port of make_plan in src/appzones/server.py, with
//! fs.bwrap_argv and what it uses (the theme copy, devices, /tmp, runtime mount points),
//! net (the nft deny list, resolv.conf, pasta, the resolver) and portals (flatpak-info,
//! the D-Bus proxy's rules, the xdg-open shim). The same files, messages and plan as
//! Python's (DifferentialPlanTest). Until stage 3 the zone's command line still starts
//! with Python's `zone.py _netsetup`, and links and the end go through `zone.py`.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value as J};
use toml::Value;

use crate::config::{self, Config, Zone};
use crate::policy::{py_repr, realpath, Policy};
use crate::pyfmt::{fnmatchcase, repr_strs};

pub const PIN: &str = "--zones-pin";
const PLAN_IF: &str = crate::serve::PLAN_IF;
const PLAN_END: &str = crate::serve::PLAN_END;
const PLAN_HOST_WAYLAND: &str = crate::serve::PLAN_HOST_WAYLAND;
const DNS_ADDR: &str = "127.0.0.53";
const ZONE_HELPER: &str = "/run/appzones-helper";
pub const RUST_HELPERS: &[&str] = &["zone-wayland", "zone-init", "open-link", "serve", "plan"];
const SBIN_DIRS: &[&str] = &["/usr/sbin", "/sbin"];
const FHS_ETC: &[&str] = &["alternatives", "ld.so.cache", "ld.so.conf", "ld.so.conf.d", "services", "protocols",
    "gai.conf", "ca-certificates", "bash.bashrc", "debian_version", "lsb-release", "environment"];
const ETC: &[&str] = &["static", "fonts", "ssl", "pki", "passwd", "group", "nsswitch.conf", "localtime", "zoneinfo",
    "mime.types", "xdg", "profile", "hosts", "host.conf", "login.defs", "os-release", "pulse", "alsa", "asound.conf",
    "gtk-3.0", "gtk-2.0", "machine-info", "locale.conf", "vconsole.conf", "pipewire", "egl", "vulkan", "inputrc",
    "bashrc", "set-environment", "terminfo"];
const THEME: &[&str] = &[".config/gtk-3.0/settings.ini", ".config/gtk-3.0/gtk.css", ".config/gtk-4.0/settings.ini",
    ".config/gtk-4.0/gtk.css", ".config/qt5ct", ".config/qt6ct", ".config/kdeglobals", ".config/Kvantum",
    ".config/fontconfig", ".gtkrc-2.0", ".icons", ".local/share/fonts", ".local/share/icons/default"];
const THEME_MAX: i64 = 200 * 1024 * 1024;
const THEME_MAX_FILES: i64 = 50000;
const THEME_SKIP: &[&str] = &["bookmarks", "*.lock"];
const THEME_NEVER_INSIDE: &[&str] = &["/proc", "/sys", "/dev", "/run", "/tmp", "/var", "/root", "/boot"];
const PORTALS: &[&str] = &["FileChooser", "OpenURI", "Notification", "Settings", "Email", "Print", "Trash", "Inhibit",
    "NetworkMonitor", "ProxyResolver", "PowerProfileMonitor", "MemoryMonitor", "GameMode", "Realtime", "Secret",
    "Request", "Session", "GlobalShortcuts"];
const MOZ_APPS: &[&str] = &["firefox", "firefox-esr", "firefox-beta", "firefox-devedition", "firefox-nightly", "librewolf"];
const XDG_OPEN: &str = "#!/bin/sh\n# zone xdg-open (links = \"{links}\"): http(s) URLs open in zone {target}'s\n# browser through the zone's link socket; anything else goes to the real xdg-open.\ncase \"$1\" in\n  http://*|https://*)\n    exec {open_link} \"$1\" ;;\nesac\nexec {xdg_open} \"$@\"\n";
const PY_OPEN_LINK: &str = "{python} -c 'import socket,sys; s=socket.socket(socket.AF_UNIX); s.connect(\"/run/zone/links.sock\"); s.sendall(sys.argv[1].encode()+b\"\\n\"); sys.exit(0 if s.recv(3)==b\"ok\\n\" else 1)'";

fn sh(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn rp(p: &str) -> String {
    realpath(p.as_bytes()).map(|r| String::from_utf8_lossy(&r).into_owned()).unwrap_or_else(|| p.to_string())
}

fn lexists(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok()
}

fn warn(msg: &str) {
    eprintln!("appzones: {msg}");
}

fn say(msg: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{msg}");
    let _ = out.flush();
}

/// shutil.which: NAME (a path when it has a slash) on PATH, an executable file.
pub fn which(name: &str, path: Option<&str>) -> Option<String> {
    let exe = |p: &Path| {
        std::fs::metadata(p).is_ok_and(|m| m.is_file())
            && std::ffi::CString::new(p.as_os_str().as_bytes())
                // Safety: access on a NUL-terminated path.
                .is_ok_and(|c| unsafe { libc::access(c.as_ptr(), libc::X_OK) } == 0)
    };
    if name.contains('/') {
        return exe(Path::new(name)).then(|| name.to_string());
    }
    let env_path = std::env::var("PATH").ok();
    let path = path.map(String::from).or(env_path).unwrap_or_else(|| "/bin:/usr/bin".into());
    path.split(':').filter(|d| !d.is_empty()).map(|d| Path::new(d).join(name)).find(|p| exe(p)).map(|p| sh(&p))
}

/// The host as appzones sees it (src/appzones/common.py's constants).
pub struct Host {
    pub here: PathBuf,
    pub python: String,
    pub uid: u32,
    pub gid: u32,
    pub user: String,
    pub home: PathBuf,
    pub host_rt: PathBuf,
    pub rt: PathBuf,
    pub state: PathBuf,
    pub config: PathBuf,
    pub helper: String,
    implements: std::cell::RefCell<Vec<(String, String, bool)>>,
}

impl Host {
    pub fn new(here: PathBuf, python: String) -> Host {
        // Safety: no preconditions.
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        let home = PathBuf::from(OsString::from_vec(config::home_real()));
        let user = std::env::var("USER").ok().filter(|u| !u.is_empty())
            .or_else(|| std::env::var("LOGNAME").ok().filter(|u| !u.is_empty())).unwrap_or_else(|| "user".into());
        let host_rt = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(format!("/run/user/{uid}")));
        let state = match config::env("STATE").filter(|s| !s.is_empty()) {
            Some(s) => PathBuf::from(s),
            None => {
                let new = home.join(".local/share/appzones");
                let old = home.join(".local/share/zones");
                if !lexists(&new) && lexists(&old) { old } else { new }
            }
        };
        let config = match config::env("CONFIG").filter(|c| !c.is_empty()) {
            Some(c) => PathBuf::from(c),
            None => {
                let new = home.join(".config/appzones/zones.toml");
                let old = home.join(".config/zones/zones.toml");
                if !lexists(&new) && lexists(&old) { old } else { new }
            }
        };
        Host { rt: host_rt.join("appzones"), here, python, uid, gid, user, home, host_rt, state, config,
               helper: config::env("HELPER").unwrap_or_default(), implements: Default::default() }
    }

    fn zrt_in(&self) -> String {
        format!("/run/user/{}", self.uid)
    }

    /// common.find_tool.
    pub fn find_tool(&self, name: &str) -> Option<String> {
        config::env(&format!("BIN_{}", name.to_uppercase().replace('-', "_"))).filter(|v| !v.is_empty())
            .or_else(|| which(name, None))
            .or_else(|| {
                let dirs: Vec<String> = std::iter::once(sh(&self.here)).chain(SBIN_DIRS.iter().map(|s| s.to_string())).collect();
                which(name, Some(&dirs.join(":")))
            })
    }

    fn tool(&self, name: &str) -> Result<String, String> {
        self.find_tool(name).ok_or_else(|| format!("{name} not found on PATH, {}", SBIN_DIRS.join(", ")))
    }

    fn implements(&self, helper: &str, command: &str) -> bool {
        if let Some((_, _, v)) = self.implements.borrow().iter().find(|(h, c, _)| h == helper && c == command) {
            return *v;
        }
        let v = Command::new(helper).arg("--commands").stdin(Stdio::null()).stderr(Stdio::null()).output()
            .is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).split_whitespace().any(|c| c == command));
        self.implements.borrow_mut().push((helper.into(), command.into(), v));
        v
    }

    /// common.rust_helper.
    fn rust_helper(&self, command: &str) -> Option<String> {
        let candidates: Vec<String> = match config::env("BIN_APPZONES_HELPER").filter(|v| !v.is_empty()) {
            Some(o) if o.contains('/') => vec![o],
            Some(o) => vec![which(&o, None).unwrap_or(o)],
            None => {
                let own = self.here.join("appzones-helper");
                let mut c = if own.is_file() { vec![sh(&own)] } else { vec![] };
                c.push(which("appzones-helper", None).unwrap_or_default());
                c
            }
        };
        candidates.into_iter().find(|p| !p.is_empty() && self.implements(p, command))
    }

    fn zone_helper(&self) -> Option<String> {
        if self.helper == "rust" { self.rust_helper("zone-init") } else { None }
    }

    fn zone_helper_has(&self, command: &str) -> bool {
        self.zone_helper().is_some_and(|h| RUST_HELPERS.contains(&command) && self.implements(&h, command))
    }

    /// common.helper_argv.
    fn helper_argv(&self, name: &str) -> Option<Vec<String>> {
        if let Some(o) = config::env(&format!("BIN_{}", name.to_uppercase().replace('-', "_"))).filter(|v| !v.is_empty()) {
            return Some(vec![o]);
        }
        if self.helper == "rust" && RUST_HELPERS.contains(&name) {
            return self.rust_helper(name).map(|p| vec![p, name.into()]);
        }
        self.find_tool(name).map(|p| vec![p])
    }
}

fn layout() -> &'static str {
    match config::env("LAYOUT").as_deref() {
        Some("nixos") => "nixos",
        Some("fhs") => "fhs",
        _ if Path::new("/run/current-system").exists() => "nixos",
        _ => "fhs",
    }
}

/// fs.system_binds.
fn system_binds(layout: &str) -> Vec<String> {
    if layout == "nixos" {
        return v(&["--ro-bind", "/nix/store", "/nix/store"]);
    }
    let mut a = v(&["--ro-bind", "/usr", "/usr"]);
    for d in ["/bin", "/sbin", "/lib", "/lib64", "/lib32", "/libx32"] {
        let p = Path::new(d);
        match std::fs::symlink_metadata(p) {
            Ok(m) if m.file_type().is_symlink() => {
                a.extend([s("--symlink"), std::fs::read_link(p).map(|t| sh(&t)).unwrap_or_default(), s(d)]);
            }
            _ if p.is_dir() => a.extend(v(&["--ro-bind", d, d])),
            _ => {}
        }
    }
    a
}

fn s(x: &str) -> String {
    x.to_string()
}

fn v(x: &[&str]) -> Vec<String> {
    x.iter().map(|y| y.to_string()).collect()
}

/// fs.system_env: PATH (the system part), XDG_DATA_DIRS, TZDIR.
fn system_env(layout: &str, user: &str) -> (String, String, String) {
    if layout == "nixos" {
        return (format!("/etc/profiles/per-user/{user}/bin:/run/current-system/sw/bin"),
                format!("/etc/profiles/per-user/{user}/share:/run/current-system/sw/share"), s("/etc/zoneinfo"));
    }
    (s("/usr/local/bin:/usr/bin:/usr/local/sbin:/usr/sbin:/bin:/sbin"), s("/usr/local/share:/usr/share"),
     s("/usr/share/zoneinfo"))
}

/// One path is P itself or inside D (Path.is_relative_to), both normalized.
fn rel_to(p: &str, d: &str) -> bool {
    p == d || d == "/" || p.strip_prefix(d).is_some_and(|r| r.starts_with('/'))
}

/// zonecfg.theme_source_error.
fn theme_source_error(policy: &Policy, real: &str) -> Option<String> {
    let home_real = rp(&policy.home.to_string_lossy());
    if !rel_to(real, &home_real) {
        if let Some(d) = THEME_NEVER_INSIDE.iter().find(|d| rel_to(real, d)) {
            return Some(format!("{real} is inside {d}"));
        }
    }
    let denied = match policy.denied_paths() {
        Ok(d) => d,
        Err(e) => return Some(e),
    };
    for d in denied {
        let shown = String::from_utf8_lossy(&d).into_owned();
        let dr = rp(&shown);
        if rel_to(real, &dr) {
            return Some(format!("{real} is inside {shown}, which never enters a zone"));
        }
    }
    None
}

/// zonecfg.fd_path: the path of an open descriptor, None when unlinked or not absolute.
fn fd_path(fd: i32) -> Option<String> {
    let t = std::fs::read_link(format!("/proc/self/fd/{fd}")).ok()?;
    let t = sh(&t);
    (!t.ends_with(" (deleted)") && t.starts_with('/')).then_some(t)
}

struct Budget;

struct Theme<'a> {
    policy: &'a Policy,
    left: [i64; 2],
}

impl Theme<'_> {
    fn file(&mut self, src: &Path, dst: &Path, count: bool) -> Result<Result<(), Budget>, std::io::Error> {
        use std::os::fd::AsRawFd;
        let Ok(f) = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK).open(src) else {
            return Ok(Ok(())); // dangling link, no permission: skip quietly
        };
        let ok = f.metadata().is_ok_and(|m| m.is_file())
            && fd_path(f.as_raw_fd()).is_some_and(|r| theme_source_error(self.policy, &r).is_none());
        if !ok {
            warn(&format!("theme: not copying {} (it resolves to a path that never enters a zone)", sh(src)));
            return Ok(Ok(()));
        }
        if count {
            self.left[1] -= 1;
            if self.left[1] < 0 {
                return Ok(Err(Budget));
            }
        }
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::fs::File::create(dst)?;
        let mut f = f;
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            self.left[0] -= n as i64;
            if self.left[0] < 0 {
                return Ok(Err(Budget));
            }
            out.write_all(&buf[..n])?;
        }
        Ok(Ok(()))
    }

    fn dir(&mut self, src: &Path, dst: &Path, seen: &mut BTreeSet<String>, depth: usize) -> Result<Result<(), Budget>, std::io::Error> {
        let real = rp(&sh(src));
        if depth > 12 || seen.contains(&real) || theme_source_error(self.policy, &real).is_some() {
            return Ok(Ok(()));
        }
        seen.insert(real);
        let Ok(it) = std::fs::read_dir(src) else { return Ok(Ok(())) };
        for e in it {
            let Ok(e) = e else { continue };
            self.left[1] -= 1;
            if self.left[1] < 0 {
                return Ok(Err(Budget));
            }
            let name = e.file_name().to_string_lossy().into_owned();
            if THEME_SKIP.iter().any(|p| fnmatchcase(&name, p)) {
                continue;
            }
            let Ok(is_dir) = std::fs::metadata(e.path()).map(|m| m.is_dir()) else { continue };
            let r = if is_dir {
                self.dir(&e.path(), &dst.join(e.file_name()), seen, depth + 1)?
            } else {
                self.file(&e.path(), &dst.join(e.file_name()), false)?
            };
            if r.is_err() {
                return Ok(r);
            }
        }
        Ok(Ok(()))
    }
}

/// fs.copy_theme: the theme files copied into the zone's state; (copy, path inside) to bind.
fn copy_theme(host: &Host, policy: &Policy, zdir: &Path) -> Vec<(String, String)> {
    let root = zdir.join("theme");
    let _ = std::fs::remove_dir_all(&root);
    let mut t = Theme { policy, left: [THEME_MAX, THEME_MAX_FILES] };
    let mut out = Vec::new();
    for rel in THEME {
        let src = host.home.join(rel);
        if !src.exists() {
            continue;
        }
        let dst = root.join(rel);
        let before = t.left;
        let r = if src.is_dir() { t.dir(&src, &dst, &mut BTreeSet::new(), 0) } else { t.file(&src, &dst, true) };
        match r {
            Ok(Err(Budget)) => {
                warn(&format!("theme: skipping ~/{rel} (over the budget of {} MiB / {THEME_MAX_FILES} files)",
                              THEME_MAX >> 20));
                if dst.is_dir() { let _ = std::fs::remove_dir_all(&dst); } else { let _ = std::fs::remove_file(&dst); }
                t.left = before;
                continue;
            }
            Err(e) => {
                warn(&format!("theme: ~/{rel}: {e}"));
                continue;
            }
            Ok(Ok(())) => {}
        }
        if dst.exists() {
            out.push((sh(&dst), sh(&host.home.join(rel))));
        }
    }
    out
}

fn sorted_glob(dir: &str, prefix: &str) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir).map(|it| it.filter_map(|e| e.ok())
        .filter(|e| e.file_name().as_bytes().starts_with(prefix.as_bytes()) && !e.file_name().as_bytes().starts_with(b"."))
        .map(|e| format!("{dir}/{}", e.file_name().to_string_lossy())).collect()).unwrap_or_default();
    out.sort();
    out
}

/// fs.gpu_binds: render nodes only, and the sysfs libdrm and Mesa read.
fn gpu_binds() -> Vec<String> {
    let mut a = Vec::new();
    for node in sorted_glob("/dev/dri", "renderD") {
        a.extend([s("--dev-bind"), node.clone(), node.clone()]);
        let name = Path::new(&node).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let dev = format!("/sys/class/drm/{name}/device");
        if Path::new(&dev).exists() {
            let real = rp(&dev);
            a.extend([s("--ro-bind"), real.clone(), real]);
        }
    }
    if !a.is_empty() {
        a.extend(v(&["--ro-bind-try", "/sys/dev/char", "/sys/dev/char"]));
    }
    a
}

fn read_trim(p: &Path) -> String {
    std::fs::read_to_string(p).map(|t| t.trim().to_string()).unwrap_or_default()
}

/// fs.usb_binds: the USB devices matching IDS, their nodes, sysfs and hidraw nodes.
fn usb_binds(ids: &[String]) -> (Vec<String>, Vec<String>) {
    let want: Vec<(String, Option<String>)> = ids.iter().map(|i| match i.split_once(':') {
        Some((v, p)) => (v.to_string(), Some(p.to_string())),
        None => (i.clone(), None),
    }).collect();
    let (mut a, mut found) = (Vec::new(), Vec::new());
    let Ok(rd) = std::fs::read_dir("/sys/bus/usb/devices") else { return (a, found) };
    let mut entries: Vec<String> = rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    entries.sort();
    let hidraws = sorted_glob("/sys/class/hidraw", "hidraw");
    let mut any_hid = false;
    for e in entries {
        let d = Path::new("/sys/bus/usb/devices").join(&e);
        let vid = read_trim(&d.join("idVendor")).to_lowercase();
        let pid = read_trim(&d.join("idProduct")).to_lowercase();
        if vid.is_empty() || !want.iter().any(|(wv, wp)| *wv == vid && wp.as_ref().is_none_or(|p| *p == pid)) {
            continue;
        }
        let real = rp(&sh(&d));
        let (bus, dev, majmin) = (read_trim(&d.join("busnum")), read_trim(&d.join("devnum")), read_trim(&d.join("dev")));
        let (Ok(bn), Ok(dn)) = (bus.parse::<u32>(), dev.parse::<u32>()) else { continue };
        if !bus.bytes().all(|b| b.is_ascii_digit()) || !dev.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let node = format!("/dev/bus/usb/{bn:03}/{dn:03}");
        let Ok(st) = std::fs::metadata(&node) else {
            warn(&format!("usb: {vid}:{pid} ({e}) has no {node}; skipped"));
            continue;
        };
        let rdev = st.rdev();
        let mm = format!("{}:{}", libc::major(rdev), libc::minor(rdev));
        if !majmin.is_empty() && mm != majmin {
            warn(&format!("usb: {node} is not {vid}:{pid} ({e}); skipped"));
            continue;
        }
        a.extend([s("--dev-bind"), node.clone(), node.clone(), s("--ro-bind"), real.clone(), real.clone()]);
        if !majmin.is_empty() {
            let u = format!("/run/udev/data/c{majmin}");
            a.extend([s("--ro-bind-try"), u.clone(), u]);
        }
        let mut what = vec![node];
        for h in &hidraws {
            let hdev = rp(&format!("{h}/device"));
            if !hdev.starts_with(&format!("{real}/")) {
                continue;
            }
            let hn = format!("/dev/{}", Path::new(h).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
            let hmm = read_trim(&Path::new(h).join("dev"));
            if !Path::new(&hn).exists() {
                continue;
            }
            a.extend([s("--dev-bind"), hn.clone(), hn.clone()]);
            if !hmm.is_empty() {
                let u = format!("/run/udev/data/c{hmm}");
                a.extend([s("--ro-bind-try"), u.clone(), u]);
            }
            what.push(hn);
            any_hid = true;
        }
        let product = read_trim(&d.join("product"));
        found.push(format!("{vid}:{pid} {} ({})", if product.is_empty() { &e } else { &product }, what.join(", ")));
    }
    if !a.is_empty() {
        a.extend(v(&["--ro-bind-try", "/sys/bus/usb", "/sys/bus/usb"]));
        if any_hid {
            a.extend(v(&["--ro-bind-try", "/sys/class/hidraw", "/sys/class/hidraw"]));
        }
    }
    (a, found)
}

fn strs_of(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

/// fs.package_paths: the zone's extra packages built with Nix; their bin directories.
fn package_paths(z: &Zone, zdir: &Path) -> Vec<String> {
    let attrs = strs_of(z.get("packages"));
    if which("nix", None).is_none() {
        warn(&format!("packages: {} not built: packages need Nix (nix on PATH)", attrs.join(" ")));
        return vec![];
    }
    let roots = zdir.join("packages");
    let _ = std::fs::create_dir(&roots);
    let mut out = Vec::new();
    for attr in attrs {
        let link = roots.join(&attr);
        let expr = format!("(import (builtins.getFlake \"nixpkgs\").outPath {{ system = builtins.currentSystem; \
                            config.allowUnfree = true; }}).{attr}");
        let r = Command::new("nix").args(["build", "--impure", "--out-link"]).arg(&link).args(["--expr", &expr])
            .stdin(Stdio::null()).output();
        match r {
            Ok(o) if o.status.success() => out.push(format!("{}/bin", rp(&sh(&link)))),
            Ok(o) => {
                let err = String::from_utf8_lossy(&o.stderr);
                let last: Vec<&str> = err.trim().lines().last().into_iter().collect();
                let why = if last.is_empty() { o.status.code().unwrap_or(1).to_string() } else { repr_strs(&last) };
                warn(&format!("packages: {attr} did not build: {why}"));
            }
            Err(e) => warn(&format!("packages: {attr} did not build: {e}")),
        }
    }
    out
}

/// fs.clean_old: files unused for DAYS (atime and mtime; symlinks and directories by
/// mtime), then directories unchanged for DAYS once empty. Never follows a link.
fn clean_old(root: &Path, days: u64) -> usize {
    let cutoff = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
        - (days * 86400) as f64;
    fn walk(d: &Path, cutoff: f64) -> usize {
        let mut n = 0;
        let Ok(it) = std::fs::read_dir(d) else { return 0 };
        let entries: Vec<_> = it.filter_map(|e| e.ok()).collect();
        for e in entries {
            let Ok(st) = std::fs::symlink_metadata(e.path()) else { continue };
            let mtime = st.mtime() as f64 + st.mtime_nsec() as f64 / 1e9;
            let atime = st.atime() as f64 + st.atime_nsec() as f64 / 1e9;
            if st.is_dir() {
                n += walk(&e.path(), cutoff);
                if mtime < cutoff {
                    let _ = std::fs::remove_dir(e.path());
                }
            } else if (if st.file_type().is_symlink() { mtime } else { atime.max(mtime) }) < cutoff
                && std::fs::remove_file(e.path()).is_ok() {
                n += 1;
            }
        }
        n
    }
    walk(root, cutoff)
}

fn mkdir_mode(p: &Path, mode: u32) -> std::io::Result<()> {
    match std::fs::DirBuilder::new().mode(mode).create(p) {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        r => r,
    }
}

/// fs.prepare_tmp.
fn prepare_tmp(zdir: &Path) -> Result<(), String> {
    for (sub, days) in [("tmp", 14), ("var-tmp", 30)] {
        let d = zdir.join(sub);
        mkdir_mode(&d, 0o700).map_err(|e| format!("{}: {e}", sh(&d)))?;
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
        let n = clean_old(&d, days);
        if n > 0 {
            say(&format!("[zone] tmp: removed {n} file(s) unused for {days} days from {}", sh(&d)));
        }
    }
    Ok(())
}

/// fs.host_mounts: the names mounted under STATE/ZONE/mnt.
fn host_mounts(host: &Host, zone: &str) -> Vec<String> {
    let pre = format!("{}/", sh(&host.state.join(zone).join("mnt")));
    let mut out: Vec<String> = Vec::new();
    for line in std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default().lines() {
        let Some(mp) = line.split_whitespace().nth(4) else { continue };
        let mp = mp.replace("\\040", " ");
        if let Some(rest) = mp.strip_prefix(&pre) {
            if !rest.contains('/') && !out.iter().any(|o| o == rest) {
                out.push(rest.to_string());
            }
        }
    }
    out
}

/// fs.prepare_mnt: no runtime mount outlives a zone run; false when some are left.
fn prepare_mnt(host: &Host, zone: &str, zdir: &Path) -> Result<bool, String> {
    let mnt = zdir.join("mnt");
    mkdir_mode(&mnt, 0o700).map_err(|e| format!("{}: {e}", sh(&mnt)))?;
    std::fs::set_permissions(&mnt, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    if !host_mounts(host, zone).is_empty() {
        let sudo = config::env("SUDO").filter(|v| !v.is_empty()).unwrap_or_else(|| {
            if Path::new("/run/wrappers/bin/sudo").exists() { s("/run/wrappers/bin/sudo") }
            else { which("sudo", None).unwrap_or_else(|| s("/usr/bin/sudo")) }
        });
        let helper = config::env("MOUNT_HELPER").filter(|v| !v.is_empty())
            .unwrap_or_else(|| sh(&host.here.join("appzones-mount-helper")));
        if let Ok(o) = Command::new(sudo).args(["-n", &helper, "umount-all", zone]).stdin(Stdio::null()).output() {
            let out = if o.stdout.is_empty() { o.stderr } else { o.stdout };
            say(&format!("[zone] mounts: {}", String::from_utf8_lossy(&out).trim()));
        }
    }
    if let Ok(it) = std::fs::read_dir(&mnt) {
        for e in it.filter_map(|e| e.ok()) {
            let is_link = std::fs::symlink_metadata(e.path()).is_ok_and(|m| m.file_type().is_symlink());
            if e.path().is_dir() && !is_link {
                let _ = std::fs::remove_dir(e.path()); // empty mount points only
            }
        }
    }
    Ok(host_mounts(host, zone).is_empty())
}

/// An IP network (address, prefix).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Net {
    v6: bool,
    addr: u128,
    prefix: u8,
}

impl Net {
    fn bits(&self) -> u8 {
        if self.v6 { 128 } else { 32 }
    }

    fn mask(&self) -> u128 {
        let b = self.bits();
        let full = if b == 128 { u128::MAX } else { (1u128 << b) - 1 };
        if self.prefix == 0 { 0 } else { full & !((1u128 << (b - self.prefix)).wrapping_sub(1)) & full }
    }

    fn of(ip: IpAddr, prefix: u8) -> Net {
        let (v6, addr) = match ip {
            IpAddr::V4(a) => (false, u32::from(a) as u128),
            IpAddr::V6(a) => (true, u128::from(a)),
        };
        let mut n = Net { v6, addr, prefix };
        n.addr &= n.mask();
        n
    }

    fn last(&self) -> u128 {
        let b = self.bits();
        let full = if b == 128 { u128::MAX } else { (1u128 << b) - 1 };
        self.addr | (!self.mask() & full)
    }

    fn show(&self) -> String {
        let a = if self.v6 { Ipv6Addr::from(self.addr).to_string() } else { Ipv4Addr::from(self.addr as u32).to_string() };
        format!("{a}/{}", self.prefix)
    }
}

fn show_ip(v6: bool, a: u128) -> String {
    if v6 { Ipv6Addr::from(a).to_string() } else { Ipv4Addr::from(a as u32).to_string() }
}

/// ipaddress.collapse_addresses: the smallest set of networks covering NETS, in order.
fn collapse(nets: &[Net]) -> Vec<Net> {
    let Some(first) = nets.first() else { return vec![] };
    let v6 = first.v6;
    let bits = first.bits();
    let mut ranges: Vec<(u128, u128)> = nets.iter().map(|n| (n.addr, n.last())).collect();
    ranges.sort();
    let mut merged: Vec<(u128, u128)> = Vec::new();
    for (lo, hi) in ranges {
        match merged.last_mut() {
            Some((_, mhi)) if *mhi == u128::MAX || lo <= *mhi + 1 => *mhi = (*mhi).max(hi),
            _ => merged.push((lo, hi)),
        }
    }
    let mut out = Vec::new();
    for (mut lo, hi) in merged {
        // the last address of the block of 2^size addresses from lo
        let block_end = |lo: u128, size: u32| if size >= 128 { u128::MAX } else { lo + ((1u128 << size) - 1) };
        loop {
            // the biggest block that starts at lo and ends by hi
            let mut size = if lo == 0 { bits as u32 } else { lo.trailing_zeros().min(bits as u32) };
            while size > 0 && block_end(lo, size) > hi {
                size -= 1;
            }
            out.push(Net { v6, addr: lo, prefix: bits - size as u8 });
            let end = block_end(lo, size);
            if end >= hi {
                break;
            }
            lo = end + 1;
        }
    }
    out
}

/// net.host_addrs: (interface, address, prefix) from `ip -j addr show`.
fn host_addrs() -> Result<Vec<(String, IpAddr, u8)>, String> {
    let o = Command::new("ip").args(["-j", "addr", "show"]).stdin(Stdio::null()).output()
        .map_err(|e| format!("ip -j addr show: {e}"))?;
    if !o.status.success() {
        let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
        return Err(format!("ip -j addr show: {}", if err.is_empty() { o.status.code().unwrap_or(1).to_string() } else { err }));
    }
    let links: J = serde_json::from_slice(if o.stdout.is_empty() { b"[]" } else { &o.stdout }).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for link in links.as_array().cloned().unwrap_or_default() {
        let ifname = link.get("ifname").and_then(J::as_str).unwrap_or("").to_string();
        for a in link.get("addr_info").and_then(J::as_array).cloned().unwrap_or_default() {
            let (Some(local), Some(plen)) = (a.get("local").and_then(J::as_str), a.get("prefixlen").and_then(J::as_u64)) else { continue };
            let Ok(ip) = local.parse::<IpAddr>() else { continue };
            if plen > if ip.is_ipv6() { 128 } else { 32 } {
                continue;
            }
            out.push((ifname.clone(), ip, plen as u8));
        }
    }
    Ok(out)
}

/// net.nft_rules: the deny list loaded in the zone's own netns.
fn nft_rules(z: &Zone) -> Result<String, String> {
    let addrs: Vec<(String, IpAddr, u8)> = host_addrs()?.into_iter().filter(|(_, ip, _)| !ip.is_loopback()).collect();
    let mut mine4: Vec<u128> = addrs.iter().filter_map(|(_, ip, _)| match ip { IpAddr::V4(a) => Some(u32::from(*a) as u128), _ => None }).collect();
    let mut mine6: Vec<u128> = addrs.iter().filter_map(|(_, ip, _)| match ip { IpAddr::V6(a) => Some(u128::from(*a)), _ => None }).collect();
    mine4.sort();
    mine4.dedup();
    mine6.sort();
    mine6.dedup();
    let link_local = |ip: &IpAddr| match ip {
        IpAddr::V4(a) => a.octets()[0] == 169 && a.octets()[1] == 254,
        IpAddr::V6(a) => (a.segments()[0] & 0xffc0) == 0xfe80,
    };
    let nets: Vec<Net> = addrs.iter().filter(|(_, ip, _)| !link_local(ip)).map(|(_, ip, p)| Net::of(*ip, *p)).collect();
    let (mut nb4, mut nb6) = (Vec::new(), Vec::new());
    if crate::config::truthy(z.get("netbird")) {
        let iface = config::env("NETBIRD_IFACE").filter(|v| !v.is_empty()).unwrap_or_else(|| s("wt0"));
        let nbn: Vec<Net> = addrs.iter().filter(|(i, _, _)| *i == iface).map(|(_, ip, p)| Net::of(*ip, *p)).collect();
        nb4 = collapse(&nbn.iter().copied().filter(|n| !n.v6).collect::<Vec<_>>());
        nb6 = collapse(&nbn.iter().copied().filter(|n| n.v6).collect::<Vec<_>>());
    }
    let lan4 = collapse(&nets.iter().copied().filter(|n| !n.v6 && !nb4.contains(n)).collect::<Vec<_>>());
    let lan6 = collapse(&nets.iter().copied().filter(|n| n.v6 && !nb6.contains(n)).collect::<Vec<_>>());
    let join_ips = |v6: bool, x: &[u128]| x.iter().map(|a| show_ip(v6, *a)).collect::<Vec<_>>().join(", ");
    let join_nets = |x: &[Net]| x.iter().map(Net::show).collect::<Vec<_>>().join(", ");
    let mut rules = String::new();
    if !mine4.is_empty() {
        rules += &format!("    ip daddr {{ {} }} reject\n", join_ips(false, &mine4));
    }
    if !mine6.is_empty() {
        rules += &format!("    ip6 daddr {{ {} }} reject\n", join_ips(true, &mine6));
    }
    if !nb4.is_empty() {
        rules += &format!("    ip daddr {{ {} }} accept\n", join_nets(&nb4));
    }
    if !nb6.is_empty() {
        rules += &format!("    ip6 daddr {{ {} }} accept\n", join_nets(&nb6));
    }
    if !lan4.is_empty() {
        rules += &format!("    ip daddr {{ {} }} reject\n", join_nets(&lan4));
    }
    if !lan6.is_empty() {
        rules += &format!("    ip6 daddr {{ {} }} reject\n", join_nets(&lan6));
    }
    Ok(format!("table inet zone {{\n  chain out {{\n    type filter hook output priority 0; policy accept;\n    oif \"lo\" accept\n\
{rules}    ip daddr {{ 0.0.0.0/8, 10.0.0.0/8, 100.64.0.0/10, 127.0.0.0/8, 169.254.0.0/16, 172.16.0.0/12,\n               \
192.0.0.0/24, 192.168.0.0/16, 198.18.0.0/15, 224.0.0.0/4, 240.0.0.0/4 }} reject\n    ip6 daddr {{ ::1, ::ffff:0:0/96, \
fc00::/7, fe80::/10, ff00::/8 }} reject\n  }}\n}}\n"))
}

/// net.resolv_conf.
fn resolv_conf(z: &Zone) -> String {
    match z.get("network").as_str() {
        Some("internet") => format!("nameserver {DNS_ADDR}\n"),
        Some("host") if z.get("dns").as_str() == Some("public") => s("nameserver 1.1.1.1\n"),
        Some("host") => match config::env("DNS_HOST").filter(|v| !v.is_empty()) {
            Some(d) => format!("nameserver {d}\n"),
            None => std::fs::read_to_string("/etc/resolv.conf").unwrap_or_default(),
        },
        _ => String::new(),
    }
}

/// net.host_resolver.
fn host_resolver() -> String {
    if let Some(d) = config::env("DNS_HOST").filter(|v| !v.is_empty()) {
        return d;
    }
    let Ok(text) = std::fs::read_to_string("/etc/resolv.conf") else { return s(DNS_ADDR) };
    for line in crate::policy::split_lines(&text) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 && parts[0] == "nameserver" {
            // the first nameserver; one that is not an address ends the search (as Python's try)
            return if parts[1].split('%').next().unwrap_or("").parse::<IpAddr>().is_ok() { s(parts[1]) } else { s(DNS_ADDR) };
        }
    }
    s(DNS_ADDR)
}

fn moz_remoting_name(zone: &str, app: &str) -> String {
    format!("{}_zone_{}", app.replace('-', "_"), zone.replace('-', "_"))
}

/// portals.dbus_rules.
fn dbus_rules(z: &Zone) -> Vec<String> {
    let devices = z.get("devices");
    let mut allowed: Vec<&str> = PORTALS.to_vec();
    for (d, ps) in [("screencast", &["ScreenCast", "Screenshot"][..]), ("camera", &["Camera"][..])] {
        if devices.as_array().is_some_and(|a| a.iter().any(|x| x.as_str() == Some(d))) {
            for p in ps {
                if !allowed.contains(p) {
                    allowed.push(p);
                }
            }
        }
    }
    let mut r = v(&["--call=org.freedesktop.portal.Desktop=org.freedesktop.DBus.Properties.*",
                    "--call=org.freedesktop.portal.Desktop=org.freedesktop.DBus.Introspectable.*",
                    "--call=org.freedesktop.portal.Desktop=org.freedesktop.DBus.Peer.*"]);
    r.extend(allowed.iter().map(|p| format!("--call=org.freedesktop.portal.Desktop=org.freedesktop.portal.{p}.*")));
    r.extend(v(&["--broadcast=org.freedesktop.portal.Desktop=@/org/freedesktop/portal/*",
                 "--talk=org.freedesktop.portal.Documents", "--talk=org.freedesktop.Notifications",
                 "--talk=org.kde.StatusNotifierWatcher"]));
    r.push(format!("--own=dev.mirql.zone.{}.*", z.name));
    r.extend(MOZ_APPS.iter().map(|a| format!("--own=org.mozilla.{}.*", moz_remoting_name(&z.name, a))));
    let dbus = z.get("dbus").as_table();
    r.extend(dbus.and_then(|t| t.get("talk")).map(strs_of).unwrap_or_default().iter().map(|n| format!("--talk={n}")));
    r.extend(dbus.and_then(|t| t.get("own")).map(strs_of).unwrap_or_default().iter().map(|n| format!("--own={n}")));
    if config::env("DBUS_LOG").is_some_and(|v| !v.is_empty()) {
        r.push(s("--log"));
    }
    r
}

/// An ordered environment, updated in place as a Python dict.
struct EnvList(Vec<(String, String)>);

impl EnvList {
    fn set(&mut self, k: &str, v: String) {
        match self.0.iter_mut().find(|(n, _)| n == k) {
            Some(e) => e.1 = v,
            None => self.0.push((k.into(), v)),
        }
    }
}

fn environ(k: &str) -> Option<String> {
    std::env::var_os(k).map(|v| v.to_string_lossy().into_owned())
}

/// fs.bwrap_argv(plan=True): the zone's bwrap command line, the parts that depend on the
/// helpers marked.
fn bwrap_argv(host: &Host, policy: &Policy, z: &Zone, zdir: &Path, zrt: &Path, network: &str,
              extra_path: &[String]) -> Result<Vec<String>, String> {
    let st = |k: &str| z.get(k);
    let home_setting = st("home").as_str().unwrap_or("").to_string();
    let home = if home_setting.is_empty() {
        zdir.join("home")
    } else {
        // os.path.expanduser: the real home
        match home_setting.strip_prefix('~') {
            Some(rest) if rest.is_empty() || rest.starts_with('/') => PathBuf::from(format!("{}{rest}", sh(&host.home))),
            _ => PathBuf::from(&home_setting),
        }
    };
    if !home_setting.is_empty() {
        if let Some(e) = policy.bind_error(home_setting.as_bytes()).filter(|e| !e.ends_with(": does not exist")) {
            return Err(format!("home {e}"));
        }
    }
    std::fs::create_dir_all(&home).map_err(|e| format!("{}: {e}", sh(&home)))?;
    let bwrap = host.tool("bwrap")?;
    let zrt_in = host.zrt_in();
    let mut a = vec![bwrap.clone()];
    a.extend(v(&["--unshare-user", "--unshare-ipc", "--unshare-pid", "--unshare-uts", "--unshare-cgroup", "--uid"]));
    a.extend([host.uid.to_string(), s("--gid"), host.gid.to_string(), s("--hostname"), format!("{}-zone", z.name)]);
    a.extend(v(&["--die-with-parent", "--new-session", "--cap-drop", "ALL", "--clearenv"]));
    if network == "none" {
        a.push(s("--unshare-net"));
    }
    if !crate::config::truthy(st("userns")) {
        a.extend(v(&["--disable-userns", "--assert-userns-disabled"]));
    }
    a.extend(v(&["--proc", "/proc", "--dev", "/dev"]));
    if st("tmp").as_str() == Some("disk") {
        a.extend([s("--bind"), sh(&zdir.join("tmp")), s("/tmp"), s("--bind"), sh(&zdir.join("var-tmp")), s("/var/tmp")]);
    } else {
        a.extend(v(&["--tmpfs", "/tmp"]));
    }
    let layout = layout();
    a.extend([s("--tmpfs"), s("/run"), s("--perms"), s("0700"), s("--tmpfs"), zrt_in.clone(), s("--dir"), s("/run/zone")]);
    a.extend(system_binds(layout));
    if layout == "fhs" && !extra_path.is_empty() {
        a.extend(v(&["--ro-bind", "/nix/store", "/nix/store"]));
    }
    a.extend([s("--ro-bind"), sh(&zdir.join("mnt")), s("/mnt/host")]);
    let mut nix_ld_lib = None;
    if crate::config::truthy(st("nix_ld")) {
        let (shim, lib) = config::host_nix_ld().ok_or("nix_ld = true, but this host has no nix-ld")?;
        a.extend([s("--dir"), s("/lib64"), s("--symlink"), shim, s("/lib64/ld-linux-x86-64.so.2")]);
        nix_ld_lib = Some(lib);
    }
    for link in [s("/run/current-system"), format!("/etc/profiles/per-user/{}", host.user), s("/run/opengl-driver"),
                 s("/run/opengl-driver-32")] {
        if lexists(Path::new(&link)) {
            a.extend([s("--symlink"), rp(&link), link]);
        }
    }
    if layout == "nixos" {
        a.extend(v(&["--symlink", "/run/current-system/sw/bin", "/bin"]));
        a.extend(v(&["--symlink", "/run/current-system/sw/bin/env", "/usr/bin/env"]));
    }
    let etc: Vec<&str> = ETC.iter().copied().chain(if layout == "fhs" { FHS_ETC.to_vec() } else { vec![] }).collect();
    for e in etc {
        let p = Path::new("/etc").join(e);
        if !lexists(&p) || (e == "hosts" && st("dns").as_str() == Some("public")) {
            continue;
        }
        if std::fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink()) {
            a.extend([s("--symlink"), std::fs::read_link(&p).map(|t| sh(&t)).unwrap_or_default(), sh(&p)]);
        } else {
            a.extend([s("--ro-bind"), sh(&p), sh(&p)]);
        }
    }
    if st("machine_id").as_str() == Some("host") && Path::new("/etc/machine-id").exists() {
        a.extend(v(&["--ro-bind", "/etc/machine-id", "/etc/machine-id"]));
    } else {
        a.extend([s("--ro-bind"), sh(&zdir.join("machine-id")), s("/etc/machine-id")]);
    }
    a.extend([s("--ro-bind"), sh(&zrt.join("resolv.conf")), s("/etc/resolv.conf")]);
    if st("dns").as_str() == Some("public") {
        a.extend([s("--ro-bind"), sh(&zrt.join("hosts")), s("/etc/hosts")]);
    }
    if home_setting.is_empty() {
        a.extend([s("--bind"), sh(&home), sh(&host.home)]);
    } else {
        a.extend([s(PIN), s("rw"), home_setting.clone(), sh(&host.home)]);
    }
    if crate::config::truthy(st("theme")) {
        for (src, dst) in copy_theme(host, policy, zdir) {
            a.extend([s("--ro-bind"), src, dst]);
        }
    }
    for b in st("binds").as_array().cloned().unwrap_or_default() {
        let Some(t) = b.as_table() else { continue };
        let path = t.get("path").and_then(Value::as_str).unwrap_or("").to_string();
        let mode = t.get("mode").and_then(Value::as_str).unwrap_or("rw").to_string();
        let dest = String::from_utf8_lossy(&policy.expand(path.as_bytes())).into_owned();
        a.extend([s(PIN), mode, path, dest]);
    }
    let devices = st("devices");
    let has = |d: &str| devices.as_array().is_some_and(|x| x.iter().any(|y| y.as_str() == Some(d)));
    if has("gpu") {
        a.extend(gpu_binds());
    }
    if has("camera") && st("camera").as_str() == Some("device") {
        for vdev in sorted_glob("/dev", "video") {
            a.extend([s("--dev-bind"), vdev.clone(), vdev]);
        }
    }
    if has("usb") {
        let ids = strs_of(st("usb"));
        let (mut ub, found) = usb_binds(&ids);
        if !found.is_empty() {
            say(&format!("[zone] usb: {}", found.join("; ")));
            if !a.iter().any(|x| x == "/sys/dev/char") {
                ub.extend(v(&["--ro-bind-try", "/sys/dev/char", "/sys/dev/char"]));
            }
            a.extend(ub);
        } else {
            warn(&format!("usb: no device matches {} now; the zone starts without one (plug it in, then \
                           `appzones restart {}`)", ids.join(", "), z.name));
        }
    }
    a.extend(v(&["--ro-bind-try", "/sys/devices/system/cpu", "/sys/devices/system/cpu"]));
    a.extend([s("--ro-bind"), sh(&zrt.join("ctl")), s("/run/zone")]);
    if zrt.join("ctl/bin").is_dir() {
        a.extend([s("--ro-bind"), sh(&zrt.join("ctl/bin")), s("/run/zone/bin")]);
    }
    let (spath, sdata, stz) = system_env(layout, &host.user);
    let mut env = EnvList(Vec::new());
    env.set("HOME", sh(&host.home));
    env.set("USER", host.user.clone());
    env.set("LOGNAME", host.user.clone());
    env.set("PATH", extra_path.iter().cloned().chain([s("/run/zone/bin"), spath]).collect::<Vec<_>>().join(":"));
    env.set("LANG", environ("LANG").unwrap_or_else(|| s("C.UTF-8")));
    env.set("TZ", environ("TZ").unwrap_or_else(|| s(":/etc/localtime")));
    env.set("XDG_RUNTIME_DIR", zrt_in.clone());
    env.set("ZONE", z.name.clone());
    env.set("TERM", s("xterm-256color"));
    if network == "internet" {
        env.set("ZONE_DNS", s(DNS_ADDR));
    }
    env.set("XDG_DATA_DIRS", sdata);
    env.set("XDG_CONFIG_DIRS", s("/etc/xdg"));
    env.set("XDG_SESSION_TYPE", s("wayland"));
    env.set("NIXOS_OZONE_WL", s("1"));
    env.set("MOZ_ENABLE_WAYLAND", s("1"));
    env.set("QT_QPA_PLATFORM", s("wayland;xcb"));
    env.set("SDL_VIDEODRIVER", s("wayland"));
    env.set("ELECTRON_OZONE_PLATFORM_HINT", s("wayland"));
    env.set("TZDIR", stz);
    let desk = environ("XDG_CURRENT_DESKTOP").unwrap_or_default();
    if !desk.is_empty() && desk.chars().all(|c| c.is_alphanumeric() || ":_-".contains(c)) {
        env.set("XDG_CURRENT_DESKTOP", desk);
    }
    if let Some(lib) = nix_ld_lib {
        env.set("NIX_LD", format!("{lib}/ld.so"));
        env.set("NIX_LD_LIBRARY_PATH", lib);
    }
    for k in ["LC_ALL", "LC_CTYPE", "LC_TIME", "LC_NUMERIC", "LC_MONETARY", "LC_MESSAGES", "XCURSOR_THEME",
              "XCURSOR_SIZE", "QT_QPA_PLATFORMTHEME", "GTK_THEME", "GDK_SCALE", "QT_SCALE_FACTOR"] {
        if let Some(val) = environ(k).filter(|x| !x.is_empty()) {
            env.set(k, val);
        }
    }
    let zwl = format!("{zrt_in}/wayland-0");
    let wl = st("wayland").as_str().unwrap_or("zone");
    if wl != "none" {
        a.extend([format!("{PLAN_IF}wayland-0"), s("--bind"), sh(&zrt.join("wayland-0")), zwl.clone(), s("--setenv"),
                  s("WAYLAND_DISPLAY"), s("wayland-0"), s(PLAN_END)]);
    }
    if wl == "host" {
        a.extend([format!("{PLAN_IF}wayland-host"), s("--bind"), s(PLAN_HOST_WAYLAND), zwl, s("--setenv"),
                  s("WAYLAND_DISPLAY"), s("wayland-0"), s(PLAN_END)]);
    }
    a.extend([format!("{PLAN_IF}bus"), s("--bind"), sh(&zrt.join("bus")), format!("{zrt_in}/bus"), s("--setenv"),
              s("DBUS_SESSION_BUS_ADDRESS"), format!("unix:path={zrt_in}/bus"), s(PLAN_END)]);
    if let Some(daemon) = host.find_tool("dbus-daemon") {
        env.set("ZONE_DBUS_DAEMON", rp(&daemon));
    }
    env.set("ZONE_BWRAP", rp(&bwrap));
    if has("audio") || has("mic") {
        if host.host_rt.join("pipewire-0").exists() {
            a.extend([s("--bind"), sh(&host.host_rt.join("pipewire-0")), format!("{zrt_in}/pipewire-0")]);
        }
        if host.host_rt.join("pulse/native").exists() {
            a.extend([s("--bind"), sh(&host.host_rt.join("pulse/native")), format!("{zrt_in}/pulse/native")]);
            env.set("PULSE_SERVER", format!("unix:{zrt_in}/pulse/native"));
        }
    }
    let doc = host.host_rt.join("doc/by-app").join(format!("dev.mirql.zone.{}", z.name));
    if doc.is_dir() {
        a.extend([s("--bind"), sh(&doc), format!("{zrt_in}/doc")]);
    }
    a.extend([s("--ro-bind"), sh(&zrt.join("flatpak-info")), s("/.flatpak-info")]);
    a.extend(v(&["--info-fd", "3", "--lock-file", "/.flatpak-info"]));
    if let Some(t) = st("env").as_table() {
        for (k, val) in t {
            env.set(k, val.as_str().unwrap_or("").to_string());
        }
    }
    for (k, val) in env.0 {
        a.extend([s("--setenv"), k, val]);
    }
    // zone-init: the Rust helper bound into the zone, or the host's Python
    if host.helper == "rust" {
        let helper = host.zone_helper().ok_or("appzones was installed with the Rust helper, but there is no \
            appzones-helper that implements zone-init (`appzones-helper --commands`); reinstall it")?;
        a.extend([s("--ro-bind"), rp(&helper), s(ZONE_HELPER), s("--"), s(ZONE_HELPER), s("zone-init")]);
    } else {
        a.extend([s("--"), rp(&host.python), sh(&host.here.join("zone_init.py"))]);
    }
    Ok(a)
}

/// server.install_error.
fn install_error(host: &Host, layout: &str) -> Option<String> {
    if ["zone-init", "open-link"].iter().all(|c| host.zone_helper_has(c)) {
        return None;
    }
    if host.helper == "rust" && host.zone_helper().is_none() {
        return Some(s("appzones was installed with the Rust helper, but there is no appzones-helper that implements \
                       zone-init (`appzones-helper --commands`); reinstall it"));
    }
    let seen = if layout == "nixos" { "/nix/store" } else { "/usr" };
    if rel_to(&rp(&sh(&host.here)), seen) {
        return None;
    }
    Some(format!("appzones runs from {}, which a zone cannot see (it sees {seen}): install it under {seen} \
                  (`sudo make install`, PREFIX=/usr/local by default; or the Nix package)", sh(&host.here)))
}

fn write(p: &Path, text: &str) -> Result<(), String> {
    std::fs::write(p, text).map_err(|e| format!("{}: {e}", sh(p)))
}

fn random_hex(n: usize) -> String {
    let mut buf = vec![0u8; n];
    let _ = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut buf));
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// server.write_colours: "NAME #rrggbb" lines, replaced atomically.
fn write_colours(host: &Host, cfg: &Config) -> Result<(), String> {
    std::fs::create_dir_all(&host.rt).map_err(|e| e.to_string())?;
    let tmp = host.rt.join(format!(".colours.{}", random_hex(4)));
    let text: String = cfg.zones.iter().map(|z| format!("{} {}\n", z.name, z.get("colour").as_str().unwrap_or(""))).collect();
    let r = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .and_then(|_| std::fs::rename(&tmp, host.rt.join("colours")));
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r.map_err(|e| e.to_string())
}

/// make_plan: the configuration checked, the zone's files written, old mounts detached,
/// packages built, and what the zone needs worked out. Err: the message `appzones` dies with.
pub fn make_plan(host: &Host, name: &str) -> Result<J, String> {
    let cfg = config::load(&host.config, &sh(&host.config), host.here.as_os_str().to_owned());
    if !cfg.errors.is_empty() {
        return Err(format!("{} is invalid:\n  {}", sh(&host.config), cfg.errors.join("\n  ")));
    }
    for w in &cfg.warnings {
        warn(&format!("warning: {w}"));
    }
    let z = cfg.zone(name).ok_or_else(|| format!("no zone {} in {}", py_repr(name), sh(&host.config)))?;
    let layout = layout();
    if let Some(e) = install_error(host, layout) {
        return Err(e);
    }
    let policy = config::policy(host.here.as_os_str().to_owned());
    let timeout = config::duration(z.get("start_timeout")).unwrap_or(30.0);
    let zdir = host.state.join(name);
    let zrt = host.rt.join(name);
    std::fs::create_dir_all(&zdir).map_err(|e| format!("{}: {e}", sh(&zdir)))?;
    std::fs::create_dir_all(&host.rt).map_err(|e| format!("{}: {e}", sh(&host.rt)))?;
    std::fs::set_permissions(&host.rt, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    if lexists(&zrt) {
        std::fs::remove_dir_all(&zrt).map_err(|e| format!("{}: {e}", sh(&zrt)))?;
    }
    std::fs::create_dir_all(&zrt).map_err(|e| e.to_string())?;
    mkdir_mode(&zrt.join("ctl"), 0o700).map_err(|e| e.to_string())?;
    if !zdir.join("machine-id").exists() {
        write(&zdir.join("machine-id"), &format!("{}\n", random_hex(16)))?;
    }
    write(&zrt.join("resolv.conf"), &resolv_conf(z))?;
    write(&zrt.join("hosts"), &format!("127.0.0.1 localhost\n::1 localhost\n127.0.0.2 {name}-zone\n"))?;
    let instance = config::env("INSTANCE_FOR_TESTS").filter(|v| !v.is_empty()).unwrap_or_else(|| {
        let mut b = [0u8; 4];
        let _ = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b));
        format!("zone-{name}-{}", u32::from_le_bytes(b) & 0x7fff_ffff)
    });
    write(&zrt.join("flatpak-info"), &format!("[Application]\nname=dev.mirql.zone.{name}\nruntime=runtime/dev.mirql.zones/x86_64/1\n\n\
        [Instance]\ninstance-id={instance}\nsession-bus-proxy=true\nsystem-bus-proxy=false\nsandbox-engine=dev.mirql.zones\n"))?;
    write(&zrt.join("config.sha256"), &format!("{}\n", config::settings_hash(z)))?;
    write_colours(host, &cfg)?;
    if z.get("tmp").as_str() == Some("disk") {
        prepare_tmp(&zdir)?;
    }
    if !prepare_mnt(host, name, &zdir)? {
        return Err(format!("mounts from the last run of {name} are still there (see above); not starting the zone with \
                            them: `appzones umount {name} NAME` or end what holds them"));
    }
    let extra_path = if strs_of(z.get("packages")).is_empty() { vec![] } else { package_paths(z, &zdir) };
    if crate::serve::stopped() {
        return Err(s("\0stopped"));
    }
    let zone_py = vec![host.python.clone(), sh(&host.here.join("zone.py"))];
    let links_setting = z.get("links").as_str().unwrap_or("portal").to_string();
    let target = match links_setting.as_str() {
        "portal" => None,
        "zone" => Some(name.to_string()),
        l => l.strip_prefix("zone:").map(String::from),
    };
    let mut links = J::Null;
    if let Some(target) = &target {
        std::fs::create_dir(zrt.join("ctl/bin")).map_err(|e| e.to_string())?;
        let xo = zrt.join("ctl/bin/xdg-open");
        let open_link = if host.zone_helper_has("open-link") {
            format!("{ZONE_HELPER} open-link")
        } else {
            PY_OPEN_LINK.replace("{python}", &rp(&host.python))
        };
        let real_xdg = if layout == "nixos" { "/run/current-system/sw/bin/xdg-open" } else { "/usr/bin/xdg-open" };
        write(&xo, &XDG_OPEN.replace("{links}", &links_setting).replace("{target}", target)
            .replace("{open_link}", &open_link).replace("{xdg_open}", real_xdg))?;
        // the zone's xdg-open shim must be executable; it sits in the zone's own runtime directory (0700)
        std::fs::set_permissions(&xo, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?; // NOSONAR
        let mut cmd = zone_py.clone();
        cmd.extend([s("_link"), target.clone()]);
        links = json!({"socket": sh(&zrt.join("ctl").join("links.sock")), "command": cmd});
    }
    let wl = z.get("wayland").as_str().unwrap_or("zone").to_string();
    let have_wl = environ("WAYLAND_DISPLAY").is_some_and(|d| !d.is_empty());
    let wayland_helper = if have_wl && wl != "none" { host.helper_argv("zone-wayland") } else { None };
    if wl == "none" {
        say(&format!("[zone] {name}: wayland = \"none\": no Wayland socket"));
    } else if have_wl && wayland_helper.is_none() {
        say(&format!("[zone] {name}: zone-wayland not found: the zone starts without a Wayland socket (desktop apps will \
                      not open windows; install appzones with zone-wayland)"));
    } else if wayland_helper.is_none() {
        say(&format!("[zone] no WAYLAND_DISPLAY: zone {name} starts without a Wayland socket"));
    }
    let mut proxy = J::Null;
    let bus = environ("DBUS_SESSION_BUS_ADDRESS").filter(|b| !b.is_empty());
    if bus.is_some() && host.find_tool("xdg-dbus-proxy").is_none() {
        say(&format!("[zone] {name}: xdg-dbus-proxy not found: the zone starts without a session bus (no portals, \
                      notifications or tray; install xdg-dbus-proxy for them)"));
    } else if let Some(bus) = bus {
        let mut p = vec![host.tool("bwrap")?, s("--unshare-all"), s("--share-net"), s("--die-with-parent")];
        p.extend(system_binds(layout));
        p.extend([s("--bind"), sh(&host.host_rt), sh(&host.host_rt), s("--ro-bind"), sh(&zrt.join("flatpak-info")),
                  s("/.flatpak-info"), s("--"), host.tool("xdg-dbus-proxy")?, bus, sh(&zrt.join("bus")), s("--filter")]);
        p.extend(dbus_rules(z));
        proxy = json!(p);
    }
    let network = z.get("network").as_str().unwrap_or("internet").to_string();
    write(&zrt.join("nft.rules"), &nft_rules(z)?)?;
    let fp = host.host_rt.join(".flatpak").join(&instance);
    std::fs::create_dir_all(&fp).map_err(|e| e.to_string())?;
    let inner = bwrap_argv(host, &policy, z, &zdir, &zrt, &network, &extra_path)?;
    let mut argv = zone_py.clone();
    argv.extend([s("_netsetup"), if network == "internet" { sh(&zrt.join("nft.rules")) } else { s("-") },
                 sh(&fp.join("bwrapinfo.json")), name.to_string(), s("--")]);
    argv.extend(inner);
    if network == "internet" {
        let pasta = host.tool("pasta")?;
        if let Some(p) = selinux_problem(&pasta) {
            say(&format!("[zone] {name}: {p}"));
        }
        let mut full = vec![pasta];
        full.extend(v(&["--config-net", "--no-map-gw", "-t", "none", "-u", "none", "-T", "none", "-U", "none",
                        "--dns", "none", "-q", "-f", "--"]));
        full.extend(argv);
        argv = full;
    }
    let autostart: Vec<J> = strs_of(z.get("autostart_apps")).iter().map(|a| {
        let argv = config::resolve_app(&cfg, name, a).ok().flatten().unwrap_or_else(|| vec![a.clone()]);
        json!({"app": a, "argv": argv, "bus": config::app_bus(&cfg, name, Some(a))})
    }).collect();
    let mut cleanup = zone_py.clone();
    cleanup.extend([s("_cleanup"), name.to_string()]);
    let upstreams = if network != "internet" {
        J::Null
    } else if z.get("dns").as_str() == Some("public") {
        json!(["1.1.1.1", "1.0.0.1"])
    } else {
        json!([host_resolver()])
    };
    Ok(json!({
        "version": 1,
        "name": name,
        "zrt": sh(&zrt),
        "start_timeout": timeout,
        "linger": config::duration(z.get("linger")),
        "linger_text": tj(z.get("linger")),
        "network": network,
        "dns": tj(z.get("dns")),
        "devices": strs_of(z.get("devices")),
        "flatpak_dir": sh(&fp),
        "wayland": {"mode": wl, "helper": wayland_helper.map(|mut h| {
            h.extend([sh(&zrt.join("wayland-0")), format!("dev.mirql.zone.{name}"), instance.clone()]);
            h
        })},
        "dbus_proxy": proxy,
        "argv": argv,
        "log_socket": network == "internet",
        "dns_upstreams": upstreams,
        "links": links,
        "autostart": autostart,
        "cleanup": cleanup,
    }))
}

/// net.pasta_selinux_problem.
fn selinux_problem(pasta: &str) -> Option<String> {
    let enforce = std::fs::read_to_string("/sys/fs/selinux/enforce").ok()?;
    if enforce.trim() != "1" {
        return None;
    }
    let pasta = if pasta.contains('/') { pasta.to_string() } else { which(pasta, None).unwrap_or_else(|| pasta.into()) };
    let real = rp(&pasta);
    let c = std::ffi::CString::new(real.as_bytes()).ok()?;
    let mut buf = vec![0u8; 256];
    // Safety: getxattr into our buffer of the given size.
    let n = unsafe { libc::getxattr(c.as_ptr(), c"security.selinux".as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
    if n < 0 {
        return None;
    }
    let label = String::from_utf8_lossy(&buf[..n as usize]).into_owned();
    if !label.contains(":passt_exec_t:") {
        return None;
    }
    Some(format!("pasta here is {real} ({}): SELinux runs it in passt's domain, which on this distribution may not build \
                  a network namespace, so an internet zone may not start. Fix: `sudo semanage permissive -a passt_t` \
                  (passt and pasta then run without SELinux confinement, denials only logged), or a passt with its own \
                  pasta binary; zones with network = \"none\" or \"host\" are not affected", label.trim_end_matches('\0')))
}

/// Write PLAN as plan.json in its zone's runtime directory (0600, never in the zone).
pub fn write_plan(plan: &J) -> Result<PathBuf, String> {
    let zrt = plan.get("zrt").and_then(J::as_str).ok_or("the plan has no zrt")?;
    let path = Path::new(zrt).join("plan.json");
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600)
        .custom_flags(libc::O_NOFOLLOW).open(&path).map_err(|e| format!("{}: {e}", sh(&path)))?;
    let text = serde_json::to_string_pretty(plan).map_err(|e| e.to_string())?;
    f.write_all(format!("{text}\n").as_bytes()).map_err(|e| e.to_string())?;
    Ok(path)
}

/// `appzones-helper plan ZONE --python EXE --code-dir DIR`: make and write the plan (as
/// `appzones serve`'s first part), print its path.
pub fn plan_main(args: &[OsString]) -> u8 {
    let (mut name, mut python, mut code) = (None, None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.to_str() {
            Some("--python") => python = it.next().map(|v| v.to_string_lossy().into_owned()),
            Some("--code-dir") => code = it.next().map(|v| PathBuf::from(v.clone())),
            Some(x) if !x.starts_with("--") && name.is_none() => name = Some(x.to_string()),
            _ => {
                eprintln!("usage: appzones-helper plan ZONE --python EXE --code-dir DIR");
                return 2;
            }
        }
    }
    let (Some(name), Some(python), Some(code)) = (name, python, code) else {
        eprintln!("usage: appzones-helper plan ZONE --python EXE --code-dir DIR");
        return 2;
    };
    let host = Host::new(code, python);
    match make_plan(&host, &name).and_then(|p| write_plan(&p)) {
        Ok(path) => {
            println!("{}", sh(&path));
            0
        }
        Err(e) => {
            eprintln!("appzones: {e}");
            1
        }
    }
}

/// A TOML value as JSON.
fn tj(v: &Value) -> J {
    match v {
        Value::String(x) => J::String(x.clone()),
        Value::Integer(i) => json!(i),
        Value::Float(f) => json!(f),
        Value::Boolean(b) => J::Bool(*b),
        Value::Datetime(d) => J::String(d.to_string()),
        Value::Array(a) => J::Array(a.iter().map(tj).collect()),
        Value::Table(t) => J::Object(t.iter().map(|(k, x)| (k.clone(), tj(x))).collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(t: &str) -> Net {
        let (a, p) = t.split_once('/').unwrap();
        Net::of(a.parse().unwrap(), p.parse().unwrap())
    }

    #[test]
    fn collapsing() {
        let c = |x: &[&str]| collapse(&x.iter().map(|t| net(t)).collect::<Vec<_>>()).iter().map(Net::show).collect::<Vec<_>>();
        assert_eq!(c(&["10.0.0.0/24", "10.0.1.0/24"]), vec!["10.0.0.0/23"]);
        assert_eq!(c(&["10.0.0.0/24", "10.0.0.128/25"]), vec!["10.0.0.0/24"]);
        assert_eq!(c(&["10.0.1.0/24", "10.0.2.0/24"]), vec!["10.0.1.0/24", "10.0.2.0/24"]);
        assert_eq!(c(&["203.0.113.7/24"]), vec!["203.0.113.0/24"]);
        assert_eq!(c(&["2001:db8:1::7/64"]), vec!["2001:db8:1::/64"]);
        assert_eq!(c(&["0.0.0.0/0", "10.0.0.0/8"]), vec!["0.0.0.0/0"]);
        assert_eq!(c(&["::/0"]), vec!["::/0"]);
    }

    #[test]
    fn rels() {
        assert!(rel_to("/a/b", "/a"));
        assert!(rel_to("/a", "/a"));
        assert!(!rel_to("/ab", "/a"));
        assert!(rel_to("/x", "/"));
    }
}
