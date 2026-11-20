//! The never-list for runtime mounts, as the root step (`appzones mount`) applies it:
//! the port of zonecfg.mount_source_error and what it uses (denied_paths,
//! host_denied_paths, policy_error, bind_error, kind_error), with the same answers and
//! the same messages. The Python side keeps its own for binds; a differential test
//! (DifferentialPolicyTest in src/test_zonecfg.py) runs both on the same paths and wants
//! the same result for each.
//!
//! No unsafe code, no environment: what it needs (the caller's home, appzones' code
//! directory, the host's denied-paths file) is passed in.

#![forbid(unsafe_code)]

use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::path::{Component, Path};

pub const HOST_DENIED_FILE: &str = "/etc/appzones/denied-paths";
pub const LEGACY_HOST_DENIED_FILE: &str = "/etc/zones/denied-paths";

/// The caller's own files that never enter a zone (zonecfg.denied_paths, in its order).
const DENIED_IN_HOME: &[&str] = &[
    ".ssh", ".gnupg", ".config/sops", ".password-store", ".local/share/keyrings", ".local/share/kwalletd",
    ".config/opencode", ".config/appzones", ".local/share/appzones", ".config/zones", ".local/share/zones",
    ".local/state/appzones", ".config/hypr", ".config/systemd", ".config/autostart", ".config/environment.d",
    ".config/uwsm", ".local/share/applications", ".local/bin", ".bin", ".bashrc", ".bash_profile", ".profile",
    ".zshrc", ".zshenv", ".zprofile", ".config/fish", ".config/nix", ".config/git", ".gitconfig",
    ".config/kwalletrc",
];
const DENIED_SYSTEM: &[&str] = &["/run/secrets", "/run/user", "/var/lib/sops-nix", "/etc/ssh", "/nix/var"];
/// Never bound whole.
const FORBIDDEN_WHOLE: &[&str] = &["/", "/home", "~", "/nix", "/etc", "/proc", "/sys", "/dev", "/run", "/tmp", "/var"];
/// Never the source of a runtime mount, nor anything inside them (outside the home).
pub const MOUNT_NEVER_INSIDE: &[&str] = &["/proc", "/sys", "/dev", "/run", "/etc", "/nix", "/boot", "/root", "/var", "/tmp"];

/// What the policy is applied for: whose home, which code directory, which host file.
pub struct Policy {
    pub home: String,
    pub code_dir: String,
    /// None: the host's file (HOST_DENIED_FILE, or the legacy one when it is absent)
    pub denied_file: Option<String>,
}

/// Python's os.path.realpath (non-strict): symlinks resolved as far as the path exists;
/// the rest is appended as it is.
pub fn realpath(path: &str) -> String {
    let mut todo: Vec<String> = path.split('/').rev().map(String::from).collect();
    let mut resolved = String::from("/");
    let mut links = 0;
    while let Some(c) = todo.pop() {
        match c.as_str() {
            "" | "." => continue,
            ".." => {
                resolved = parent_str(&resolved);
                continue;
            }
            _ => {}
        }
        let next = if resolved == "/" { format!("/{c}") } else { format!("{resolved}/{c}") };
        match fs::symlink_metadata(&next) {
            Ok(m) if m.file_type().is_symlink() && links < 40 => {
                links += 1;
                let Ok(target) = fs::read_link(&next) else {
                    resolved = next;
                    continue;
                };
                let target = target.to_string_lossy().into_owned();
                if target.starts_with('/') {
                    resolved = String::from("/");
                }
                for t in target.split('/').rev() {
                    todo.push(t.to_string());
                }
            }
            _ => resolved = next,
        }
    }
    resolved
}

fn parent_str(p: &str) -> String {
    match p.rfind('/') {
        Some(0) | None => "/".into(),
        Some(i) => p[..i].to_string(),
    }
}

/// str(pathlib.PurePosixPath(p)): repeated slashes and "." parts gone, no trailing slash,
/// a leading "//" kept (and three or more become one).
pub fn py_path_str(p: &str) -> String {
    if p.is_empty() {
        return ".".into();
    }
    let lead = if p.starts_with("//") && !p.starts_with("///") { "//" } else if p.starts_with('/') { "/" } else { "" };
    let parts: Vec<&str> = p.split('/').filter(|c| !c.is_empty() && *c != ".").collect();
    let body = parts.join("/");
    match (lead, body.is_empty()) {
        ("", true) => ".".into(),
        (l, true) => l.into(),
        (l, false) => format!("{l}{body}"),
    }
}

/// pathlib's is_relative_to and ==, part by part.
fn inside(p: &str, dir: &str) -> bool {
    Path::new(p).starts_with(Path::new(dir))
}

