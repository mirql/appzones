//! The never-list for runtime mounts, as the root step (`appzones mount`) applies it:
//! the port of zonecfg.mount_source_error and what it uses (denied_paths,
//! host_denied_paths, policy_error, bind_error, kind_error), with the same answers and
//! the same messages. The Python side keeps its own for binds; a differential test
//! (DifferentialPolicyTest in src/test_zonecfg.py) compares both lists and both answers.
//!
//! No unsafe code, no environment: what it needs (the caller's home, appzones' code
//! directory, the host's denied-paths file) is passed in. Paths stay bytes (OsStr) until a
//! message is written. Where it cannot be sure (a symlink loop, a directory it cannot
//! list, a bad line in the host's file) it refuses: this runs as root.

#![forbid(unsafe_code)]

use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
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
/// Never bound whole ("~": the home).
const FORBIDDEN_WHOLE: &[&str] = &["/", "/home", "~", "/nix", "/etc", "/proc", "/sys", "/dev", "/run", "/tmp", "/var"];
/// Never the source of a runtime mount, nor anything inside them (outside the home).
pub const MOUNT_NEVER_INSIDE: &[&str] = &["/proc", "/sys", "/dev", "/run", "/etc", "/nix", "/boot", "/root", "/var", "/tmp"];
/// More symlinks than this on one path: a loop, or as good as one. Refused.
const MAX_LINKS: usize = 256;

/// What the policy is applied for: whose home, which code directory, which host file.
pub struct Policy {
    pub home: OsString,
    pub code_dir: OsString,
    /// None: the host's file (HOST_DENIED_FILE, or the legacy one when it is absent)
    pub denied_file: Option<OsString>,
}

fn show(p: &[u8]) -> String {
    String::from_utf8_lossy(p).into_owned()
}

fn join(a: &[u8], b: &[u8]) -> Vec<u8> {
    let mut out = a.to_vec();
    if out.last() != Some(&b'/') {
        out.push(b'/');
    }
    out.extend_from_slice(b);
    out
}

/// Python's os.path.realpath (non-strict): symlinks resolved as far as the path exists,
/// the rest appended as it is. None for a loop (more than MAX_LINKS links).
pub fn realpath(path: &[u8]) -> Option<Vec<u8>> {
    let mut todo: Vec<Vec<u8>> = path.split(|&b| b == b'/').rev().map(<[u8]>::to_vec).collect();
    let mut resolved = b"/".to_vec();
    let mut links = 0;
    while let Some(c) = todo.pop() {
        match c.as_slice() {
            b"" | b"." => continue,
            b".." => {
                resolved = parent(&resolved);
                continue;
            }
            _ => {}
        }
        let next = join(&resolved, &c);
        let p = Path::new(OsStr::from_bytes(&next));
        match fs::symlink_metadata(p) {
            // missing (or below a non-directory): the rest stays as it is, as Python's
            Err(e) if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory) => {
                resolved = next
            }
            Err(_) => return None, // could not look: never a guess
            Ok(m) if m.file_type().is_symlink() => {
                links += 1;
                if links > MAX_LINKS {
                    return None;
                }
                let target = fs::read_link(p).ok()?.into_os_string().into_vec();
                if target.starts_with(b"/") {
                    resolved = b"/".to_vec();
                }
                todo.extend(target.split(|&b| b == b'/').rev().map(<[u8]>::to_vec));
            }
            _ => resolved = next,
        }
    }
    Some(resolved)
}

fn parent(p: &[u8]) -> Vec<u8> {
    match p.iter().rposition(|&b| b == b'/') {
        Some(0) | None => b"/".to_vec(),
        Some(i) => p[..i].to_vec(),
    }
}

/// str(pathlib.PurePosixPath(p)): repeated slashes and "." parts gone, no trailing slash,
/// a leading "//" kept (three or more become one).
pub fn py_path(p: &[u8]) -> Vec<u8> {
    if p.is_empty() {
        return b".".to_vec();
    }
    let lead: &[u8] = if p.starts_with(b"//") && !p.starts_with(b"///") {
        b"//"
    } else if p.starts_with(b"/") {
        b"/"
    } else {
        b""
    };
    let parts: Vec<&[u8]> = p.split(|&b| b == b'/').filter(|c| !c.is_empty() && *c != b".").collect();
    let body = parts.join(&b'/');
    match (lead.is_empty(), body.is_empty()) {
        (true, true) => b".".to_vec(),
        (false, true) => lead.to_vec(),
        _ => [lead, &body].concat(),
    }
}

