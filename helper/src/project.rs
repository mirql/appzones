//! Project zones: a repository's own zones, in `appzones.toml` at its root (design: the
//! hub's design/project-zones.md; docs/configuration/projects.md).
//!
//! A project file is someone else's input to a sandbox tool, so:
//! - nothing of it is used before `appzones trust`, which shows what it asks for and copies
//!   the file into appzones' state folder (on the never-list: no zone writes there);
//! - zones run from that copy, never from the repo (which a zone with the checkout writable
//!   could rewrite); `@` refuses to start when the repo's file differs from the copy;
//! - a project file may use only a few keys (PROJECT_KEYS); the rest are the host's alone;
//! - a project zone starts with no desktop: no devices, no Wayland, no host D-Bus.

use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use toml::{Table, Value};

use crate::config::{self, Zone};
use crate::policy::{py_repr, zone_name_ok};

/// The project file, at the project's root.
pub const FILE: &str = "appzones.toml";
/// The release a project file needs at least (the one that added project zones).
pub const SINCE_RELEASE: i64 = 8;
/// The most a project file may be.
const MAX_BYTES: u64 = 1 << 20;
/// The keys a project zone may set; every other zone key is the host's alone.
const PROJECT_KEYS: &[&str] = &["network", "allow", "env", "packages", "tmp", "linger", "start_timeout", "binds",
    "checkout"];
/// Colours for project zones (border and bar): one per zone, never a host zone's.
const PALETTE: &[&str] = &["#7f849c", "#a6adc8", "#b4a7d6", "#9fb4c7", "#c9b29b", "#a8c69f", "#c7a0b0", "#a0c4c4",
    "#bcb49a", "#9aa7bc", "#c4a3a3", "#a3c4a3"];

/// A trusted project zone, as `list` and the commands show it.
#[derive(Clone, Debug)]
pub struct ProjectZone {
    /// the zone's name everywhere inside appzones (state, sockets, logs): p + 10 hex
    pub name: String,
    /// `my-app/agent`
    pub shown: String,
    /// the project's root (resolved)
    pub root: PathBuf,
}

/// Escape everything outside printable ASCII (control characters, bidirectional overrides,
/// zero-width characters): what a project file says is shown, never interpreted.
pub fn shown(s: &str) -> String {
    s.chars().map(|c| if (' '..='~').contains(&c) { c.to_string() } else { c.escape_unicode().to_string() }).collect()
}

/// The 10 hex digits naming a project (its root) or a project zone (root and zone).
fn digest(parts: &[&[u8]]) -> String {
    let joined: Vec<u8> = parts.join(&0u8);
    crate::pyfmt::sha256_hex(&joined)[..10].to_string()
}

/// The internal name of zone ZONE of the project at ROOT.
pub fn zone_id(root: &Path, zone: &str) -> String {
    format!("p{}", digest(&[root.as_os_str().as_bytes(), zone.as_bytes()]))
}

/// Whether NAME is shaped like a project zone's internal name (never a host zone's).
pub fn is_internal_name(name: &str) -> bool {
    name.len() == 11 && name.starts_with('p') && name[1..].bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// The project's root: from START (resolved) up, the first folder with an `appzones.toml`.
/// The walk stops after a folder that holds `.git` (a file or a folder: never running git),
/// and never looks at HOME itself, above it, or at `/`.
pub fn find_root(start: &Path, home: &Path) -> Result<PathBuf, String> {
    let start = std::fs::canonicalize(start).map_err(|e| format!("{}: {e}", start.display()))?;
    let home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    for dir in start.ancestors() {
        if dir == home || dir == Path::new("/") || !dir.starts_with(&home) {
            break;
        }
        if dir.join(FILE).symlink_metadata().is_ok() {
            return Ok(dir.to_path_buf());
        }
        if dir.join(".git").symlink_metadata().is_ok() {
            break;
        }
    }
    Err(format!("no {FILE} in {} or above it (up to the repository's root, inside your home)", shown(&start.to_string_lossy())))
}

/// The bytes of a regular file of this user at PATH, never through a symlink, at most 1 MiB.
fn read_plain(path: &Path) -> Result<Vec<u8>, String> {
    let shown_path = shown(&path.to_string_lossy());
    let mut f = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path)
        .map_err(|e| format!("{shown_path}: {e}"))?;
    let m = f.metadata().map_err(|e| format!("{shown_path}: {e}"))?;
    // Safety: getuid has no preconditions.
    if !m.is_file() || m.uid() != unsafe { libc::getuid() } {
        return Err(format!("{shown_path}: not a regular file of yours"));
    }
    if m.len() > MAX_BYTES {
        return Err(format!("{shown_path}: larger than 1 MiB"));
    }
    let mut b = Vec::new();
    f.by_ref().take(MAX_BYTES + 1).read_to_end(&mut b).map_err(|e| format!("{shown_path}: {e}"))?;
    if b.len() as u64 > MAX_BYTES {
        return Err(format!("{shown_path}: larger than 1 MiB"));
    }
    Ok(b)
}

