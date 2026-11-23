//! A zone on macOS (design/macos.md, stage 2): its Seatbelt profile (Apple's sandbox
//! language, SBPL, run by `sandbox-exec -f`), its environment, and the settings macOS
//! cannot honour. Plain functions of the zone and the host's paths, so they are tested on
//! every system; plan.rs uses them on macOS only.
//!
//! A zone process may read the system (not the users' homes, /Volumes, the per-user temp
//! and cache folders, /tmp, the never-list) and its own home, tmp and binds; it may write
//! only its home, tmp and `rw` binds. It may signal and look at processes of its own
//! sandbox only, and reach a short list of the system's services. The network is none
//! (unix sockets in the zone's own folders only), the internet through appzones' proxy
//! (one port on localhost, the profile's parameter PROXY_PORT: crate::proxy), or the host's.

use std::path::{Path, PathBuf};

use crate::config::Zone;

/// The system's trees a zone never reads (the real paths: /tmp and /var are links into
/// /private, and /System/Volumes/Data holds the same files as /Users by another path).
pub const NEVER_READ: &[&str] = &["/Users", "/Volumes", "/Network", "/System/Volumes/Data", "/private/var/folders",
    "/private/tmp", "/private/var/tmp", "/private/var/db", "/private/var/root", "/Library/Keychains",
    "/private/etc/ssh", "/nix/var", "/cores"];

/// The system's services every zone may reach (mach-lookup): names, the system log, the
/// notification centre. Never the preferences daemon (every app's settings) or
/// LaunchServices (which opens apps outside the zone).
const SERVICES: &[&str] = &["com.apple.system.opendirectoryd.libinfo", "com.apple.system.notification_center",
    "com.apple.system.logger", "com.apple.logd"];

/// With network = "internet": certificate checks (trustd fetches what it needs itself;
/// TrustEvaluationAgent is how Apple's LibreSSL, in /usr/bin/python3 among others, asks the
/// system's trust store).
const INTERNET_SERVICES: &[&str] = &["com.apple.trustd", "com.apple.trustd.agent", "com.apple.TrustEvaluationAgent"];

/// The profile's parameter that holds the proxy's port (`sandbox-exec -D PROXY_PORT=N`):
/// known only when the zone starts. A profile that names it and is run without it is
/// refused by sandbox-exec (fails closed).
pub const PROXY_PORT: &str = "PROXY_PORT";

/// And with network = "host": names, proxies and certificate checks.
const NET_SERVICES: &[&str] = &["com.apple.dnssd.service", "com.apple.SystemConfiguration.configd",
    "com.apple.SystemConfiguration.DNSConfiguration", "com.apple.trustd", "com.apple.trustd.agent",
    "com.apple.TrustEvaluationAgent",
    "com.apple.networkd", "com.apple.nehelper"];

/// sysctls that tell this machine (or this boot of it) from another.
const SYSCTL_NEVER: &[&str] = &["kern.uuid", "kern.bootuuid", "kern.apfsprebootuuid", "kern.bootsessionuuid"];
/// And the IOKit properties that do (the hardware's UUID, serial numbers, network addresses).
/// (gethostuuid(2) still answers in a sandbox: no profile rule reaches it. seatbelt::less.)
const IOKIT_NEVER: &[&str] = &["IOPlatformUUID", "IOPlatformSerialNumber", "serial-number", "mlb-serial-number",
    "IOMACAddress", "board-id"];

/// What a profile is made from: real paths (symlinks resolved), checked already.
pub struct Profile<'a> {
    /// the user's own home (resolved), never read whatever its path
    pub user_home: &'a Path,
    pub home: &'a Path,
    pub tmp: &'a Path,
    /// (path, writable)
    pub binds: &'a [(PathBuf, bool)],
    pub network: &'a str,
    /// more trees never read: the never-list's entries outside the homes, resolved
    pub never: &'a [PathBuf],
    /// the zone's marks (crate::sys::zone_pids): a file this profile alone may read, and
    /// one beside it that it alone refuses by name
    pub marks: (&'a Path, &'a Path),
}

