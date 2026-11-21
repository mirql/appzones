//! zones.toml, loaded and checked: the port of src/zonecfg.py (load, load_text, validate,
//! KEYS and their checks, RELEASE and SINCE, the D-Bus and environment never-lists), with
//! the same errors and warnings in the same order, the same settings and origins, and
//! settings_hash byte for byte. The path policy (binds, the never-list) is policy.rs.
//! Kept equal to Python by the differential test (DifferentialConfigTest).

use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;

use toml::{Table, Value};

use crate::policy::{py_path, py_repr, realpath, zone_name_ok, Policy, RESERVED};
use crate::pyfmt::{close_match, fnmatchcase, json_dumps, repr, repr_strs, sha256_hex};

pub const RELEASE: i64 = 6;
pub const NAME_PATTERN: &str = "^[a-z][a-z0-9-]{1,20}$";
const TOP: &[&str] = &["version", "requires", "defaults", "zones", "apps"];
pub const APP_KEYS: &[&str] = &["command", "desktop", "zones", "args", "bus"];
const DEVICES: &[&str] = &["gpu", "audio", "mic", "camera", "screencast", "usb"];
pub const BUSES: &[&str] = &["host", "zone", "none"];
/// (setting, the release that added it), in zonecfg.SINCE's order.
const SINCE: &[(&str, i64)] = &[("requires", 2), ("bus", 2), ("apps.bus", 2), ("bus = \"zone\"", 2),
    ("bus = \"none\"", 2), ("nix_ld", 3), ("tmp", 3), ("usb", 4), ("supervisor", 5), ("start_timeout", 5),
    ("linger", 5), ("wayland", 6)];
const SERVER_ONLY: &[&str] = &["supervisor", "start_timeout"];

const DBUS_NEVER: &[&str] = &["org.freedesktop.systemd1*", "org.freedesktop.secrets*", "org.kde.kwalletd*",
    "org.freedesktop.Flatpak*", "org.gnome.keyring*", "org.freedesktop.impl.portal.*", "ca.desrt.dconf*",
    "org.kde.kdeconnect*", "org.freedesktop.login1*", "org.freedesktop.portal.Flatpak*", "org.kde.klauncher*",
    "org.kde.KWin*", "org.gtk.vfs*", "org.freedesktop.thumbnails*", "org.xfce.Xfconf*",
    "org.freedesktop.FileManager1*", "org.xfce.FileManager*", "org.xfce.Thunar*", "org.xfce.SessionManager*",
    "org.gnome.SessionManager*", "org.gnome.Shell*", "org.kde.ksmserver*", "org.kde.plasmashell*",
    // the portal itself (zones get it filtered, per method; a talk would beat the filters),
    // other zones' names, and host-side control
    "org.freedesktop.portal.Desktop", "dev.mirql.zone*", "org.mozilla.*_zone_*", "org.gnome.Mutter*",
    "org.kde.kglobalaccel*"];
const OWN_NEVER: &[&str] = &["org.freedesktop.*", "org.kde.*", "org.gnome.*", "org.xfce.*", "org.a11y.*",
    "com.canonical.*", "org.mozilla.firefox*", "org.mozilla.thunderbird*", "org.mozilla.librewolf*",
    "dev.mirql.zone*", "org.mozilla.*_zone_*"];
const ENV_NEVER: &[&str] = &["SSH_AUTH_SOCK", "GPG_*", "HYPRLAND_INSTANCE_SIGNATURE", "OPENCODE*", "SOPS_*",
    "*TOKEN*", "*KEY*", "*SECRET*", "*PASSWORD*", "DBUS_SESSION_BUS_ADDRESS", "WAYLAND_DISPLAY",
    "XDG_RUNTIME_DIR", "LD_PRELOAD", "LD_LIBRARY_PATH", "DISPLAY", "XAUTHORITY",
    // the loader (all of it), glibc's own hooks, appzones' own and the nix-ld settings
    "LD_*", "GLIBC_TUNABLES", "GCONV_PATH", "ZONE_*", "NIX_LD*", "HYPRLAND_*"];

/// $APPZONES_<name>, or the pre-0.6 $ZONES_<name> (zonecfg.env).
pub fn env(name: &str) -> Option<String> {
    std::env::var_os(format!("APPZONES_{name}"))
        .or_else(|| std::env::var_os(format!("ZONES_{name}")))
        .map(|v| v.to_string_lossy().into_owned())
}

fn has_env(name: &str) -> bool {
    std::env::var_os(format!("APPZONES_{name}")).is_some() || std::env::var_os(format!("ZONES_{name}")).is_some()
}

/// The user's home as zonecfg.HOME: $APPZONES_HOME_FOR_TESTS, else $HOME, else passwd.
pub fn home() -> OsString {
    if let Some(h) = env("HOME_FOR_TESTS").filter(|h| !h.is_empty()) {
        return OsString::from_vec(py_path(h.as_bytes()));
    }
    if let Some(h) = std::env::var_os("HOME") {
        return OsString::from_vec(py_path(h.as_bytes()));
    }
    // Safety: getpwuid with our uid; the strings are copied before the next call.
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if !pw.is_null() && !(*pw).pw_dir.is_null() {
            return OsString::from_vec(std::ffi::CStr::from_ptr((*pw).pw_dir).to_bytes().to_vec());
        }
    }
    OsString::from("/")
}
/// The real home (Path.home(): $HOME, else the passwd entry), for what the zone binds.
pub fn home_real() -> Vec<u8> {
    if let Some(h) = std::env::var_os("HOME") {
        return py_path(h.as_bytes());
    }
    // Safety: getpwuid with our uid; the string is copied before the next call.
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if !pw.is_null() && !(*pw).pw_dir.is_null() {
            return std::ffi::CStr::from_ptr((*pw).pw_dir).to_bytes().to_vec();
        }
    }
    b"/".to_vec()
}