/// The project file at ROOT, as it is now.
pub fn read_file(root: &Path) -> Result<Vec<u8>, String> {
    read_plain(&root.join(FILE))
}

/// A project file, parsed and checked: its default zone and its zones' own settings.
#[derive(Debug)]
pub struct ProjectFile {
    pub default: String,
    /// (zone, the keys it sets)
    pub zones: Vec<(String, Table)>,
}

/// BYTES as a project file (SHOWN: how to name it in errors). Every error, not the first.
pub fn parse(bytes: &[u8], shown_as: &str) -> Result<ProjectFile, Vec<String>> {
    let text = std::str::from_utf8(bytes).map_err(|_| vec![format!("{shown_as}: not UTF-8")])?;
    let raw: Table = text.parse().map_err(|e: toml::de::Error| vec![format!("{shown_as}: {}", e.message())])?;
    let mut errs = Vec::new();
    for k in raw.keys() {
        if !["version", "requires", "default", "zones"].contains(&k.as_str()) {
            errs.push(format!("{shown_as}: unknown top-level key {} (a project file has version, requires, default, \
                               zones)", py_repr(k)));
        }
    }
    if raw.get("version").and_then(Value::as_integer) != Some(1) {
        errs.push(format!("{shown_as}: version must be 1"));
    }
    match raw.get("requires").and_then(Value::as_integer) {
        None => errs.push(format!("{shown_as}: requires is needed in a project file (requires = {})", config::RELEASE)),
        Some(r) if r > config::RELEASE => {
            return Err(vec![format!("{shown_as} needs zones release {r}, and this `appzones` is release {}: update appzones",
                                    config::RELEASE)]);
        }
        Some(r) if r < SINCE_RELEASE => {
            errs.push(format!("{shown_as}: requires = {r}, but project files are release {SINCE_RELEASE}: set requires = \
                               {SINCE_RELEASE}"));
        }
        Some(_) => {}
    }
    let empty = Table::new();
    let zones_t = match raw.get("zones") {
        Some(Value::Table(t)) => t,
        Some(_) => {
            errs.push(format!("{shown_as}: zones must be a table"));
            &empty
        }
        None => &empty,
    };
    let mut zones = Vec::new();
    for (name, z) in zones_t {
        let p = format!("{shown_as}: [zones.{}]", shown(name));
        if !zone_name_ok(name) || name.len() > 20 {
            errs.push(format!("{p}: a zone name is lowercase letters, digits and -, 2 to 20 characters"));
            continue;
        }
        let Value::Table(z) = z else {
            errs.push(format!("{p} must be a table"));
            continue;
        };
        for (k, v) in z {
            if !PROJECT_KEYS.contains(&k.as_str()) {
                errs.push(if config::keys().iter().any(|(n, _)| n == k) {
                    format!("{p} {k}: never in a project file (only your own zones.toml may set it)")
                } else {
                    format!("{p}: unknown key {} (a project zone may set {})", py_repr(k), PROJECT_KEYS.join(", "))
                });
                continue;
            }
            let err = match k.as_str() {
                "network" => (!matches!(v.as_str(), Some("none") | Some("internet")))
                    .then(|| "must be \"none\" or \"internet\" in a project file".to_string()),
                "checkout" => (!matches!(v.as_str(), Some("ro") | Some("rw"))).then(|| "must be \"ro\" or \"rw\"".to_string()),
                "binds" => binds_error(v),
                _ => config::check_key(k, v),
            };
            if let Some(e) = err {
                errs.push(format!("{p} {k} {e}"));
            }
        }
        if z.get("allow").and_then(Value::as_array).is_some_and(|a| !a.is_empty())
            && z.get("network").and_then(Value::as_str) != Some("internet") {
            errs.push(format!("{p} allow needs network = \"internet\""));
        }
        zones.push((name.clone(), z.clone()));
    }
    if zones.is_empty() {
        errs.push(format!("{shown_as}: no zones (add [zones.NAME])"));
    }
    let default = match raw.get("default") {
        Some(Value::String(d)) if zones.iter().any(|(n, _)| n == d) => d.clone(),
        Some(Value::String(d)) => {
            errs.push(format!("{shown_as}: default = {}: no such zone in the file", py_repr(&shown(d))));
            String::new()
        }
        Some(_) => {
            errs.push(format!("{shown_as}: default must be a zone's name"));
            String::new()
        }
        None if zones.len() == 1 => zones[0].0.clone(),
        None => {
            errs.push(format!("{shown_as}: several zones and no default: say which, default = \"NAME\""));
            String::new()
        }
    };
    if errs.is_empty() { Ok(ProjectFile { default, zones }) } else { Err(errs) }
}