/// pathlib's HOME / rest: an absolute rest replaces the home.
fn home_join(home: &[u8], rest: &[u8]) -> Vec<u8> {
    py_path(&if rest.starts_with(b"/") { rest.to_vec() } else { join(home, rest) })
}

/// Python's repr() of a str, for messages.
pub fn py_repr(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::from(q);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

fn parts(p: &[u8]) -> Vec<Vec<u8>> {
    Path::new(OsStr::from_bytes(p)).components().filter(|c| !matches!(c, Component::CurDir))
        .map(|c| c.as_os_str().as_bytes().to_vec()).collect()
}

/// pathlib's == and is_relative_to, part by part.
fn same(a: &[u8], b: &[u8]) -> bool {
    parts(a) == parts(b)
}

fn inside(p: &[u8], dir: &[u8]) -> bool {
    Path::new(OsStr::from_bytes(p)).starts_with(Path::new(OsStr::from_bytes(dir)))
}

/// Python's str.strip(): its whitespace is Unicode's and \x1c-\x1f.
fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c))
}

/// The line breaks Python's str.splitlines() knows ("\r\n" is one).
fn split_lines(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        match rest.find(['\n', '\r', '\x0b', '\x0c', '\x1c', '\x1d', '\x1e', '\u{85}', '\u{2028}', '\u{2029}']) {
            Some(i) => {
                out.push(&rest[..i]);
                let skip = if rest[i..].starts_with("\r\n") { 2 } else { rest[i..].chars().next().map_or(1, char::len_utf8) };
                rest = &rest[i + skip..];
            }
            None => {
                out.push(rest);
                break;
            }
        }
    }
    out
}

impl Policy {
    fn home(&self) -> &[u8] {
        self.home.as_bytes()
    }

    /// zonecfg.expand: a leading "~" is the home.
    fn expand(&self, raw: &[u8]) -> Vec<u8> {
        match raw.strip_prefix(b"~") {
            Some(rest) => py_path(&[self.home(), rest].concat()),
            None => py_path(raw),
        }
    }

    /// The host's additions: a missing file means none; anything wrong with it is an error.
    pub fn host_denied_paths(&self) -> Result<Vec<Vec<u8>>, String> {
        let path: OsString = match &self.denied_file {
            Some(p) => p.clone(),
            None if fs::symlink_metadata(HOST_DENIED_FILE).is_err()
                && fs::symlink_metadata(LEGACY_HOST_DENIED_FILE).is_ok() => LEGACY_HOST_DENIED_FILE.into(),
            None => HOST_DENIED_FILE.into(),
        };
        let shown = path.to_string_lossy().into_owned();
        let text = match fs::read(&path) {
            Ok(b) => String::from_utf8(b).map_err(|_| format!("{shown}: cannot read the host's denied paths: not UTF-8"))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && self.denied_file.is_none() => return Ok(Vec::new()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(format!("{shown} (APPZONES_DENIED_PATHS) does not exist"))
            }
            Err(e) => return Err(format!("{shown}: cannot read the host's denied paths: {e}")),
        };
        let mut out = Vec::new();
        for (n, line) in split_lines(&text).into_iter().enumerate() {
            let p = py_strip(line.split('#').next().unwrap_or(""));
            if p.is_empty() {
                continue;
            }
            if p.contains('\0') {
                return Err(format!("{shown}:{}: {} has a NUL byte", n + 1, py_repr(p)));
            }
            if p == "~" {
                out.push(py_path(self.home()));
            } else if let Some(rest) = p.strip_prefix("~/") {
                out.push(home_join(self.home(), rest.as_bytes()));
            } else if p.starts_with('/') {
                out.push(py_path(p.as_bytes()));
            } else {
                return Err(format!("{shown}:{}: {} must be absolute or start with ~/", n + 1, py_repr(p)));
            }
        }
        Ok(out)
    }