/// The never-list's context for this user: the home, appzones' code, the host's file
/// ($APPZONES_DENIED_PATHS overrides it), a custom $XDG_STATE_HOME.
pub fn policy(code_dir: OsString) -> Policy {
    let state = std::env::var_os("XDG_STATE_HOME").filter(|s| s.as_bytes().starts_with(b"/"));
    let extra = ["CONFIG", "STATE"].iter().filter_map(|k| env(k)).filter(|v| v.starts_with('/')).map(OsString::from).collect();
    Policy { home: home(), code_dir, denied_file: env("DENIED_PATHS").map(OsString::from), state_dir: state, extra }
}

fn host_layout() -> &'static str {
    match env("LAYOUT").as_deref() {
        Some("nixos") => "nixos",
        Some("fhs") => "fhs",
        _ if Path::new("/run/current-system").exists() => "nixos",
        _ => "fhs",
    }
}

fn nix_ld_paths() -> (String, String) {
    let shim = env("NIX_LD_SHIM").filter(|s| !s.is_empty()).unwrap_or_else(|| "/lib64/ld-linux-x86-64.so.2".into());
    let lib = env("NIX_LD_LIB").filter(|s| !s.is_empty()).unwrap_or_else(|| "/run/current-system/sw/share/nix-ld/lib".into());
    (shim, lib)
}

/// zonecfg.host_nix_ld: (the shim's real path, the lib dir) when the host runs nix-ld.
pub fn host_nix_ld() -> Option<(String, String)> {
    let (shim, lib) = nix_ld_paths();
    let real = String::from_utf8_lossy(&realpath(shim.as_bytes())?).into_owned();
    (Path::new(&real).exists() && real.contains("nix-ld") && Path::new(&lib).join("ld.so").exists()).then_some((real, lib))
}

fn dbus_denied(name: &str) -> bool {
    if name.is_empty() || name == "*" || (name.ends_with(".*") && name.matches('.').count() < 2) {
        return true; // talk=* or org.* is far too wide
    }
    let trimmed = format!("{}*", name.trim_end_matches('*'));
    DBUS_NEVER.iter().any(|p| fnmatchcase(name, p) || fnmatchcase(p.trim_end_matches('*'), &trimmed))
}

fn own_denied(name: &str) -> bool {
    OWN_NEVER.iter().any(|p| fnmatchcase(name, p) || format!("{name}.x") == p.replace('*', "x"))
}

pub fn env_denied(name: &str) -> bool {
    let up = name.to_uppercase();
    ENV_NEVER.iter().any(|p| fnmatchcase(&up, p))
}

/// Every name a host .service file provides; None when that cannot be known
/// (zonecfg.activatable_names: $APPZONES_DBUS_SERVICE_DIRS, else the session bus).
fn activatable_names() -> Option<Vec<String>> {
    if !has_env("DBUS_SERVICE_DIRS") {
        let out = std::process::Command::new("busctl")
            .args(["--user", "--json=short", "call", "org.freedesktop.DBus", "/org/freedesktop/DBus",
                   "org.freedesktop.DBus", "ListActivatableNames"])
            .stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output().ok()?;
        if !out.status.success() {
            return None;
        }
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
        let list = v.get("data")?.get(0)?.as_array()?;
        return Some(list.iter().filter_map(|n| n.as_str().map(String::from)).collect());
    }
    let mut names = Vec::new();
    for d in env("DBUS_SERVICE_DIRS").unwrap_or_default().split(':').filter(|d| !d.is_empty()) {
        let entries = std::fs::read_dir(d).ok()?;
        let mut files = Vec::new();
        for e in entries {
            let e = e.ok()?;
            if e.file_name().as_bytes().ends_with(b".service") {
                files.push(e.path());
            }
        }
        for f in files {
            let text = std::fs::read(&f).ok()?;
            for line in crate::policy::split_lines(&String::from_utf8_lossy(&text)) {
                if let Some(n) = line.strip_prefix("Name=") {
                    names.push(crate::policy::py_strip(n).to_string());
                }
            }
        }
    }
    Some(names)
}

/// The settings' context: what every check may need, asked once per load.
struct Ctx {
    policy: Policy,
    activatable: std::cell::OnceCell<Option<Vec<String>>>,
}

impl Ctx {
    fn activatable(&self) -> &Option<Vec<String>> {
        self.activatable.get_or_init(activatable_names)
    }

    fn host_activatable(&self, name: &str) -> bool {
        let Some(names) = self.activatable() else { return true };
        match name.strip_suffix(".*") {
            Some(pre) => names.iter().any(|n| n == pre || n.starts_with(&format!("{pre}."))),
            None => names.iter().any(|n| n == name),
        }
    }
}

fn s(v: &str) -> Value {
    Value::String(v.into())
}

fn strs(v: &[&str]) -> Value {
    Value::Array(v.iter().map(|x| s(x)).collect())
}

/// The keys, in zonecfg.KEYS's order, with their built-in defaults.
pub fn keys() -> Vec<(&'static str, Value)> {
    let mut dbus = Table::new();
    dbus.insert("talk".into(), Value::Array(vec![]));
    dbus.insert("own".into(), Value::Array(vec![]));
    vec![("network", s("internet")), ("netbird", Value::Boolean(false)), ("dns", s("host")), ("links", s("portal")),
         ("keyring", s("basic")), ("devices", strs(&["gpu", "audio"])), ("camera", s("portal")),
         ("usb", Value::Array(vec![])), ("wayland", s("zone")), ("x11", s("none")), ("clipboard", s("both")),
         ("autostart", Value::Boolean(false)), ("autostart_apps", Value::Array(vec![])),
         ("userns", Value::Boolean(true)), ("machine_id", s("zone")), ("theme", Value::Boolean(true)),
         ("packages", Value::Array(vec![])), ("env", Value::Table(Table::new())), ("binds", Value::Array(vec![])),
         ("dbus", Value::Table(dbus)), ("bus", s("host")), ("nix_ld", Value::Boolean(false)), ("tmp", s("ram")),
         ("home", s("")), ("colour", s("")), ("force_host", Value::Boolean(false)), ("supervisor", s("auto")),
         ("start_timeout", s("30s")), ("linger", s(""))]
}

fn is_key(k: &str) -> bool {
    keys().iter().any(|(n, _)| *n == k)
}

fn str_of(v: &Value) -> Option<&str> {
    v.as_str()
}