/// A project file's binds: relative paths inside the project, `ro` (the default) or `rw`.
fn binds_error(v: &Value) -> Option<String> {
    let Value::Array(a) = v else { return Some("must be a list of { path, mode }".into()) };
    for b in a {
        let Some(t) = b.as_table() else { return Some("must be a list of { path, mode }".into()) };
        if t.keys().any(|k| k != "path" && k != "mode") {
            return Some("entries have path and mode".into());
        }
        let Some(path) = t.get("path").and_then(Value::as_str).filter(|p| !p.is_empty()) else {
            return Some("entries need a path".into());
        };
        if path.starts_with('/') || path.starts_with('~') || Path::new(path).components()
            .any(|c| !matches!(c, std::path::Component::Normal(_) | std::path::Component::CurDir)) || path.contains('\0') {
            return Some(format!("{}: in a project file a bind is a relative path inside the project (no /, ~ or ..)",
                                py_repr(&shown(path))));
        }
        if !t.get("mode").is_none_or(|m| matches!(m.as_str(), Some("ro") | Some("rw"))) {
            return Some(format!("{}: mode is \"ro\" or \"rw\"", py_repr(&shown(path))));
        }
    }
    None
}

// ── the trust store ─────────────────────────────────────────────────────

/// Where trusted copies live: $XDG_STATE_HOME/appzones/projects (absolute), else
/// ~/.local/state/appzones/projects: on the never-list, so no zone reads or writes it.
pub fn store(home: &Path) -> PathBuf {
    match std::env::var_os("XDG_STATE_HOME").filter(|s| s.as_bytes().starts_with(b"/")) {
        Some(s) => PathBuf::from(s).join("appzones/projects"),
        None => home.join(".local/state/appzones/projects"),
    }
}

/// The record of the project at ROOT: its folder in the store.
pub fn record_dir(home: &Path, root: &Path) -> PathBuf {
    store(home).join(digest(&[root.as_os_str().as_bytes()]))
}

/// A trusted project: its root and the copy of its file.
pub struct Trusted {
    pub dir: PathBuf,
    pub root: PathBuf,
    pub copy: Vec<u8>,
}

/// A folder of this user, mode 0700, not a symlink.
fn private_dir(p: &Path) -> bool {
    // Safety: getuid has no preconditions.
    p.symlink_metadata().is_ok_and(|m| m.is_dir() && m.uid() == unsafe { libc::getuid() } && m.mode() & 0o077 == 0)
}

/// The record at DIR, None when it is not a sound one (then it is never used).
fn read_record(dir: &Path) -> Option<Trusted> {
    if !private_dir(dir) {
        return None;
    }
    let root = String::from_utf8(read_plain(&dir.join("root")).ok()?).ok()?;
    let root = PathBuf::from(root.trim_end_matches('\n'));
    let copy = read_plain(&dir.join(FILE)).ok()?;
    (root.is_absolute() && dir.file_name()? == digest(&[root.as_os_str().as_bytes()]).as_str())
        .then_some(Trusted { dir: dir.to_path_buf(), root, copy })
}

/// The project at ROOT, when it is trusted.
pub fn trusted_at(home: &Path, root: &Path) -> Option<Trusted> {
    read_record(&record_dir(home, root))
}