/// PATH as an SBPL string: in quotes, with \ and " escaped; never a path with a control
/// character (a newline would end the line it is on, and nothing needs one).
pub fn quote(p: &Path) -> Result<String, String> {
    let s = p.to_str().ok_or_else(|| format!("{}: not UTF-8, which a Seatbelt profile cannot hold", p.display()))?;
    if s.chars().any(char::is_control) {
        return Err(format!("{}: has a control character, which a Seatbelt profile cannot hold", s.escape_debug()));
    }
    Ok(format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")))
}

/// Every parent of PATH, from / down (their metadata is read on the way: realpath, getcwd).
fn parents(p: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = p.ancestors().skip(1).map(Path::to_path_buf).collect();
    out.reverse();
    out
}

/// The zone's profile.
pub fn profile(p: &Profile) -> Result<String, String> {
    let q = quote;
    let mut s = String::from(
        "(version 1)\n\
         (deny default)\n\
         ; processes: its own, and only those of this sandbox\n\
         (allow process-exec process-fork)\n\
         (allow signal (target same-sandbox))\n\
         (allow process-info* (target same-sandbox))\n\
         (allow sysctl-read)\n");
    s += &format!("(deny sysctl-read {})\n",
                  SYSCTL_NEVER.iter().map(|n| format!("(sysctl-name \"{n}\")")).collect::<Vec<_>>().join(" "));
    s += &format!("(deny iokit-get-properties {})\n",
                  IOKIT_NEVER.iter().map(|n| format!("(iokit-property \"{n}\")")).collect::<Vec<_>>().join(" "));
    s += "; the system, read-only: not the homes, the temp folders, the never-list\n\
          (allow file-read* file-map-executable)\n";
    let mut never: Vec<String> = NEVER_READ.iter().map(|n| format!("(subpath \"{n}\")")).collect();
    never.push(format!("(subpath {})", q(p.user_home)?)); // (also a home outside /Users)
    for n in p.never {
        never.push(format!("(subpath {})", q(n)?));
    }
    s += &format!("(deny file-read* file-map-executable\n  {})\n", never.join("\n  "));
    // the way down to the zone's folders: their parents' metadata (not their listing)
    let mut ways: Vec<PathBuf> = Vec::new();
    for d in [p.home, p.tmp].into_iter().chain(p.binds.iter().map(|(b, _)| b.as_path())) {
        for a in parents(d) {
            if !ways.contains(&a) {
                ways.push(a);
            }
        }
    }
    s += "; the way to the zone's folders\n";
    s += &format!("(allow file-read-metadata\n  {})\n",
                  ways.iter().map(|w| q(w).map(|w| format!("(literal {w})"))).collect::<Result<Vec<_>, _>>()?.join("\n  "));
    // the zone's folders and binds, the shallowest first: in SBPL the last matching rule
    // wins, so a folder inside another gets its own mode (an ro bind inside an rw one, or
    // inside the zone's home, is read-only)
    s += "; the zone's own folders, and its binds (deeper ones after: theirs is the last word)\n";
    let mut areas: Vec<(&Path, bool)> = vec![(p.home, true), (p.tmp, true)];
    areas.extend(p.binds.iter().map(|(b, rw)| (b.as_path(), *rw)));
    areas.sort_by_key(|(a, _)| a.components().count()); // (stable: the same depth keeps its order)
    for (a, rw) in areas {
        s += &if rw {
            format!("(allow file-read* file-write* (subpath {}))\n", q(a)?)
        } else {
            format!("(allow file-read* (subpath {0}))\n(deny file-write* (subpath {0}))\n", q(a)?)
        };
    }
    // last, so no bind's rule can override them
    s += &format!("; the zone's marks, how its processes are told from others'\n(allow file-read-data (literal {}))\n\
                   (deny file-read* file-write* (literal {}))\n(deny file-write* (literal {}))\n",
                  q(p.marks.0)?, q(p.marks.1)?, q(p.marks.0)?);
    s += "; devices: the harmless ones, its own terminal (never another's: no new ptys either)\n\
          (allow file-read* file-write-data (literal \"/dev/null\") (literal \"/dev/zero\") (literal \"/dev/dtracehelper\"))\n\
          (allow file-read* (literal \"/dev/random\") (literal \"/dev/urandom\"))\n\
          (allow file-read* file-write-data file-ioctl (literal \"/dev/tty\"))\n\
          (allow file-ioctl (regex #\"^/dev/ttys[0-9]+$\"))\n";
    let mut services: Vec<&str> = SERVICES.to_vec();
    if p.network == "host" {
        services.extend(NET_SERVICES);
    } else if p.network == "internet" {
        services.extend(INTERNET_SERVICES);
    }
    s += "; the system's services it may reach\n";
    s += &format!("(allow mach-lookup\n  {})\n",
                  services.iter().map(|n| format!("(global-name \"{n}\")")).collect::<Vec<_>>().join("\n  "));
    s += "(allow ipc-posix-shm-read-data (ipc-posix-name \"apple.shm.notification_center\"))\n";
    match p.network {
        "host" => s += "; network = \"host\": the host's network, all of it\n\
                        (allow network* system-socket)\n",
        "internet" => {
            // the proxy's port on localhost and nothing else; unix sockets as with none
            let own = format!("(path-prefix {}) (path-prefix {})", q(p.home)?, q(p.tmp)?);
            s += "; network = \"internet\": appzones' proxy (one port on localhost), unix sockets in the zone's own folders\n";
            s += "(allow system-socket (socket-domain AF_UNIX) (socket-domain AF_INET) (socket-domain AF_INET6))\n";
            s += &format!("(allow network-outbound (remote ip (string-append \"localhost:\" (param \"{PROXY_PORT}\"))))\n");
            s += &format!("(allow network-bind network-inbound (local unix-socket {own}))\n");
            s += &format!("(allow network-outbound (remote unix-socket {own}))\n");
        }
        _ => {
            // unix sockets of its own (a language server, an agent's helper), nothing else
            let own = format!("(subpath {}) (subpath {})", q(p.home)?, q(p.tmp)?);
            s += "; network = \"none\": unix sockets in the zone's own folders only\n";
            s += "(allow system-socket (socket-domain AF_UNIX))\n";
            s += &format!("(allow network-bind network-inbound (local unix-socket {}))\n",
                          own.replace("(subpath ", "(path-prefix "));
            s += &format!("(allow network-outbound (remote unix-socket {}))\n", own.replace("(subpath ", "(path-prefix "));
        }
    }
    Ok(s)
}

/// The zone's settings macOS cannot honour, as errors: each one would give the zone more
/// than asked, or something appzones cannot do there (yet). Defaults never are errors.
pub fn unsupported(z: &Zone) -> Vec<String> {
    let st = |k: &str| z.get(k);
    let is = |k: &str, v: &str| st(k).as_str() == Some(v);
    let list = |k: &str| st(k).as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect::<Vec<_>>())
        .unwrap_or_default();
    let mut e = Vec::new();
    let p = format!("zone {}:", z.name);
    if crate::config::truthy(st("netbird")) {
        e.push(format!("{p} netbird = true: not on macOS"));
    }
    if is("dns", "public") {
        e.push(format!("{p} dns = \"public\": not on macOS yet (the host's resolver answers)"));
    }
    if st("links").as_str().is_some_and(|l| l != "portal") {
        e.push(format!("{p} links = {:?}: not on macOS yet", st("links").as_str().unwrap_or("")));
    }
    if is("keyring", "zone") {
        e.push(format!("{p} keyring = \"zone\": not on macOS"));
    }
    let more: Vec<String> = list("devices").into_iter().filter(|d| !["gpu", "audio"].contains(&d.as_str())).collect();
    if !more.is_empty() {
        e.push(format!("{p} devices {}: not on macOS yet (gpu and audio are not given either)", more.join(", ")));
    }
    if !list("usb").is_empty() {
        e.push(format!("{p} usb: not on macOS"));
    }
    if is("wayland", "host") {
        e.push(format!("{p} wayland = \"host\": no Wayland on macOS"));
    }
    if is("x11", "own") {
        e.push(format!("{p} x11 = \"own\": no X11 on macOS"));
    }
    if crate::config::truthy(st("autostart")) {
        e.push(format!("{p} autostart = true: not on macOS yet (a LaunchAgent, later)"));
    }
    if !list("packages").is_empty() {
        e.push(format!("{p} packages: not on macOS yet"));
    }
    let dbus = |k: &str| st("dbus").get(k).and_then(|v| v.as_array()).is_some_and(|a| !a.is_empty());
    if dbus("talk") || dbus("own") {
        e.push(format!("{p} dbus: no D-Bus on macOS"));
    }
    if crate::config::truthy(st("nix_ld")) {
        e.push(format!("{p} nix_ld = true: not on macOS"));
    }
    if is("supervisor", "systemd") {
        e.push(format!("{p} supervisor = \"systemd\": no systemd on macOS (\"auto\" or \"spawn\")"));
    }
    e
}