/// Python's `x in LIST` for a string x.
fn list_has(v: &Value, x: &str) -> bool {
    v.as_array().is_some_and(|a| a.iter().any(|e| e.as_str() == Some(x)))
}

fn strlist(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::Array(a)) if a.iter().all(|x| x.is_str()) => None,
        _ => Some("must be a list of strings".into()),
    }
}

fn enum_check(v: &Value, vals: &[&str]) -> Option<String> {
    if v.as_str().is_some_and(|x| vals.contains(&x)) {
        None
    } else {
        Some(format!("must be one of {}", vals.iter().map(|x| py_repr(x)).collect::<Vec<_>>().join(", ")))
    }
}

/// zonecfg.duration: seconds of "30s", "10m", "2h" or a whole number; None otherwise.
pub fn duration(v: &Value) -> Option<f64> {
    let n: f64 = match v {
        Value::Integer(i) => *i as f64,
        Value::String(t) => {
            let unit = t.chars().last()?;
            let mult = match unit {
                's' => 1.0,
                'm' => 60.0,
                'h' => 3600.0,
                _ => return None,
            };
            let digits = &t[..t.len() - 1];
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let d = digits.trim_start_matches('0');
            if d.is_empty() {
                0.0
            } else if d.len() > 15 {
                f64::INFINITY
            } else {
                d.parse::<f64>().ok()? * mult
            }
        }
        _ => return None,
    };
    if n <= 0.0 {
        return None;
    }
    Some(if n < 1e9 { n } else { f64::INFINITY })
}

fn duration_check(v: &Value, lo: i64, hi: i64, empty: bool) -> Option<String> {
    if empty && v.as_str() == Some("") {
        return None;
    }
    let Some(d) = duration(v) else {
        return Some(format!("must be a duration such as \"30s\", \"10m\" or \"1h\"{}", if empty { " or \"\"" } else { "" }));
    };
    if !(lo as f64 <= d && d <= hi as f64) {
        return Some(if hi >= 3600 {
            format!("must be between {lo} s and {} h", hi / 3600)
        } else {
            format!("must be between {lo} and {hi} s")
        });
    }
    None
}

/// Python's re.match of ^PATTERN$ allows one trailing newline.
fn match_dollar(v: &str, ok: impl Fn(&str) -> bool) -> bool {
    ok(v) // (Python's re.match(...$) also took a trailing newline; never meant)
}

fn usb_id_ok(x: &str) -> bool {
    let hex = |s: &str| s.len() == 4 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    match x.split_once(':') {
        Some((v, p)) => hex(v) && hex(p),
        None => hex(x),
    }
}