/// The trusted project whose root holds START (resolved), the innermost: for commands that
/// manage a project's zones also when its file is gone.
pub fn trusted_holding(home: &Path, start: &Path) -> Option<Trusted> {
    let start = std::fs::canonicalize(start).ok()?;
    all_trusted(home).into_iter().filter(|t| start.starts_with(&t.root)).max_by_key(|t| t.root.as_os_str().len())
}

/// The zone names of a project file's bytes (none when it does not parse).
pub fn zone_names(bytes: &[u8]) -> Vec<String> {
    parse(bytes, FILE).map(|f| f.zones.into_iter().map(|(n, _)| n).collect()).unwrap_or_default()
}

/// Every trusted project.
pub fn all_trusted(home: &Path) -> Vec<Trusted> {
    let s = store(home);
    if !private_dir(&s) {
        return vec![];
    }
    let mut v: Vec<Trusted> = std::fs::read_dir(&s).map(|it| it.flatten().filter_map(|e| read_record(&e.path())).collect())
        .unwrap_or_default();
    v.sort_by(|a, b| a.root.cmp(&b.root));
    v
}

/// A new file at PATH (0600) with DATA, written beside it and moved into place.
fn write_private(path: &Path, data: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let tmp = path.with_extension("new");
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW)
        .open(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    f.write_all(data).and_then(|_| f.sync_all()).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Trust the project at ROOT with BYTES (its file as shown to the person).
pub fn record(home: &Path, root: &Path, bytes: &[u8]) -> Result<(), String> {
    let s = store(home);
    for d in [s.parent().unwrap_or(&s), s.as_path()] {
        let _ = std::fs::create_dir_all(d);
    }
    std::fs::set_permissions(&s, std::fs::Permissions::from_mode(0o700)).map_err(|e| format!("{}: {e}", s.display()))?;
    let dir = record_dir(home, root);
    if !dir.exists() {
        std::fs::create_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| format!("{}: {e}", dir.display()))?;
    if !private_dir(&dir) {
        return Err(format!("{}: not a private folder of yours", dir.display()));
    }
    write_private(&dir.join("root"), format!("{}\n", root.display()).as_bytes())?;
    write_private(&dir.join(FILE), bytes)
}

/// Forget the project whose record is DIR.
pub fn forget(dir: &Path) -> Result<(), String> {
    std::fs::remove_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))
}

// ── zones from trusted copies ───────────────────────────────────────────

/// The settings a project zone starts from: the host's built-in ones, without a desktop.
fn baseline() -> Table {
    let mut t: Table = config::keys().into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    for (k, v) in [("network", Value::String("none".into())), ("devices", Value::Array(vec![])),
                   ("wayland", Value::String("none".into())), ("theme", Value::Boolean(false))] {
        t.insert(k.into(), v);
    }
    t
}

/// The binds of a project zone: the checkout itself, then its relative binds, resolved and
/// still inside the project; Err when one leads outside it.
fn zone_binds(root: &Path, z: &Table) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    let checkout = z.get("checkout").and_then(Value::as_str).unwrap_or("ro");
    let bind = |p: &Path, mode: &str| {
        let mut t = Table::new();
        t.insert("path".into(), Value::String(p.to_string_lossy().into_owned()));
        t.insert("mode".into(), Value::String(mode.into()));
        Value::Table(t)
    };
    out.push(bind(root, checkout));
    for b in z.get("binds").and_then(Value::as_array).cloned().unwrap_or_default() {
        let rel = b.get("path").and_then(Value::as_str).unwrap_or("");
        let mode = b.get("mode").and_then(Value::as_str).unwrap_or("ro");
        let real = std::fs::canonicalize(root.join(rel)).map_err(|e| format!("bind {}: {e}", shown(rel)))?;
        if !real.starts_with(root) {
            return Err(format!("bind {}: leads outside the project ({})", shown(rel), shown(&real.to_string_lossy())));
        }
        out.push(bind(&real, mode));
    }
    Ok(out)
}

/// Err when a bind of a zone of F leads outside the project at ROOT (or does not exist).
pub fn check_binds(root: &Path, f: &ProjectFile) -> Result<(), String> {
    for (zname, z) in &f.zones {
        zone_binds(root, z).map_err(|e| format!("zone {zname}: {e}"))?;
    }
    Ok(())
}