/// What the zone gets less of on macOS (said in its log at the start, never an error).
pub fn less(z: &Zone) -> Vec<String> {
    let mut out = Vec::new();
    let devices: Vec<&str> = z.get("devices").as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect()).unwrap_or_default();
    if !devices.is_empty() {
        out.push(format!("devices {}: not given to zones on macOS yet", devices.join(", ")));
    }
    if z.get("network").as_str() == Some("internet") {
        out.push("network = \"internet\": through appzones' proxy (HTTPS_PROXY, HTTP_PROXY, ALL_PROXY); programs \
                  that do not use it get no network, and names resolve only through it".into());
    }
    if z.get("machine_id").as_str() == Some("zone") {
        out.push("machine_id = \"zone\": on macOS the zone can still read the Mac's hardware UUID (gethostuuid), \
                  not its serial numbers".into());
    }
    out
}

/// The zone's environment: (name, value), as zone-init passes it to every app.
pub struct Env<'a> {
    pub zone: &'a str,
    pub user: &'a str,
    pub home: &'a Path,
    pub tmp: &'a Path,
    pub host_home: &'a Path,
}

/// The directories on a zone's PATH that exist here (nix-darwin's, Homebrew's, the system's).
pub fn path_dirs(user: &str) -> Vec<String> {
    [format!("/etc/profiles/per-user/{user}/bin"), "/run/current-system/sw/bin".into(), "/opt/homebrew/bin".into(),
     "/opt/homebrew/sbin".into(), "/usr/local/bin".into(), "/usr/bin".into(), "/bin".into(), "/usr/sbin".into(),
     "/sbin".into()]
        .into_iter().filter(|d| Path::new(d).is_dir()).collect()
}