fn colour_ok(c: &str) -> bool {
    c.len() == 7 && c.starts_with('#') && c[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

impl Ctx {
    /// The check of key K (zonecfg.KEYS[k][1]).
    fn check(&self, k: &str, v: &Value) -> Option<String> {
        match k {
            "network" => enum_check(v, &["none", "internet", "host"]),
            "netbird" | "autostart" | "userns" | "theme" | "nix_ld" | "force_host" => {
                (!v.is_bool()).then(|| "must be true or false".to_string())
            }
            "dns" => enum_check(v, &["host", "public"]),
            "links" => {
                let ok = matches!(v.as_str(), Some("portal") | Some("zone"))
                    || v.as_str().and_then(|x| x.strip_prefix("zone:")).is_some_and(zone_name_ok);
                (!ok).then(|| "must be 'portal' (host default browser), 'zone' (this zone's browser) or 'zone:NAME'".into())
            }
            "keyring" => enum_check(v, &["basic", "zone"]),
            "devices" => strlist(Some(v)).or_else(|| {
                let bad: Vec<&str> = v.as_array()?.iter().filter_map(str_of).filter(|d| !DEVICES.contains(d)).collect();
                (!bad.is_empty()).then(|| format!("unknown device(s) {}; known: {}", repr_strs(&bad), DEVICES.join(", ")))
            }),
            "camera" => enum_check(v, &["portal", "device"]),
            "usb" => strlist(Some(v)).or_else(|| {
                let bad: Vec<&str> = v.as_array()?.iter().filter_map(str_of)
                    .filter(|x| !match_dollar(x, usb_id_ok)).collect();
                (!bad.is_empty()).then(|| format!("entries must be \"vvvv\" or \"vvvv:pppp\" (lowercase hex, as lsusb \
                                                  prints them): {}", repr_strs(&bad)))
            }),
            "wayland" => enum_check(v, &["zone", "none", "host"]),
            "x11" => enum_check(v, &["none", "own"]),
            "clipboard" => enum_check(v, &["both"]),
            "autostart_apps" => strlist(Some(v)),
            "packages" => strlist(Some(v)).or_else(|| {
                // nixpkgs attribute paths: they go into a Nix expression and a path
                let ok = |a: &str| a.split('.').all(|p| !p.is_empty() && p.bytes().next().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
                    && p.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-'".contains(&b)));
                let bad: Vec<&str> = v.as_array()?.iter().filter_map(str_of).filter(|a| !ok(a)).collect();
                (!bad.is_empty()).then(|| format!("not nixpkgs attribute names: {}", repr_strs(&bad)))
            }),
            "machine_id" => enum_check(v, &["zone", "host"]),
            "env" => match v {
                Value::Table(t) if t.values().all(Value::is_str) => {
                    let name_ok = |k: &str| k.bytes().next().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
                        && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
                    let odd: Vec<&str> = t.keys().map(String::as_str).filter(|k| !name_ok(k)).collect();
                    let bad: Vec<&str> = t.keys().map(String::as_str).filter(|k| env_denied(k)).collect();
                    let nul: Vec<&str> = t.iter().filter(|(_, v)| v.as_str().is_some_and(|s| s.contains('\0')
                        || s == crate::plan::PIN)).map(|(k, _)| k.as_str()).collect();
                    if !odd.is_empty() {
                        Some(format!("names are letters, digits and _, not starting with a digit: {}", repr_strs(&odd)))
                    } else if !nul.is_empty() {
                        Some(format!("values may not hold a NUL byte or be {}: {}", crate::plan::PIN, repr_strs(&nul)))
                    } else {
                        (!bad.is_empty()).then(|| format!("never passed into a zone: {}", repr_strs(&bad)))
                    }
                }
                _ => Some("must be a table of strings".into()),
            },
            "binds" => {
                let Value::Array(a) = v else { return Some("must be a list of { path, mode }".into()) };
                for b in a {
                    let Value::Table(t) = b else { return Some(format!("bind {}: keys are path and mode", repr(b))) };
                    if t.keys().any(|k| k != "path" && k != "mode") || !t.contains_key("path") {
                        return Some(format!("bind {}: keys are path and mode", repr(b)));
                    }
                    let Some(path) = t["path"].as_str().filter(|p| !p.is_empty()) else {
                        return Some(format!("bind {}: path must be a non-empty string", repr(b)));
                    };
                    if !t.get("mode").is_none_or(|m| matches!(m.as_str(), Some("rw") | Some("ro"))) {
                        return Some(format!("bind {path}: mode must be 'rw' or 'ro'"));
                    }
                }
                None
            }
            "dbus" => self.check_dbus(v),
            "bus" => enum_check(v, BUSES),
            "tmp" => enum_check(v, &["ram", "disk"]),
            "home" | "colour" => (!v.is_str()).then(|| "must be a string".to_string()),
            "supervisor" => enum_check(v, &["auto", "systemd", "spawn"]),
            "start_timeout" => duration_check(v, 1, 3600, false),
            "linger" => duration_check(v, 10, 7 * 86400, true),
            _ => None,
        }
    }

    fn check_dbus(&self, v: &Value) -> Option<String> {
        let Value::Table(t) = v else { return Some("must be { talk = [...], own = [...] }".into()) };
        if t.keys().any(|k| k != "talk" && k != "own") {
            return Some("must be { talk = [...], own = [...] }".into());
        }
        let empty = Value::Array(vec![]);
        for k in ["talk", "own"] {
            let list = t.get(k).unwrap_or(&empty);
            if let Some(e) = strlist(Some(list)) {
                return Some(format!("dbus.{k} {e}"));
            }
            let names: Vec<&str> = list.as_array().map(|a| a.iter().filter_map(str_of).collect()).unwrap_or_default();
            let bad: Vec<&str> = names.iter().copied().filter(|n| dbus_denied(n) || (k == "own" && own_denied(n))).collect();
            if !bad.is_empty() {
                return Some(format!("dbus.{k}: never granted: {}", repr_strs(&bad)));
            }
            if k == "own" {
                let wide: Vec<&str> = names.iter().copied().filter(|n| n.ends_with(".*") && n.matches('.').count() < 3).collect();
                if !wide.is_empty() {
                    return Some(format!("dbus.own: a wildcard needs at least three labels before .* (org.example.app.*): {}",
                                        repr_strs(&wide)));
                }
            }
            if !names.is_empty() && self.activatable().is_none() {
                return Some(format!("dbus.{k}: cannot ask the session bus which names it can start (ListActivatableNames), \
                                     so cannot check whether a call would start a host program; nothing extra is granted"));
            }
            let act: Vec<&str> = names.iter().copied().filter(|n| self.host_activatable(n)).collect();
            if !act.is_empty() {
                return Some(format!("dbus.{k}: {} can be started on the host bus by a call from the zone (a host .service \
                                     file provides it); a single-instance app that needs it: bus = \"zone\" in its [apps] entry",
                                    repr_strs(&act)));
            }
        }
        None
    }
}

pub struct Zone {
    pub name: String,
    /// Every key, in KEYS's order.
    pub settings: Table,
    /// key -> "builtin" | "defaults" | "zone"
    pub origin: Vec<(&'static str, &'static str)>,
}

impl Zone {
    pub fn get(&self, k: &str) -> &Value {
        &self.settings[k]
    }

    fn origin(&self, k: &str) -> &str {
        self.origin.iter().find(|(n, _)| *n == k).map_or("builtin", |(_, o)| *o)
    }
}

pub struct Config {
    pub zones: Vec<Zone>,
    pub apps: Table,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl Config {
    pub fn zone(&self, name: &str) -> Option<&Zone> {
        self.zones.iter().find(|z| z.name == name)
    }
}

fn unknown_key<'a>(where_: &str, k: &str, known: impl IntoIterator<Item = &'a str>) -> String {
    let near = close_match(k, known, 0.75);
    format!("{where_}unknown key {}{}", py_repr(k), near.map_or(String::new(), |n| format!(" (did you mean {}?)", py_repr(n))))
}

/// Python's `x == 1` for a TOML value (1, 1.0 and true all are).
fn equals_one(v: Option<&Value>) -> bool {
    matches!(v, Some(Value::Integer(1)) | Some(Value::Boolean(true))) || matches!(v, Some(Value::Float(f)) if *f == 1.0)
}

/// C's strerror, as Python's OSError shows it.
fn strerror(n: i32) -> String {
    // Safety: strerror returns a NUL-terminated static string for any number.
    unsafe { std::ffi::CStr::from_ptr(libc::strerror(n)).to_string_lossy().into_owned() }
}

/// tomllib's form of a parse error: "MESSAGE (at line L, column C)".
fn parse_error(text: &str, e: &toml::de::Error) -> String {
    let msg = e.message().trim().trim_end_matches('.');
    let Some(span) = e.span() else { return msg.to_string() };
    let before = &text[..span.start.min(text.len())];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    format!("{msg} (at line {line}, column {col})")
}

/// zonecfg.load: the file at PATH (SHOWN names it in messages).
pub fn load(path: &Path, shown: &str, code_dir: OsString) -> Config {
    match std::fs::read(path) {
        Err(e) => {
            let msg = match e.raw_os_error() {
                Some(n) => format!("[Errno {n}] {}: {}", strerror(n), py_repr(shown)),
                None => e.to_string(),
            };
            Config { zones: vec![], apps: Table::new(), errors: vec![format!("{shown}: {msg}")], warnings: vec![] }
        }
        Ok(b) => match String::from_utf8(b) {
            Ok(text) => load_text(&text, shown, code_dir),
            Err(e) => Config { zones: vec![], apps: Table::new(), errors: vec![format!(
                "{shown}: 'utf-8' codec can't decode byte 0x{:02x} in position {}: invalid start byte",
                e.as_bytes()[e.utf8_error().valid_up_to()], e.utf8_error().valid_up_to())], warnings: vec![] },
        },
    }
}

/// zonecfg.load_text.
pub fn load_text(text: &str, path: &str, code_dir: OsString) -> Config {
    let ctx = Ctx { policy: policy(code_dir), activatable: std::cell::OnceCell::new() };
    let mut errors: Vec<String> = Vec::new();
    let raw: Table = match text.parse() {
        Ok(t) => t,
        Err(e) => {
            return Config { zones: vec![], apps: Table::new(), errors: vec![format!("{path}: {}", parse_error(text, &e))],
                            warnings: vec![] };
        }
    };
    for k in raw.keys() {
        if !TOP.contains(&k.as_str()) {
            errors.push(unknown_key("top level: ", k, TOP.iter().copied()));
        }
    }
    let shapes: Vec<String> = ["defaults", "zones", "apps"].iter()
        .filter(|k| raw.get(**k).is_some_and(|v| !v.is_table())).map(|k| format!("{k} must be a table")).collect();
    if !shapes.is_empty() {
        errors.extend(shapes);
        return Config { zones: vec![], apps: Table::new(), errors, warnings: vec![] };
    }
    if !equals_one(raw.get("version")) {
        errors.push("version must be 1".into());
    }
    let empty = Table::new();
    let defaults = raw.get("defaults").and_then(Value::as_table).unwrap_or(&empty);
    let keys = keys();
    for k in defaults.keys() {
        if !is_key(k) {
            errors.push(unknown_key("[defaults] ", k, keys.iter().map(|(n, _)| *n)));
        }
    }
    let mut zones: Vec<Zone> = Vec::new();
    for (zname, zraw) in raw.get("zones").and_then(Value::as_table).unwrap_or(&empty) {
        if !zone_name_ok(zname) {
            errors.push(format!("zone name {} must match {NAME_PATTERN}", py_repr(zname)));
        } else if RESERVED.contains(&zname.as_str()) {
            errors.push(format!("zone name {} is reserved (bin, etc, launcher, lib, share)", py_repr(zname)));
        }
        let Value::Table(zraw) = zraw else {
            errors.push(format!("[zones.{zname}] must be a table"));
            continue;
        };
        let mut settings = Table::new();
        let mut origin = Vec::new();
        for (k, dflt) in &keys {
            let (v, o) = if let Some(v) = zraw.get(*k) {
                (v.clone(), "zone")
            } else if let Some(v) = defaults.get(*k) {
                (v.clone(), "defaults")
            } else {
                (dflt.clone(), "builtin")
            };
            settings.insert((*k).into(), v);
            origin.push((*k, o));
        }
        for k in zraw.keys() {
            if !is_key(k) {
                errors.push(unknown_key(&format!("[zones.{zname}] "), k, keys.iter().map(|(n, _)| *n)));
            }
        }
        // a name seen twice (TOML refuses that already): the last one, as a dict would
        zones.retain(|z| &z.name != zname);
        zones.push(Zone { name: zname.clone(), settings, origin });
    }
    let apps = raw.get("apps").and_then(Value::as_table).cloned().unwrap_or_default();
    errors.extend(validate(&ctx, &zones, &apps));
    let req = raw.get("requires").cloned().unwrap_or(Value::Integer(1));
    let req_int = match &req {
        Value::Integer(i) => Some(*i),
        Value::Boolean(b) => Some(i64::from(*b)), // Python's bool is an int
        _ => None,
    };
    match &req {
        Value::Integer(i) if *i >= 1 => {
            if *i > RELEASE {
                let mut errs = vec![format!("{path} needs zones release {i}, and this `appzones` is release {RELEASE}: \
                                             update appzones (and rebuild) to get the newer one. Until then, what this \
                                             zone does not know:")];
                errs.extend(errors.iter().map(|e| format!("  {e}")));
                return Config { zones, apps, errors: errs, warnings: vec![] };
            }
        }
        _ => errors.push("requires must be a release number (an integer, 1 or more)".into()),
    }
    let used: Vec<(&str, i64)> = SINCE.iter().copied()
        .filter(|(n, since)| req_int.is_some_and(|r| *since > r) && uses(&raw, n)).collect();
    if !used.is_empty() && errors.is_empty() {
        let max = used.iter().map(|(_, s)| *s).max().unwrap_or(1);
        errors.push(format!("requires = {}, but the file uses {} (zones release {max}): set requires = {max}, so an \
                             older `appzones` says to update instead of failing on an unknown key",
                            req_int.unwrap_or(1), used.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")));
    }
    let mut warnings: Vec<String> = zones.iter().filter(|z| matches!(z.get("userns"), Value::Boolean(false)))
        .map(|z| format!("[zones.{}] userns = false: Chromium/Electron refuse to start, Firefox runs with a weaker \
                          sandbox without saying so", z.name)).collect();
    for z in &zones {
        let n = &z.name;
        let is = |k: &str, v: &str| z.get(k).as_str() == Some(v);
        if is("keyring", "zone") {
            warnings.push(format!("[zones.{n}] keyring = \"zone\" is not built yet; the zone runs with \"basic\" for now"));
        }
        if is("x11", "own") {
            warnings.push(format!("[zones.{n}] x11 = \"own\" is not built yet; the zone has no X11 for now"));
        }
        if is("network", "host") {
            warnings.push(format!("[zones.{n}] network = \"host\": the zone reaches everything the host reaches, \
                                   localhost and the tailnet included, and the host's abstract sockets (Xwayland's \
                                   among them: X11 apps of the host can be read and driven)"));
        }
        if is("wayland", "host") {
            warnings.push(format!("[zones.{n}] wayland = \"host\": where the compositor has no security context \
                                   (GNOME, Weston), the zone's apps get the host's Wayland socket: whatever it offers \
                                   any app, and windows not marked as the zone's (elsewhere the zone gets its own context)"));
        }
        if is("machine_id", "host") {
            warnings.push(format!("[zones.{n}] machine_id = \"host\": apps can link this zone to the host and to other \
                                   zones by it"));
        }
    }
    // the host bus is one bus: a name two zones may own is owned by whichever starts first
    let mut grants: Vec<(String, String)> = Vec::new();
    for z in &zones {
        if let Some(Value::Array(own)) = z.get("dbus").as_table().and_then(|t| t.get("own")) {
            grants.extend(own.iter().filter_map(str_of).map(|n| (n.to_string(), z.name.clone())));
        }
    }
    let mut clashes: Vec<(String, Vec<String>)> = Vec::new();
    for (i, (a, za)) in grants.iter().enumerate() {
        for (b, zb) in &grants[i + 1..] {
            if za != zb && names_overlap(a, b) {
                let key = if a == b { a.clone() } else { format!("{a} / {b}") };
                let at = match clashes.iter().position(|(k, _)| *k == key) {
                    Some(p) => p,
                    None => {
                        clashes.push((key, Vec::new()));
                        clashes.len() - 1
                    }
                };
                for zn in [za, zb] {
                    if !clashes[at].1.contains(zn) {
                        clashes[at].1.push(zn.clone());
                    }
                }
            }
        }
    }
    for (n, mut zs) in clashes {
        zs.sort();
        errors.push(format!("dbus.own {} in zones {}: on the shared host bus the first zone to start the app owns it, \
                             and the app in the other zone would hand its work (a URL, a file) to that one; a zone \
                             never hands anything to another zone", py_repr(&n), zs.join(", ")));
    }
    Config { zones, apps, errors, warnings }
}

/// zonecfg.names_overlap: bus names (or "prefix.*" patterns) that can name the same name.
fn names_overlap(a: &str, b: &str) -> bool {
    match (a.strip_suffix(".*"), b.strip_suffix(".*")) {
        (None, None) => a == b,
        (Some(pa), Some(pb)) => pa == pb || pa.starts_with(&format!("{pb}.")) || pb.starts_with(&format!("{pa}.")),
        (Some(p), None) => b == p || b.starts_with(&format!("{p}.")),
        (None, Some(p)) => a == p || a.starts_with(&format!("{p}.")),
    }
}

/// zonecfg.uses: whether the raw TOML uses a setting from SINCE.
fn uses(raw: &Table, what: &str) -> bool {
    let empty = Table::new();
    let mut tables: Vec<&Table> = vec![raw.get("defaults").and_then(Value::as_table).unwrap_or(&empty)];
    tables.extend(raw.get("zones").and_then(Value::as_table).into_iter().flat_map(|z| z.values().filter_map(Value::as_table)));
    let apps: Vec<&Table> = raw.get("apps").and_then(Value::as_table).into_iter()
        .flat_map(|a| a.values().filter_map(Value::as_table)).collect();
    if what == "requires" {
        return raw.contains_key("requires");
    }
    if let Some(k) = what.strip_prefix("apps.") {
        return apps.iter().any(|a| a.contains_key(k));
    }
    if let Some((k, v)) = what.split_once(" = ") {
        let v = v.trim_matches('"');
        return tables.iter().chain(apps.iter()).any(|t| t.get(k).and_then(Value::as_str) == Some(v));
    }
    tables.iter().any(|t| t.contains_key(what))
}

/// zonecfg.validate.
fn validate(ctx: &Ctx, zones: &[Zone], apps: &Table) -> Vec<String> {
    let mut errs = Vec::new();
    let mut seen_colour: Vec<(String, String)> = Vec::new();
    let zone_named = |n: &str| zones.iter().find(|z| z.name == n);
    for z in zones {
        let p = format!("[zones.{}]", z.name);
        for (k, v) in &z.settings {
            if let Some(e) = ctx.check(k, v) {
                errs.push(format!("{p} {k} {e}"));
            }
        }
        match z.get("colour").as_str() {
            Some(c) if match_dollar(c, colour_ok) => {
                let low = c.to_lowercase();
                if let Some((_, other)) = seen_colour.iter().find(|(s, _)| *s == low) {
                    errs.push(format!("{p} colour {c} already used by zone {other}"));
                } else {
                    seen_colour.push((low, z.name.clone()));
                }
            }
            _ => errs.push(format!("{p} colour is required, as #rrggbb")),
        }
        let is = |k: &str, v: &str| z.get(k).as_str() == Some(v);
        let truly = |k: &str| matches!(z.get(k), Value::Boolean(true));
        if truly("netbird") && !is("network", "internet") {
            errs.push(format!("{p} netbird = true needs network = \"internet\""));
        }
        if is("network", "host") && z.name == "untrusted" && !value_truthy(z.get("force_host")) {
            errs.push(format!("{p} network = \"host\" in untrusted needs force_host = true"));
        }
        if is("wayland", "host") && z.name == "untrusted" && !value_truthy(z.get("force_host")) {
            errs.push(format!("{p} wayland = \"host\" in untrusted needs force_host = true"));
        }
        if let Some(t) = z.get("links").as_str().and_then(|l| l.strip_prefix("zone:")) {
            if zone_named(t).is_none() {
                errs.push(format!("{p} links names unknown zone {}", py_repr(t)));
            }
        }
        if truly("nix_ld") && host_layout() == "fhs" {
            errs.push(format!("{p} nix_ld = true is for NixOS; on this distribution generic Linux binaries run without \
                               it: remove it"));
        } else if truly("nix_ld") && host_nix_ld().is_none() {
            let (shim, lib) = nix_ld_paths();
            errs.push(format!("{p} nix_ld = true, but this host has no nix-ld ({shim} is not its shim, or {lib}/ld.so is \
                               missing): programs.nix-ld.enable = true on the host first"));
        }
        let devices = z.get("devices");
        if devices.is_array() && !list_has(devices, "camera") && z.origin("camera") == "zone" {
            errs.push(format!("{p} camera is set but \"camera\" is not in devices"));
        }
        if let (true, Some(usb)) = (devices.is_array(), z.get("usb").as_array()) {
            if !usb.is_empty() && !list_has(devices, "usb") && z.origin("usb") == "zone" {
                errs.push(format!("{p} usb is set but \"usb\" is not in devices"));
            }
            if list_has(devices, "usb") && usb.is_empty() {
                errs.push(format!("{p} \"usb\" is in devices but usb is empty: name the devices, usb = [\"vvvv\"] or \
                                   [\"vvvv:pppp\"] (never all of them)"));
            }
        }
        if let Some(binds) = z.get("binds").as_array() {
            for b in binds {
                if let Some(path) = b.as_table().and_then(|t| t.get("path")).and_then(Value::as_str) {
                    if let Some(e) = ctx.policy.bind_error(path.as_bytes()) {
                        errs.push(format!("{p} bind {e}"));
                    }
                }
            }
        }
        if let Some(home) = z.get("home").as_str().filter(|h| !h.is_empty()) {
            match ctx.policy.bind_error(home.as_bytes()) {
                Some(e) if !e.ends_with(": does not exist") => errs.push(format!("{p} home {e}")),
                Some(_) => {}
                None => {
                    let ex = ctx.policy.expand(home.as_bytes());
                    if !Path::new(std::ffi::OsStr::from_bytes(&ex)).is_dir() {
                        errs.push(format!("{p} home {home}: not a directory"));
                    }
                }
            }
        }
        for a in z.get("autostart_apps").as_array().map(|v| v.as_slice()).unwrap_or(&[]) {
            let Some(a) = a.as_str() else { continue };
            match apps.get(a) {
                None => errs.push(format!("{p} autostart_apps names undeclared app {}", py_repr(a))),
                Some(Value::Table(t)) if t.get("zones").is_none_or(Value::is_array) => {
                    if !t.get("zones").is_some_and(|zs| list_has(zs, &z.name)) {
                        errs.push(format!("{p} autostart_apps: app {} is not declared for this zone", py_repr(a)));
                    }
                }
                Some(_) => {} // reported with the app
            }
        }
    }
    for (aname, a) in apps {
        let p = format!("[apps.{aname}]");
        let Value::Table(a) = a else {
            errs.push(format!("{p} must be a table with command = [...], zones = [...], args = {{ ZONE = [...] }}"));
            continue;
        };
        if !a.get("zones").is_none_or(Value::is_array) || !a.get("args").is_none_or(Value::is_table) {
            errs.push(format!("{p} must be a table with command = [...], zones = [...], args = {{ ZONE = [...] }}"));
            continue;
        }
        if !zone_name_ok(aname) {
            errs.push(format!("{p} app id must match {NAME_PATTERN}"));
        }
        for k in a.keys() {
            if !APP_KEYS.contains(&k.as_str()) {
                errs.push(unknown_key(&format!("{p} "), k, APP_KEYS.iter().copied()));
            }
        }
        let cmd = a.get("command");
        if strlist(cmd).is_some() || !cmd.is_some_and(value_truthy) {
            errs.push(format!("{p} command must be a non-empty list of strings"));
        }
        let empty = Value::Array(vec![]);
        let azones = a.get("zones").unwrap_or(&empty);
        if strlist(Some(azones)).is_some() {
            errs.push(format!("{p} zones must be a list of zone names"));
            continue;
        }
        let znames: Vec<&str> = azones.as_array().map(|v| v.iter().filter_map(str_of).collect()).unwrap_or_default();
        for zn in &znames {
            if zone_named(zn).is_none() {
                errs.push(format!("{p} zones names unknown zone {}", py_repr(zn)));
            }
        }
        if a.get("desktop").is_some_and(|d| !d.is_str()) {
            errs.push(format!("{p} desktop must be a string"));
        }
        if let Some(args) = a.get("args").and_then(Value::as_table) {
            for (zn, args) in args {
                if !znames.contains(&zn.as_str()) {
                    errs.push(format!("{p} args for {}, which is not in its zones", py_repr(zn)));
                }
                if strlist(Some(args)).is_some() {
                    errs.push(format!("{p} args.{zn} must be a list of strings"));
                }
            }
        }
        if a.get("bus").is_some_and(|b| !b.as_str().is_some_and(|b| BUSES.contains(&b))) {
            errs.push(format!("{p} bus must be one of 'host', 'zone', 'none'"));
        }
        if a.get("bus").and_then(Value::as_str) == Some("none") {
            for zn in &znames {
                if zone_named(zn).is_some_and(|z| matches!(z.get("userns"), Value::Boolean(false))) {
                    errs.push(format!("{p} bus = \"none\" needs userns = true in zone {} (the app gets its own mount \
                                       namespace without the bus socket)", py_repr(zn)));
                }
            }
        }
    }
    for z in zones {
        if z.get("bus").as_str() == Some("none") && matches!(z.get("userns"), Value::Boolean(false)) {
            errs.push(format!("[zones.{}] bus = \"none\" needs userns = true", z.name));
        }
    }
    errs
}

/// Python's truth value of a TOML value.
pub fn truthy(v: &Value) -> bool {
    value_truthy(v)
}

fn value_truthy(v: &Value) -> bool {
    match v {
        Value::String(s) => !s.is_empty(),
        Value::Integer(i) => *i != 0,
        Value::Float(f) => *f != 0.0,
        Value::Boolean(b) => *b,
        Value::Datetime(_) => true,
        Value::Array(a) => !a.is_empty(),
        Value::Table(t) => !t.is_empty(),
    }
}

/// common.settings_hash: what a running zone was built from, byte for byte as Python's.
pub fn settings_hash(z: &Zone) -> String {
    let mut t = Table::new();
    for (k, v) in &z.settings {
        if SERVER_ONLY.contains(&k.as_str()) || (k == "linger" && v.as_str() == Some(""))
            || (k == "wayland" && v.as_str() == Some("zone")) {
            continue;
        }
        t.insert(k.clone(), v.clone());
    }
    let mut out = String::new();
    json_dumps(&Value::Table(t), &mut out);
    sha256_hex(out.as_bytes())
}

/// zonecfg.resolve_app: the argv of app APP in ZONE; Ok(None) when APP is not declared.
pub fn resolve_app(cfg: &Config, zone: &str, app: &str) -> Result<Option<Vec<String>>, String> {
    let Some(a) = cfg.apps.get(app).and_then(Value::as_table) else { return Ok(None) };
    let zones: Vec<&str> = a.get("zones").and_then(Value::as_array).map(|v| v.iter().filter_map(str_of).collect())
        .unwrap_or_default();
    if !zones.contains(&zone) {
        return Err(format!("app {} is not declared for zone {} (its zones: {})", py_repr(app), py_repr(zone), zones.join(", ")));
    }
    let mut argv: Vec<String> = a.get("command").and_then(Value::as_array).map(|v| v.iter().filter_map(str_of)
        .map(String::from).collect()).unwrap_or_default();
    if let Some(extra) = a.get("args").and_then(Value::as_table).and_then(|t| t.get(zone)).and_then(Value::as_array) {
        argv.extend(extra.iter().filter_map(str_of).map(String::from));
    }
    Ok(Some(argv))
}

/// zonecfg.app_bus: the bus an app (or, with None, an undeclared command) gets in ZONE.
pub fn app_bus(cfg: &Config, zone: &str, app: Option<&str>) -> String {
    if let Some(b) = app.and_then(|a| cfg.apps.get(a)).and_then(|a| a.get("bus")).and_then(Value::as_str)
        .filter(|b| !b.is_empty()) {
        return b.to_string();
    }
    cfg.zone(zone).and_then(|z| z.get("bus").as_str()).unwrap_or("host").to_string()
}

/// `appzones-helper config-json FILE [--name SHOWN] [--code-dir DIR]`: the loaded
/// configuration as one JSON object, for the differential test.
pub fn config_json_main(args: &[OsString]) -> u8 {
    let mut it = args.iter();
    let Some(file) = it.next() else {
        eprintln!("usage: appzones-helper config-json FILE [--name SHOWN] [--code-dir DIR]");
        return 2;
    };
    let mut shown = file.to_string_lossy().into_owned();
    let mut code_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.as_os_str().to_owned()))
        .unwrap_or_default();
    while let Some(a) = it.next() {
        let Some(v) = it.next() else { return 2 };
        match a.to_str() {
            Some("--name") => shown = v.to_string_lossy().into_owned(),
            Some("--code-dir") => code_dir = v.clone(),
            _ => return 2,
        }
    }
    let cfg = load(Path::new(file), &shown, code_dir);
    let mut zones = serde_json::Map::new();
    for z in &cfg.zones {
        let mut settings = String::new();
        json_dumps(&Value::Table(z.settings.clone()), &mut settings);
        let origin: serde_json::Map<String, serde_json::Value> =
            z.origin.iter().map(|(k, o)| ((*k).to_string(), serde_json::Value::String((*o).into()))).collect();
        zones.insert(z.name.clone(), serde_json::json!({
            "settings": serde_json::from_str::<serde_json::Value>(&settings).unwrap_or_default(),
            "origin": origin,
            "hash": settings_hash(z),
        }));
    }
    println!("{}", serde_json::json!({"errors": cfg.errors, "warnings": cfg.warnings, "zones": zones}));
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        let d = |t: &str| duration(&Value::String(t.into()));
        assert_eq!(d("30s"), Some(30.0));
        assert_eq!(d("10m"), Some(600.0));
        assert_eq!(d("2h"), Some(7200.0));
        assert_eq!(d("0s"), None);
        assert_eq!(d("1.5m"), None);
        assert_eq!(d(""), None);
        assert_eq!(d("99999999999999999999s"), Some(f64::INFINITY));
        assert_eq!(duration(&Value::Integer(45)), Some(45.0));
        assert_eq!(duration(&Value::Boolean(true)), None);
    }

    #[test]
    fn denied_names() {
        assert!(dbus_denied("org.freedesktop.systemd1"));
        assert!(dbus_denied("org.*"));
        assert!(dbus_denied("*"));
        assert!(!dbus_denied("org.example.App"));
        assert!(own_denied("org.freedesktop.Notifications"));
        assert!(own_denied("org.mozilla.firefox_zone_work"));
        assert!(!own_denied("com.example.app"));
        // the portal itself, other zones' names, host-side control
        for n in ["org.freedesktop.portal.Desktop", "dev.mirql.zone.work", "org.mozilla.firefox_zone_work.x",
                  "org.gnome.Mutter.RemoteDesktop", "org.kde.kglobalaccel"] {
            assert!(dbus_denied(n), "{n}");
        }
        for n in ["LD_AUDIT", "GLIBC_TUNABLES", "GCONV_PATH", "ZONE_DNS", "NIX_LD_LIBRARY_PATH", "HYPRLAND_CMD"] {
            assert!(env_denied(n), "{n}");
        }
        assert!(env_denied("github_token"));
        assert!(env_denied("SSH_AUTH_SOCK"));
        assert!(!env_denied("LANG"));
    }

    #[test]
    fn checked_values() {
        let ctx = Ctx { policy: policy("/nonexistent-code".into()), activatable: std::cell::OnceCell::new() };
        let t = |text: &str| -> Value { format!("x = {text}").parse::<Table>().unwrap()["x"].clone() };
        assert!(ctx.check("env", &t("{ \"A=B\" = \"x\" }")).is_some_and(|e| e.contains("letters, digits")));
        assert!(ctx.check("env", &t("{ \"\" = \"x\" }")).is_some());
        assert!(ctx.check("env", &t("{ A = \"x\\u0000y\" }")).is_some_and(|e| e.contains("NUL")));
        assert!(ctx.check("env", &t("{ A = \"--zones-pin\" }")).is_some());
        assert_eq!(ctx.check("env", &t("{ FOO_1 = \"bar\" }")), None);
        assert!(ctx.check("packages", &t("[\"hello\", \"python3Packages.requests\"]")).is_none());
        assert!(ctx.check("packages", &t("[\"a; b\"]")).is_some_and(|e| e.contains("not nixpkgs attribute names")));
        assert!(ctx.check("packages", &t("[\"../x\"]")).is_some());
        assert!(ctx.check("usb", &t("[\"abcd\\n\"]")).is_some(), "a trailing newline is not a match");
    }

    #[test]
    fn overlaps() {
        assert!(names_overlap("a.b.*", "a.b.c"));
        assert!(names_overlap("a.b.c", "a.b.c"));
        assert!(!names_overlap("a.b.c", "a.b.d"));
        assert!(names_overlap("a.b.*", "a.*"));
        assert!(!names_overlap("a.bc.*", "a.b.*"));
    }
}