/// The zones of the trusted projects, for a host whose zones are HOST and whose own additions
/// to project zones are OVERRIDES (zones.toml's [projects."PATH".zones.NAME]): (zones, how to
/// show them, warnings for projects that cannot be used now).
pub fn zones(home: &Path, host: &[Zone], overrides: &[(Vec<u8>, String, Table)]) -> (Vec<Zone>, Vec<ProjectZone>, Vec<String>) {
    let (mut zones, mut shown_as, mut warnings) = (Vec::new(), Vec::new(), Vec::new());
    let mut colours: Vec<String> = host.iter().filter_map(|z| z.get("colour").as_str().map(str::to_lowercase)).collect();
    // the host zones' own homes, resolved (a home whose path does not resolve: "/", which
    // every project is inside, so no project is used next to it)
    let homes: Vec<PathBuf> = host.iter().filter_map(|z| z.get("home").as_str()).filter(|h| !h.is_empty())
        .map(|h| {
            let p = config::policy(Default::default()).expand(h.as_bytes());
            crate::policy::realpath(&p).map(|r| PathBuf::from(std::ffi::OsString::from(std::ffi::OsStr::from_bytes(&r))))
                .unwrap_or_else(|| PathBuf::from("/"))
        })
        .collect();
    for t in all_trusted(home) {
        let label = t.root.file_name().map(|n| shown(&n.to_string_lossy())).unwrap_or_else(|| "?".into());
        let file = match parse(&t.copy, &format!("{label}/{FILE}")) {
            Ok(f) => f,
            Err(e) => {
                warnings.push(format!("project {}: its trusted file is not valid for this appzones ({}); appzones trust it \
                                       again", shown(&t.root.to_string_lossy()), e.join("; ")));
                continue;
            }
        };
        // a project inside (or holding) a host zone's own home would share it
        if let Some(h) = homes.iter().find(|h| t.root.starts_with(h) || h.starts_with(&t.root)) {
            warnings.push(format!("project {}: it is inside (or holds) {}, a host zone's home; not used",
                                  shown(&t.root.to_string_lossy()), shown(&h.to_string_lossy())));
            continue;
        }
        for (zname, zt) in &file.zones {
            let name = zone_id(&t.root, zname);
            let mut settings = baseline();
            for (k, v) in zt {
                if k != "binds" && k != "checkout" {
                    settings.insert(k.clone(), v.clone());
                }
            }
            match zone_binds(&t.root, zt) {
                Ok(b) => {
                    settings.insert("binds".into(), Value::Array(b));
                }
                Err(e) => {
                    warnings.push(format!("project {} zone {zname}: {e}; not used", shown(&t.root.to_string_lossy())));
                    continue;
                }
            }
            // a colour of its own, never a host zone's or another project zone's
            let start = u64::from_str_radix(&name[1..9], 16).unwrap_or(0) as usize;
            let colour = (0..PALETTE.len()).map(|i| PALETTE[(start + i) % PALETTE.len()])
                .find(|c| !colours.iter().any(|u| u == c)).map(String::from)
                .unwrap_or_else(|| format!("#{}", &name[1..7]));
            colours.push(colour.clone());
            settings.insert("colour".into(), Value::String(colour));
            let mut origin: Vec<(&'static str, &'static str)> = config::keys().into_iter()
                .map(|(k, _)| (k, if zt.contains_key(k) || k == "binds" { "zone" } else { "builtin" })).collect();
            // the user's own additions (their zones.toml): binds added, env merged, the rest replaced
            for (_, _, ot) in overrides.iter().filter(|(p, z, _)| z == zname && same_root(p, &t.root)) {
                for (k, v) in ot {
                    match (k.as_str(), v, settings.get_mut(k.as_str())) {
                        ("binds", Value::Array(add), Some(Value::Array(b))) => b.extend(add.iter().cloned()),
                        ("env", Value::Table(add), Some(Value::Table(e))) => e.extend(add.clone()),
                        _ => {
                            settings.insert(k.clone(), v.clone());
                        }
                    }
                    if let Some(o) = origin.iter_mut().find(|(n, _)| *n == k) {
                        o.1 = "yours";
                    }
                }
            }
            let zone = Zone { name: name.clone(), settings, origin, project: true };
            // the whole zone checked as the host's are (the user's additions may not fit the rest)
            let errs = config::check_zone(&zone, host);
            if !errs.is_empty() {
                warnings.push(format!("project {} zone {zname}: {} (with your additions in zones.toml); not used",
                                      shown(&t.root.to_string_lossy()), errs.join("; ")));
                continue;
            }
            zones.push(zone);
            shown_as.push(ProjectZone { name, shown: format!("{label}/{zname}"), root: t.root.clone() });
        }
    }
    (zones, shown_as, warnings)
}