fn same(a: &str, b: &str) -> bool {
    let norm = |s: &str| -> Vec<String> {
        Path::new(s).components().filter(|c| !matches!(c, Component::CurDir))
            .map(|c| c.as_os_str().to_string_lossy().into_owned()).collect()
    };
    norm(a) == norm(b)
}

impl Policy {
    /// zonecfg.expand: a leading "~" is the home.
    fn expand(&self, raw: &str) -> String {
        match raw.strip_prefix('~') {
            Some(rest) => py_path_str(&format!("{}{rest}", self.home)),
            None => py_path_str(raw),
        }
    }

    fn home_real(&self) -> String {
        realpath(&self.home)
    }

    /// The host's additions: a missing file means none (fail closed on anything else).
    pub fn host_denied_paths(&self) -> Result<Vec<String>, String> {
        let path = match &self.denied_file {
            Some(p) => p.clone(),
            None if fs::symlink_metadata(HOST_DENIED_FILE).is_err()
                && fs::symlink_metadata(LEGACY_HOST_DENIED_FILE).is_ok() => LEGACY_HOST_DENIED_FILE.into(),
            None => HOST_DENIED_FILE.into(),
        };
        let text = match fs::read(&path) {
            Ok(b) => String::from_utf8(b).map_err(|_| format!("{path}: cannot read the host's denied paths: not UTF-8"))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && self.denied_file.is_none() => return Ok(Vec::new()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(format!("{path} (APPZONES_DENIED_PATHS) does not exist"))
            }
            Err(e) => return Err(format!("{path}: cannot read the host's denied paths: {e}")),
        };
        let mut out = Vec::new();
        // the line breaks Python's splitlines() knows
        let text = text.replace("\r\n", "\n");
        let breaks = |c: char| matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}'
                                         | '\u{2028}' | '\u{2029}');
        let lines: Vec<&str> = text.strip_suffix(breaks).unwrap_or(&text).split(breaks).collect();
        for (n, line) in lines.into_iter().enumerate() {
            let p = line.split('#').next().unwrap_or("").trim();
            if p.is_empty() {
                continue;
            }
            if p == "~" {
                out.push(py_path_str(&self.home));
            } else if let Some(rest) = p.strip_prefix("~/") {
                out.push(py_path_str(&format!("{}/{rest}", self.home)));
            } else if p.starts_with('/') {
                out.push(py_path_str(p));
            } else {
                return Err(format!("{path}:{}: '{p}' must be absolute or start with ~/", n + 1));
            }
        }
        Ok(out)
    }

    /// zonecfg.denied_paths, in its order (the first match gives the message).
    pub fn denied_paths(&self) -> Result<Vec<String>, String> {
        let h = py_path_str(&self.home);
        let mut out: Vec<String> = DENIED_SYSTEM.iter().map(|s| s.to_string()).collect();
        out.extend(DENIED_IN_HOME.iter().map(|r| format!("{h}/{r}")));
        out.push(py_path_str(&self.code_dir));
        out.extend(self.host_denied_paths()?);
        for (parent, prefix) in [(format!("{h}/.local/share"), "opencode"), (h.clone(), ".claude")] {
            if !Path::new(&parent).is_dir() {
                continue;
            }
            if let Ok(entries) = fs::read_dir(&parent) {
                for e in entries.flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if name.starts_with(prefix) {
                        out.push(format!("{parent}/{name}"));
                    }
                }
            }
        }
        Ok(out)
    }

    /// zonecfg.policy_error: why the resolved path REAL may never enter a zone.
    pub fn policy_error(&self, real: &str, raw: &str) -> Option<String> {
        let via = if real != self.expand(raw) { format!(" (resolves to {real})") } else { String::new() };
        let home_real = self.home_real();
        for f in FORBIDDEN_WHOLE {
            let f = if *f == "~" { home_real.as_str() } else { f };
            if same(real, f) {
                return Some(format!("{raw}{via}: binding {f} whole is never allowed"));
            }
        }
        let denied = match self.denied_paths() {
            Ok(d) => d,
            Err(e) => return Some(format!("{raw}: {e}")),
        };
        for d in &denied {
            let dr = realpath(d);
            let how = if same(real, &dr) {
                "is"
            } else if inside(&dr, real) {
                "contains"
            } else if inside(real, &dr) {
                "is inside"
            } else {
                continue;
            };
            return Some(format!("{raw}{via}: {how} {d}, which never enters a zone"));
        }
        if !inside(real, &home_real) {
            for d in MOUNT_NEVER_INSIDE {
                if inside(real, d) {
                    return Some(format!("{raw}{via}: is inside {d}, which never enters a zone"));
                }
            }
        }
        None
    }

    /// zonecfg.kind_error: only directories and regular files are bound.
    fn kind_error(meta: &fs::Metadata, raw: &str) -> Option<String> {
        let t = meta.file_type();
        if t.is_dir() || t.is_file() {
            return None;
        }
        let what = if t.is_socket() {
            "a socket"
        } else if t.is_fifo() {
            "a FIFO"
        } else if t.is_char_device() || t.is_block_device() {
            "a device"
        } else {
            "not a file or directory"
        };
        Some(format!("{raw}: is {what}; only directories and files are bound"))
    }

    /// zonecfg.bind_error.
    pub fn bind_error(&self, raw: &str) -> Option<String> {
        let p = self.expand(raw);
        if !p.starts_with('/') {
            return Some(format!("{raw}: must be absolute or start with ~"));
        }
        let real = realpath(&p);
        if let Some(e) = self.policy_error(&real, raw) {
            return Some(e);
        }
        match fs::metadata(&p) {
            Ok(m) => Self::kind_error(&m, raw),
            Err(_) => Some(format!("{raw}: does not exist")),
        }
    }

    /// zonecfg.mount_source_error: why a folder may not be mounted into a zone at run time.
    pub fn mount_source_error(&self, raw: &str) -> Option<String> {
        let p = self.expand(raw);
        if !p.starts_with('/') {
            return Some(format!("{raw}: must be absolute or start with ~"));
        }
        let real = realpath(&p);
        if !inside(&real, &self.home_real()) {
            for d in MOUNT_NEVER_INSIDE {
                if same(&real, d) || inside(&real, d) {
                    let via = if real != p { format!(" (resolves to {real})") } else { String::new() };
                    return Some(format!("{raw}{via}: is inside {d}, which is never mounted into a zone"));
                }
            }
        }
        if let Some(e) = self.bind_error(raw) {
            return Some(e);
        }
        if Path::new(&p).is_dir() { None } else { Some(format!("{raw}: not a directory")) }
    }
}