    /// zonecfg.denied_paths, in its order (the first match gives the message).
    pub fn denied_paths(&self) -> Result<Vec<Vec<u8>>, String> {
        let h = py_path(self.home());
        let mut out: Vec<Vec<u8>> = DENIED_SYSTEM.iter().map(|s| s.as_bytes().to_vec()).collect();
        out.extend(DENIED_IN_HOME.iter().map(|r| join(&h, r.as_bytes())));
        out.push(py_path(self.code_dir.as_bytes()));
        out.extend(self.host_denied_paths()?);
        for (dir, prefix) in [(join(&h, b".local/share"), &b"opencode"[..]), (h.clone(), &b".claude"[..])] {
            let p = Path::new(OsStr::from_bytes(&dir));
            match fs::metadata(p) {
                Ok(m) if m.is_dir() => {}
                Ok(_) => continue, // not a directory: nothing to deny there
                Err(e) if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory) => continue,
                Err(e) => return Err(format!("{}: cannot look at it: {e}", show(&dir))),
            }
            let entries = fs::read_dir(p).map_err(|e| format!("{}: cannot list it: {e}", show(&dir)))?;
            for e in entries {
                let e = e.map_err(|e| format!("{}: cannot list it: {e}", show(&dir)))?;
                let name = e.file_name().into_vec();
                if name.starts_with(prefix) {
                    out.push(join(&dir, &name));
                }
            }
        }
        Ok(out)
    }

    /// zonecfg.policy_error: why the resolved path REAL may never enter a zone.
    pub fn policy_error(&self, real: &[u8], raw_b: &[u8]) -> Option<String> {
        let raw = show(raw_b);
        let via = if real != self.expand(raw_b).as_slice() {
            format!(" (resolves to {})", show(real))
        } else {
            String::new()
        };
        let Some(home_real) = realpath(self.home()) else {
            return Some(format!("{raw}: the home's symlinks cannot be resolved"));
        };
        for f in FORBIDDEN_WHOLE {
            let f: &[u8] = if *f == "~" { &home_real } else { f.as_bytes() };
            if same(real, f) {
                return Some(format!("{raw}{via}: binding {} whole is never allowed", show(f)));
            }
        }
        let denied = match self.denied_paths() {
            Ok(d) => d,
            Err(e) => return Some(format!("{raw}: {e}")),
        };
        for d in &denied {
            let Some(dr) = realpath(d) else {
                return Some(format!("{raw}: the symlinks of {}, which never enters a zone, cannot be resolved", show(d)));
            };
            let how = if same(real, &dr) {
                "is"
            } else if inside(&dr, real) {
                "contains"
            } else if inside(real, &dr) {
                "is inside"
            } else {
                continue;
            };
            return Some(format!("{raw}{via}: {how} {}, which never enters a zone", show(d)));
        }
        if !inside(real, &home_real) {
            for d in MOUNT_NEVER_INSIDE {
                if inside(real, d.as_bytes()) {
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
    pub fn bind_error(&self, raw_b: &[u8]) -> Option<String> {
        let raw = show(raw_b);
        let p = self.expand(raw_b);
        if !p.starts_with(b"/") {
            return Some(format!("{raw}: must be absolute or start with ~"));
        }
        let Some(real) = realpath(&p) else {
            return Some(format!("{raw}: its symlinks cannot be resolved (a loop?)"));
        };
        if let Some(e) = self.policy_error(&real, raw_b) {
            return Some(e);
        }
        match fs::metadata(OsStr::from_bytes(&p)) {
            Ok(m) => Self::kind_error(&m, &raw),
            Err(_) => Some(format!("{raw}: does not exist")),
        }
    }

    /// zonecfg.mount_source_error: why a folder may not be mounted into a zone at run time.
    pub fn mount_source_error(&self, raw_b: &[u8]) -> Option<String> {
        let raw = show(raw_b);
        let p = self.expand(raw_b);
        if !p.starts_with(b"/") {
            return Some(format!("{raw}: must be absolute or start with ~"));
        }
        let (Some(real), Some(home_real)) = (realpath(&p), realpath(self.home())) else {
            return Some(format!("{raw}: its symlinks cannot be resolved (a loop?)"));
        };
        if !inside(&real, &home_real) {
            for d in MOUNT_NEVER_INSIDE {
                if same(&real, d.as_bytes()) || inside(&real, d.as_bytes()) {
                    let via = if real != p { format!(" (resolves to {})", show(&real)) } else { String::new() };
                    return Some(format!("{raw}{via}: is inside {d}, which is never mounted into a zone"));
                }
            }
        }
        if let Some(e) = self.bind_error(raw_b) {
            return Some(e);
        }
        if Path::new(OsStr::from_bytes(&p)).is_dir() { None } else { Some(format!("{raw}: not a directory")) }
    }
}

/// A zone name (zonecfg.NAME_RE: ^[a-z][a-z0-9-]{1,20}$, no trailing newline either).
#[allow(dead_code)] // the mount helper's; not every binary uses all
pub fn zone_name_ok(s: &str) -> bool {
    let b = s.as_bytes();
    (2..=21).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b[1..].iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// Directories under the state directory that are not zones (zonecfg.RESERVED).
#[allow(dead_code)] // the mount helper's; not every binary uses all
pub const RESERVED: &[&str] = &["launcher", "share", "bin", "lib", "etc"];

/// A runtime mount's name (zonecfg.MOUNT_NAME_RE: ^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$).
#[allow(dead_code)] // the mount helper's; not every binary uses all
pub fn mount_name_ok(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=64).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b[1..].iter().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of the test's own, made fresh and removed when the test ends.
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(tag: &str) -> TempDir {
            for n in 0..1000 {
                let p = std::env::temp_dir().join(format!("az-{tag}-{}-{n}", std::process::id()));
                if fs::create_dir(&p).is_ok() {
                    return TempDir(fs::canonicalize(&p).unwrap());
                }
            }
            panic!("no temporary directory");
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn s(v: Vec<u8>) -> String {
        String::from_utf8(v).unwrap()
    }

    #[test]
    fn py_paths() {
        assert_eq!(s(py_path(b"/a//b/./c/")), "/a/b/c");
        assert_eq!(s(py_path(b"//etc")), "//etc");
        assert_eq!(s(py_path(b"///etc")), "/etc");
        assert_eq!(s(py_path(b"/")), "/");
        assert_eq!(s(py_path(b"/a/../b")), "/a/../b");
        assert_eq!(s(py_path(b"x")), "x");
        assert_eq!(s(home_join(b"/home/u", b"/srv/private")), "/srv/private");
        assert_eq!(s(home_join(b"/home/u", b"Secrets")), "/home/u/Secrets");
    }

    #[test]
    fn reprs_and_lines() {
        assert_eq!(py_repr("bad entry"), "'bad entry'");
        assert_eq!(py_repr("bad'entry"), "\"bad'entry\"");
        assert_eq!(py_repr("a'b\"c"), "'a\\'b\"c'");
        assert_eq!(py_repr("x\ty"), "'x\\ty'");
        assert_eq!(split_lines("a\r\nb\rc\nd"), vec!["a", "b", "c", "d"]);
        assert_eq!(py_strip(" \x1f~/x\x1c\t"), "~/x");
    }

    #[test]
    fn names() {
        assert!(zone_name_ok("ab") && zone_name_ok("work-2") && !zone_name_ok("a") && !zone_name_ok("Ab"));
        assert!(!zone_name_ok("ab\n") && !zone_name_ok(&"a".repeat(22)) && !zone_name_ok("-ab"));
        assert!(mount_name_ok("dl") && mount_name_ok("My.Folder_1") && !mount_name_ok(".x") && !mount_name_ok("a/b"));
        assert!(!mount_name_ok("") && !mount_name_ok(&"a".repeat(65)) && !mount_name_ok("a\n"));
    }

    #[test]
    fn realpath_resolves_and_refuses_loops() {
        let t = TempDir::new("policy");
        fs::create_dir_all(t.0.join("real/sub")).unwrap();
        std::os::unix::fs::symlink(t.0.join("real"), t.0.join("link")).unwrap();
        std::os::unix::fs::symlink(t.0.join("loop"), t.0.join("loop")).unwrap();
        let tr = t.0.to_string_lossy().into_owned();
        let rp = |p: String| realpath(p.as_bytes()).map(s);
        assert_eq!(rp(format!("{tr}/link/sub")), Some(format!("{tr}/real/sub")));
        assert_eq!(rp(format!("{tr}/link/missing/x")), Some(format!("{tr}/real/missing/x")));
        assert_eq!(rp(format!("{tr}/real/sub/..")), Some(format!("{tr}/real")));
        assert_eq!(rp(format!("{tr}/loop/x")), None);
        let policy = Policy { home: t.0.clone().into(), code_dir: "/nonexistent-code".into(), denied_file: None };
        assert!(policy.mount_source_error(format!("{tr}/loop").as_bytes()).unwrap().contains("cannot be resolved"));
    }

    #[test]
    fn host_file_errors() {
        let t = TempDir::new("denied");
        let f = t.0.join("denied-paths");
        let policy = Policy { home: "/home/u".into(), code_dir: "/c".into(), denied_file: Some(f.clone().into()) };
        fs::write(&f, "~//srv/private\n~/Secrets # mine\n/abs\n").unwrap();
        let got: Vec<String> = policy.host_denied_paths().unwrap().into_iter().map(s).collect();
        assert_eq!(got, vec!["/srv/private", "/home/u/Secrets", "/abs"]);
        fs::write(&f, "~/ok\nbad'entry\n").unwrap();
        assert_eq!(policy.host_denied_paths().unwrap_err(),
                   format!("{}:2: \"bad'entry\" must be absolute or start with ~/", f.display()));
        fs::write(&f, "~/Secrets\0x\n").unwrap();
        assert!(policy.host_denied_paths().unwrap_err().contains("NUL"));
    }
}