/// Whether PATH (from the user's zones.toml, expanded) is the project at ROOT (resolved).
fn same_root(path: &[u8], root: &Path) -> bool {
    std::fs::canonicalize(std::ffi::OsStr::from_bytes(path)).is_ok_and(|p| p == root)
}

/// What a project file asks for, to show before it is trusted: the widenings (against a
/// zone with no network, no desktop and its checkout read-only) first, then the rest.
pub fn summary(root: &Path, f: &ProjectFile) -> (Vec<String>, Vec<String>) {
    let (mut wide, mut rest) = (Vec::new(), Vec::new());
    for (zname, z) in &f.zones {
        let s = |k: &str| z.get(k).and_then(Value::as_str).unwrap_or("");
        let p = format!("@{}{}", shown(zname), if *zname == f.default { " (the default)" } else { "" });
        let allow: Vec<String> = z.get("allow").and_then(Value::as_array).map(|a| a.iter()
            .filter_map(|x| x.as_str().map(shown)).collect()).unwrap_or_default();
        if s("network") == "internet" && allow.is_empty() {
            wide.push(format!("{p}: the internet, any host"));
        } else if s("network") == "internet" {
            wide.push(format!("{p}: the internet, only {}", allow.join(", ")));
        }
        if s("checkout") == "rw" {
            wide.push(format!("{p}: the project folder {} READ-WRITE", shown(&root.to_string_lossy())));
        } else {
            rest.push(format!("{p}: the project folder {}, read-only", shown(&root.to_string_lossy())));
        }
        for b in z.get("binds").and_then(Value::as_array).cloned().unwrap_or_default() {
            let path = shown(b.get("path").and_then(Value::as_str).unwrap_or(""));
            if b.get("mode").and_then(Value::as_str) == Some("rw") {
                wide.push(format!("{p}: {path} (in the project) READ-WRITE"));
            } else {
                rest.push(format!("{p}: {path} (in the project), read-only"));
            }
        }
        let pk: Vec<String> = z.get("packages").and_then(Value::as_array).map(|a| a.iter()
            .filter_map(|x| x.as_str().map(shown)).collect()).unwrap_or_default();
        if !pk.is_empty() {
            wide.push(format!("{p}: packages built on this host by Nix: {}", pk.join(", ")));
        }
        if let Some(env) = z.get("env").and_then(Value::as_table) {
            for (k, v) in env {
                rest.push(format!("{p}: environment {}={}", shown(k), shown(v.as_str().unwrap_or(""))));
            }
        }
        for k in ["tmp", "linger", "start_timeout"] {
            if let Some(v) = z.get(k) {
                rest.push(format!("{p}: {k} = {}", shown(&v.as_str().map(String::from).unwrap_or_else(|| format!("{v:?}")))));
            }
        }
    }
    (wide, rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let t = std::env::temp_dir().join(format!("az-project-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        std::fs::canonicalize(t).unwrap()
    }

    #[test]
    fn files() {
        let ok = |t: &str| parse(t.as_bytes(), "p/appzones.toml");
        let f = ok("version = 1\nrequires = 8\n[zones.agent]\nnetwork = \"internet\"\nallow = [\"github.com\"]\n").unwrap();
        assert_eq!((f.default.as_str(), f.zones.len()), ("agent", 1));
        let f = ok("version = 1\nrequires = 8\ndefault = \"dev\"\n[zones.agent]\n[zones.dev]\ncheckout = \"rw\"\n").unwrap();
        assert_eq!(f.default, "dev");
        for (text, says) in [
            ("version = 1\n[zones.a1]\n", "requires is needed"),
            ("version = 1\nrequires = 7\n[zones.a1]\n", "requires = 8"),
            ("version = 1\nrequires = 99\n[zones.a1]\n", "update appzones"),
            ("version = 1\nrequires = 8\n[zones.a1]\n[zones.b1]\n", "no default"),
            ("version = 1\nrequires = 8\ndefault = \"zz\"\n[zones.a1]\n", "no such zone"),
            ("version = 1\nrequires = 8\n[zones.a1]\nnetwork = \"host\"\n", "\"none\" or \"internet\""),
            ("version = 1\nrequires = 8\n[zones.a1]\ndevices = [\"gpu\"]\n", "never in a project file"),
            ("version = 1\nrequires = 8\n[zones.a1]\ndbus = { talk = [\"x.y\"] }\n", "never in a project file"),
            ("version = 1\nrequires = 8\n[zones.a1]\nhome = \"~/x\"\n", "never in a project file"),
            ("version = 1\nrequires = 8\n[zones.a1]\ncolour = \"#000000\"\n", "never in a project file"),
            ("version = 1\nrequires = 8\n[zones.a1]\nbinds = [{ path = \"~/.ssh\" }]\n", "relative path inside"),
            ("version = 1\nrequires = 8\n[zones.a1]\nbinds = [{ path = \"../x\" }]\n", "relative path inside"),
            ("version = 1\nrequires = 8\n[zones.a1]\nbinds = [{ path = \"/etc\" }]\n", "relative path inside"),
            ("version = 1\nrequires = 8\n[zones.a1]\nallow = [\"github.com\"]\n", "needs network"),
            ("version = 1\nrequires = 8\n[zones.a1]\nenv = { SSH_AUTH_SOCK = \"x\" }\n", "never passed"),
            ("version = 1\nrequires = 8\n[zones.a1]\nsandwich = 1\n", "unknown key"),
            ("version = 1\nrequires = 8\nzones = 3\n", "zones must be a table"),
            ("version = 1\nrequires = 8\nautostart = true\n[zones.a1]\n", "unknown top-level key"),
            ("version = 1\nrequires = 8\n[zones.\"p0123456789\"]\n", ""),
        ] {
            let r = ok(text);
            if says.is_empty() {
                continue;
            }
            let e = r.expect_err(text).join("\n");
            assert!(e.contains(says), "{text}: {e}");
        }
    }

    #[test]
    fn names_and_escapes() {
        assert_eq!(shown("a\u{202e}b\x1b[2Kc"), "a\\u{202e}b\\u{1b}[2Kc");
        let id = zone_id(Path::new("/home/u/src/app"), "agent");
        assert!(is_internal_name(&id) && zone_name_ok(&id), "{id}");
        assert_ne!(id, zone_id(Path::new("/home/u/src/app"), "dev"));
        assert_ne!(id, zone_id(Path::new("/home/u/src/app2"), "agent"));
        assert!(!is_internal_name("pABCDEF0123") && !is_internal_name("work"));
    }

    #[test]
    fn roots() {
        let t = tmp("roots");
        let home = t.join("home");
        std::fs::create_dir_all(home.join("src/app/.git")).unwrap();
        std::fs::create_dir_all(home.join("src/app/deep/er")).unwrap();
        std::fs::create_dir_all(home.join("src/lone/x")).unwrap();
        std::fs::write(home.join("src/app/appzones.toml"), "").unwrap();
        std::fs::write(home.join("appzones.toml"), "").unwrap(); // the home's own: never a project
        assert_eq!(find_root(&home.join("src/app/deep/er"), &home).unwrap(), home.join("src/app"));
        assert!(find_root(&home.join("src/lone/x"), &home).is_err(), "never the home's file");
        // a repository without one: the walk stops at its .git
        std::fs::create_dir_all(home.join("src/app/sub/.git")).unwrap();
        assert!(find_root(&home.join("src/app/sub"), &home).is_err());
        assert!(find_root(&t, &home).is_err(), "outside the home");
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn binds_stay_inside() {
        let t = tmp("binds");
        std::fs::create_dir_all(t.join("root/data")).unwrap();
        std::fs::create_dir_all(t.join("outside")).unwrap();
        std::os::unix::fs::symlink(t.join("outside"), t.join("root/link")).unwrap();
        let z = |b: &str| format!("binds = [{{ path = \"{b}\", mode = \"rw\" }}]").parse::<Table>().unwrap();
        let r = t.join("root");
        let b = zone_binds(&r, &z("data")).unwrap();
        assert_eq!(b[0].get("mode").and_then(Value::as_str), Some("ro"), "the checkout: read-only by default");
        assert_eq!(b[1].get("path").and_then(Value::as_str), Some(t.join("root/data").to_str().unwrap()));
        assert!(zone_binds(&r, &z("link")).unwrap_err().contains("leads outside"), "a symlink out of the project");
        let _ = std::fs::remove_dir_all(&t);
    }
}