/// A zone name (zonecfg.NAME_RE: ^[a-z][a-z0-9-]{1,20}$, no trailing newline either).
#[allow(dead_code)] // the mount helper's (next step)
pub fn zone_name_ok(s: &str) -> bool {
    let b = s.as_bytes();
    (2..=21).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b[1..].iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// Directories under the state directory that are not zones (zonecfg.RESERVED).
#[allow(dead_code)] // the mount helper's (next step)
pub const RESERVED: &[&str] = &["launcher", "share", "bin", "lib", "etc"];

/// A runtime mount's name (zonecfg.MOUNT_NAME_RE: ^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$).
#[allow(dead_code)] // the mount helper's (next step)
pub fn mount_name_ok(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=64).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b[1..].iter().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn py_paths() {
        assert_eq!(py_path_str("/a//b/./c/"), "/a/b/c");
        assert_eq!(py_path_str("//etc"), "//etc");
        assert_eq!(py_path_str("///etc"), "/etc");
        assert_eq!(py_path_str("/"), "/");
        assert_eq!(py_path_str("/a/../b"), "/a/../b");
        assert_eq!(py_path_str("x"), "x");
    }

    #[test]
    fn names() {
        assert!(zone_name_ok("ab") && zone_name_ok("work-2") && !zone_name_ok("a") && !zone_name_ok("Ab"));
        assert!(!zone_name_ok("ab\n") && !zone_name_ok(&"a".repeat(22)) && !zone_name_ok("-ab"));
        assert!(mount_name_ok("dl") && mount_name_ok("My.Folder_1") && !mount_name_ok(".x") && !mount_name_ok("a/b"));
        assert!(!mount_name_ok("") && !mount_name_ok(&"a".repeat(65)) && !mount_name_ok("a\n"));
    }

    #[test]
    fn realpath_resolves_and_keeps_the_rest() {
        let t = std::env::temp_dir().join(format!("az-policy-{}", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        fs::create_dir_all(t.join("real/sub")).unwrap();
        std::os::unix::fs::symlink(t.join("real"), t.join("link")).unwrap();
        let tr = fs::canonicalize(&t).unwrap().to_string_lossy().into_owned();
        assert_eq!(realpath(&format!("{tr}/link/sub")), format!("{tr}/real/sub"));
        assert_eq!(realpath(&format!("{tr}/link/missing/x")), format!("{tr}/real/missing/x"));
        assert_eq!(realpath(&format!("{tr}/real/sub/..")), format!("{tr}/real"));
        let _ = fs::remove_dir_all(&t);
    }
}