pub fn environment(e: &Env, z: &Zone, path: &[String], host: impl Fn(&str) -> Option<String>) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = Vec::new();
    let mut set = |k: &str, v: String| {
        env.retain(|(n, _)| n != k);
        env.push((k.into(), v));
    };
    set("HOME", e.home.to_string_lossy().into_owned());
    set("USER", e.user.into());
    set("LOGNAME", e.user.into());
    set("SHELL", "/bin/zsh".into());
    set("PATH", path.join(":"));
    set("TMPDIR", format!("{}/", e.tmp.to_string_lossy()));
    set("LANG", host("LANG").filter(|l| !l.is_empty()).unwrap_or_else(|| "en_US.UTF-8".into()));
    set("TERM", "xterm-256color".into());
    set("ZONE", e.zone.into());
    // certificates for the Nix tools, when the file is the system's (never one in the home)
    for k in ["NIX_SSL_CERT_FILE", "SSL_CERT_FILE"] {
        if let Some(v) = host(k).filter(|v| v.starts_with('/') && !Path::new(v).starts_with(e.host_home)) {
            set(k, v);
        }
    }
    for k in ["LC_ALL", "LC_CTYPE", "TZ"] {
        if let Some(v) = host(k).filter(|v| !v.is_empty()) {
            set(k, v);
        }
    }
    if let Some(t) = z.get("env").as_table() {
        for (k, v) in t {
            set(k, v.as_str().unwrap_or("").to_string());
        }
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zone(toml: &str) -> crate::config::Config {
        let cfg = crate::config::load_text(&format!("version = 1\n[zones.dev]\ncolour = \"#336699\"\n{toml}\n"), "zones.toml", "/nonexistent".into());
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        cfg
    }

    #[test]
    fn quoting() {
        assert_eq!(quote(Path::new("/a b/c\"d\\e")).unwrap(), r#""/a b/c\"d\\e""#);
        assert!(quote(Path::new("/a\nb")).unwrap_err().contains("control character"));
    }

    #[test]
    fn profiles() {
        let binds = vec![(PathBuf::from("/Users/u/src"), true), (PathBuf::from("/Users/u/docs"), false)];
        let never = vec![PathBuf::from("/srv/secret")];
        let p = Profile { user_home: Path::new("/Users/u"), home: Path::new("/Users/u/.local/share/appzones/dev/home"),
                          tmp: Path::new("/Users/u/.local/share/appzones/dev/tmp"), binds: &binds, network: "none",
                          never: &never, marks: (Path::new("/z/seatbelt-ab.yes"), Path::new("/z/seatbelt-ab.no")) };
        let s = profile(&p).unwrap();
        assert!(s.contains("(allow file-read-data (literal \"/z/seatbelt-ab.yes\"))"));
        assert!(s.contains("(literal \"/z/seatbelt-ab.no\")"));
        assert!(s.starts_with("(version 1)\n(deny default)\n"));
        assert!(s.contains("(allow file-read* file-write* (subpath \"/Users/u/src\"))"));
        assert!(s.contains("(allow file-read* (subpath \"/Users/u/docs\"))"));
        assert!(s.contains("(subpath \"/srv/secret\")") && s.contains("(subpath \"/Users\")"));
        assert!(s.contains("(literal \"/Users/u/.local/share/appzones\")") && s.contains("(literal \"/\")"));
        assert!(!s.contains("(allow network*") && !s.contains("com.apple.dnssd.service"));
        assert!(s.contains("(local unix-socket (path-prefix \"/Users/u/.local/share/appzones/dev/home\")"));
        // the binds come after the never-read list: in SBPL the last matching rule wins
        assert!(s.find("(subpath \"/Users\")").unwrap() < s.find("(subpath \"/Users/u/src\")").unwrap());
        assert!(s.contains("(subpath \"/Users/u\")")); // the user's home, by its own path too
        // an ro bind inside an rw one: read-only, after it
        let nested = vec![(PathBuf::from("/w/src/vendor"), false), (PathBuf::from("/w/src"), true)];
        let s = profile(&Profile { binds: &nested, ..p }).unwrap();
        let (rw, ro) = (s.find("(allow file-read* file-write* (subpath \"/w/src\"))").unwrap(),
                        s.find("(deny file-write* (subpath \"/w/src/vendor\"))").unwrap());
        assert!(rw < ro);
        // the marks after every bind
        assert!(s.rfind("(deny file-read* file-write* (literal \"/z/seatbelt-ab.no\"))").unwrap() > ro);
        let host = profile(&Profile { network: "host", ..p }).unwrap();
        assert!(host.contains("(allow network* system-socket)") && host.contains("com.apple.dnssd.service"));
        // internet: the proxy's port alone, by the parameter; no names, no other network
        let inet = profile(&Profile { network: "internet", ..p }).unwrap();
        assert!(inet.contains("(allow network-outbound (remote ip (string-append \"localhost:\" (param \"PROXY_PORT\"))))"));
        assert!(!inet.contains("(allow network*") && !inet.contains("com.apple.dnssd.service") && inet.contains("com.apple.trustd"));
    }

    #[test]
    fn what_macos_refuses() {
        let cfg = zone("network = \"none\"");
        assert!(unsupported(cfg.zone("dev").unwrap()).is_empty(), "the defaults pass");
        let l = less(cfg.zone("dev").unwrap());
        assert!(l.len() == 2 && l[0] == "devices gpu, audio: not given to zones on macOS yet" && l[1].contains("gethostuuid"), "{l:?}");
        let cfg = zone("network = \"internet\"\ndevices = [\"camera\"]\ndns = \"public\"");
        let z = cfg.zone("dev").unwrap();
        let e = unsupported(z);
        assert_eq!(e.len(), 2, "{e:?}"); // (internet itself is fine: through the proxy)
        assert!(e[0].contains("dns = \"public\"") && e[1].contains("devices camera"));
        assert!(less(z).iter().any(|l| l.contains("through appzones' proxy")));
    }

    #[test]
    fn environments() {
        let cfg = zone("network = \"none\"\nenv = { EDITOR = \"vi\", LANG = \"de_DE.UTF-8\" }");
        let e = Env { zone: "dev", user: "u", home: Path::new("/z/home"), tmp: Path::new("/z/tmp"), host_home: Path::new("/Users/u") };
        let host = |k: &str| match k {
            "NIX_SSL_CERT_FILE" => Some("/etc/ssl/certs/ca-certificates.crt".to_string()),
            "SSL_CERT_FILE" => Some("/Users/u/certs.pem".to_string()), // in the home: not passed
            "LANG" => Some("en_GB.UTF-8".to_string()),
            _ => None,
        };
        let env = environment(&e, cfg.zone("dev").unwrap(), &["/usr/bin".into(), "/bin".into()], host);
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("HOME"), Some("/z/home"));
        assert_eq!(get("TMPDIR"), Some("/z/tmp/"));
        assert_eq!(get("PATH"), Some("/usr/bin:/bin"));
        assert_eq!(get("LANG"), Some("de_DE.UTF-8")); // the zone's own env wins
        assert_eq!(get("NIX_SSL_CERT_FILE"), Some("/etc/ssl/certs/ca-certificates.crt"));
        assert_eq!(get("SSL_CERT_FILE"), None);
        assert_eq!(get("EDITOR"), Some("vi"));
    }
}
