//! `appzones` (appzones-helper cli) from the outside: its messages, exit codes and files,
//! in a temporary home with no zone running. What needs a zone running is tests/vm.nix's.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const HELPER: &str = env!("CARGO_BIN_EXE_appzones-helper");
const TWO: &str = "version = 1\n[zones.aa]\ncolour = \"#111111\"\nnetwork = \"none\"\n\
                   [zones.bb]\ncolour = \"#222222\"\nnetwork = \"none\"\n";

/// A home, a runtime directory, a config and an empty PATH of fake tools, all temporary.
struct Box_ {
    t: PathBuf,
}

impl Box_ {
    fn new(tag: &str, config: Option<&str>) -> Box_ {
        let t = std::env::temp_dir().join(format!("az-clitest-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        for d in ["home", "rt", "bin"] {
            fs::create_dir_all(t.join(d)).unwrap();
        }
        fs::set_permissions(t.join("rt"), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(t.join("denied-paths"), "").unwrap();
        if let Some(c) = config {
            fs::write(t.join("zones.toml"), c).unwrap();
        }
        let t = fs::canonicalize(&t).unwrap();
        Box_ { t }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = self.raw(&["cli"]);
        c.args(args);
        c
    }

    /// appzones-helper ARGS (not its cli) in the box's environment.
    fn raw(&self, args: &[&str]) -> Command {
        let mut c = Command::new(HELPER);
        c.args(args).env_clear().stdin(Stdio::null())
            .env("HOME", self.t.join("home")).env("XDG_RUNTIME_DIR", self.t.join("rt"))
            .env("PATH", self.t.join("bin")).env("USER", "alice")
            .env("APPZONES_CONFIG", self.t.join("zones.toml"))
            .env("APPZONES_DENIED_PATHS", self.t.join("denied-paths"))
            .env("APPZONES_DBUS_SERVICE_DIRS", "").env("APPZONES_SUPERVISOR", "spawn");
        c
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        out(self.cmd(args).output().unwrap())
    }

    /// A fake tool on PATH: a shell script (the real /bin/sh runs it).
    fn tool(&self, name: &str, body: &str) {
        let f = self.t.join("bin").join(name);
        fs::write(&f, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&f, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn logs(&self) -> PathBuf {
        self.t.join("home/.local/state/appzones")
    }
}

impl Drop for Box_ {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.t);
    }
}

fn out(o: Output) -> (i32, String, String) {
    (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into_owned(),
     String::from_utf8_lossy(&o.stderr).into_owned())
}

#[test]
fn unknown_command_suggests_the_nearest() {
    let b = Box_::new("unknown", Some(TWO));
    assert_eq!(b.run(&["lsit"]), (2, String::new(),
               "appzones: unknown command 'lsit' (did you mean 'list'?); see appzones --help\n".into()));
    assert_eq!(b.run(&["zzzzzz"]), (2, String::new(), "appzones: unknown command 'zzzzzz'; see appzones --help\n".into()));
    let (rc, _, err) = b.run(&["netsetup"]);
    assert_eq!(rc, 2);
    assert!(!err.contains("_netsetup"), "an internal command suggested: {err}");
}

#[test]
fn ps_on_a_stopped_zone_is_one_line() {
    let b = Box_::new("ps", Some(TWO));
    assert_eq!(b.run(&["ps", "nosuchzone"]),
               (1, String::new(), "appzones: zone nosuchzone is not running (start it: appzones up nosuchzone)\n".into()));
}

#[test]
fn version_and_help() {
    let b = Box_::new("version", None);
    for flag in ["--version", "-V"] {
        let (rc, so, _) = b.run(&[flag]);
        assert_eq!(rc, 0);
        assert!(so.starts_with(&format!("appzones {} (config release ", env!("CARGO_PKG_VERSION"))), "{so}");
    }
    let (rc, so, _) = b.run(&[]);
    assert_eq!(rc, 2);
    assert!(so.starts_with("appzones"), "{so}");
    let (rc, so2, _) = b.run(&["--help"]);
    assert_eq!((rc, so2), (0, so));
}

#[test]
#[cfg(target_os = "linux")] // (starts or serves a zone: Linux so far)
fn serve_arguments() {
    let b = Box_::new("serve-args", Some(TWO));
    let (rc, _, err) = b.run(&["serve", "aa", "--notify-fd", "9", "--lock-fd", "9"]);
    assert_eq!(rc, 1);
    assert!(err.contains("must be different"), "{err}");
    for bad in [&["serve"][..], &["serve", "aa", "--notify-fd"], &["serve", "aa", "--notify-fd", "x"],
                &["serve", "aa", "--notify-fd=\u{b2}"], &["serve", "aa", "--notify-fd", "99999999999999999999"],
                &["serve", "aa", "--other"], &["serve", "aa", "--notify-fd", "1"], &["serve", "../x"]] {
        let (rc, _, err) = b.run(bad);
        assert_ne!(rc, 0, "{bad:?}");
        assert!(err.starts_with("appzones: "), "{bad:?}: {err}");
    }
    // a readiness fd that is not open: refused before anything starts
    let (rc, _, err) = b.run(&["serve", "aa", "--notify-fd", "57"]);
    assert_eq!(rc, 1);
    assert!(err.contains("--notify-fd 57"), "{err}");
    assert!(!b.logs().exists(), "nothing started");
}

/// PATH locked (flock) here, as a server holds it, until the file is dropped: a lock is the
/// open file's, so the command under test, which opens it again, finds it taken.
fn hold_lock(path: &Path) -> fs::File {
    let f = fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path).unwrap();
    // Safety: flock on our own file.
    assert_eq!(unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&f), libc::LOCK_EX | libc::LOCK_NB) }, 0);
    f
}

#[test]
#[cfg(target_os = "linux")] // (starts or serves a zone: Linux so far)
fn one_server_per_zone() {
    let b = Box_::new("lock", Some(TWO));
    let rt = b.t.join("rt/appzones");
    fs::create_dir_all(&rt).unwrap();
    // held by another: refused with 75, "already served"
    let holder = hold_lock(&rt.join("aa.lock"));
    let (rc, _, err) = b.run(&["serve", "aa"]);
    drop(holder);
    assert_eq!(rc, 75, "{err}");
    assert!(err.contains("zone aa is already served"), "{err}");
    // a --lock-fd that is not the zone's lock
    let other = b.t.join("other");
    fs::write(&other, "").unwrap();
    let c = std::ffi::CString::new(other.to_str().unwrap()).unwrap();
    // Safety: an fd opened without O_CLOEXEC, so the child inherits it; closed below.
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDWR) };
    assert!(fd > 2);
    let (rc, _, err) = b.run(&["serve", "bb", "--lock-fd", &fd.to_string()]);
    // Safety: our own fd.
    unsafe { libc::close(fd) };
    assert_eq!(rc, 1);
    assert!(err.contains("not zone bb's lock"), "{err}");
    // an older server without a lock still answers: refused
    let zrt = rt.join("aa");
    fs::create_dir_all(&zrt).unwrap();
    let l = std::os::unix::net::UnixListener::bind(zrt.join("agent.sock")).unwrap();
    let answer = std::thread::spawn(move || {
        use std::io::{BufRead, Write};
        let (mut c, _) = l.accept().unwrap();
        let mut line = String::new();
        std::io::BufReader::new(c.try_clone().unwrap()).read_line(&mut line).unwrap();
        c.write_all(b"{\"ok\": true}\n").unwrap();
    });
    let (rc, _, err) = b.run(&["serve", "aa"]);
    answer.join().unwrap();
    assert_eq!(rc, 75, "{err}");
    assert!(err.contains("without a lock"), "{err}");
    // a lock the server makes itself: 0600 (this one fails later, with no `ip` on PATH)
    let (rc, _, err) = b.run(&["serve", "cc"]);
    assert_eq!(rc, 1, "{err}");
    assert_eq!(fs::metadata(rt.join("cc.lock")).unwrap().permissions().mode() & 0o777, 0o600);
}

#[test]
#[cfg(target_os = "linux")] // (starts or serves a zone: Linux so far)
fn supervisor_setting() {
    let b = Box_::new("supervisor", Some(TWO));
    let (rc, _, err) = b.cmd(&["up", "aa"]).env("APPZONES_SUPERVISOR", "runit").output().map(out).unwrap();
    assert_eq!(rc, 1);
    assert!(err.contains("use one of auto, systemd, spawn"), "{err}");
}

#[test]
#[cfg(target_os = "linux")] // (starts or serves a zone: Linux so far)
fn a_failed_start_shows_its_log() {
    // no `ip` on PATH: the server fails while it plans the zone
    let b = Box_::new("failed", Some(TWO));
    let (rc, _, err) = b.run(&["up", "aa"]);
    assert_eq!(rc, 1, "{err}");
    assert!(err.contains("zone aa did not start; the end of its log"), "{err}");
    assert!(err.contains("ip -j addr show"), "{err}");
    let log = b.logs().join("aa.log");
    assert_eq!(fs::metadata(&log).unwrap().permissions().mode() & 0o777, 0o600);
    // a log over its size is rotated first, and the tail is this attempt's alone
    let max = 2 * 1024 * 1024;
    fs::write(&log, "old\n".repeat(max / 4 + 1)).unwrap();
    let (rc, _, err) = b.run(&["up", "aa"]);
    assert_eq!(rc, 1);
    assert!(err.contains("ip -j addr show") && !err.contains("old"), "{err}");
    assert_eq!(fs::metadata(b.logs().join("aa.log.1")).unwrap().len() as usize, (max / 4 + 1) * 4);
    // a log that cannot be opened: said so
    fs::create_dir_all(b.logs().join("bb.log")).unwrap();
    let (rc, _, err) = b.run(&["up", "bb"]);
    assert_eq!(rc, 1);
    assert!(err.contains("cannot open its log"), "{err}");
}

#[test]
#[cfg(target_os = "linux")] // (starts or serves a zone: Linux so far)
fn each_zone() {
    let b = Box_::new("each", Some(TWO));
    // one zone that fails does not keep the others from being tried; the result says so
    let (rc, _, err) = b.run(&["up", "--all"]);
    assert_eq!(rc, 1, "{err}");
    assert!(err.contains("zone aa did not start") && err.contains("zone bb did not start"), "{err}");
    // stop --all skips the zones that do not answer
    let (rc, so, err) = b.run(&["stop", "--all"]);
    assert_eq!((rc, so.as_str()), (0, ""), "{err}");
    let (rc, _, err) = b.run(&["up"]);
    assert_eq!(rc, 1);
    assert!(err.contains("usage: appzones up ZONE|--all"), "{err}");
}

#[test]
fn new_zone() {
    let b = Box_::new("new", None);
    let cfg = b.t.join("zones.toml");
    let (rc, so, err) = b.run(&["new", "games"]);
    assert_eq!(rc, 0);
    assert!(so.contains("[zones.games]") && err.contains("not written"), "{so}{err}");
    assert!(!cfg.exists(), "a preview writes nothing");
    assert_eq!(b.run(&["new", "games", "-y"]).0, 0);
    let (rc, so, err) = b.run(&["which", "games"]);
    assert_eq!(rc, 0, "{err}");
    assert!(so.contains("network") && so.contains("\"internet\""), "{so}");
    // three at once: every zone ends up in the file, each with its own colour
    let kids: Vec<_> = ["one", "two", "three"].iter().map(|n| b.cmd(&["new", n, "-y"]).stdout(Stdio::null())
        .stderr(Stdio::piped()).spawn().unwrap()).collect();
    for k in kids {
        let o = k.wait_with_output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }
    let (rc, so, err) = b.run(&["list", "--json"]);
    assert_eq!(rc, 0, "{err}");
    let zones: serde_json::Value = serde_json::from_str(&so).unwrap();
    let mut names: Vec<&str> = zones.as_array().unwrap().iter().map(|z| z["name"].as_str().unwrap()).collect();
    assert_eq!(names[0], "games");
    names.sort();
    assert_eq!(names, ["games", "one", "three", "two"]);
    let mut colours: Vec<&str> = zones.as_array().unwrap().iter().map(|z| z["colour"].as_str().unwrap()).collect();
    colours.sort();
    colours.dedup();
    assert_eq!(colours.len(), 4, "{colours:?}");
    // an existing name: refused
    let (rc, _, err) = b.run(&["new", "games", "-y"]);
    assert_ne!(rc, 0);
    assert!(err.contains("games"), "{err}");
}

#[test]
fn list_json() {
    let b = Box_::new("list", Some("version = 1\n[zones.daily]\ncolour = \"#a6e3a1\"\nnetwork = \"internet\"\n\
                                    [zones.work]\ncolour = \"#f38ba8\"\nnetwork = \"none\"\n"));
    let (rc, so, err) = b.run(&["list", "--json"]);
    assert_eq!(rc, 0, "{err}");
    let data: serde_json::Value = serde_json::from_str(&so).unwrap();
    assert_eq!(data, serde_json::json!([
        {"name": "daily", "state": "stopped", "network": "internet", "dns": "host", "devices": ["gpu", "audio"],
         "apps": null, "colour": "#a6e3a1"},
        {"name": "work", "state": "stopped", "network": "none", "dns": "host", "devices": ["gpu", "audio"],
         "apps": null, "colour": "#f38ba8"}]));
    // Python's JSON: ", " and ": " between items
    assert!(so.starts_with("[{\"name\": \"daily\", \"state\": "), "{so}");
    let (rc, table, _) = b.run(&["list"]);
    assert_eq!(rc, 0);
    let first = table.lines().next().unwrap();
    assert!(first.starts_with("ZONE") && first.contains("STATE") && first.contains("COLOUR"), "{table}");
    assert!(table.contains("stopped"), "{table}");
}

#[test]
fn which_json() {
    let b = Box_::new("which", Some("version = 1\n[defaults]\ndns = \"public\"\n[zones.work]\ncolour = \"#f38ba8\"\nnetwork = \"internet\"\n"));
    let (rc, so, err) = b.run(&["which", "work", "--json"]);
    assert_eq!(rc, 0, "{err}");
    assert!(so.starts_with("{\"name\": \"work\", \"settings\": {"), "{so}");
    let data: serde_json::Value = serde_json::from_str(&so).unwrap();
    assert_eq!(data["name"], "work");
    assert_eq!(data["settings"]["network"], serde_json::json!({"value": "internet", "from": "zone"}));
    assert_eq!(data["settings"]["dns"], serde_json::json!({"value": "public", "from": "defaults"}));
    assert_eq!(data["settings"]["userns"], serde_json::json!({"value": true, "from": "builtin"}));

    let (rc2, plain, _) = b.run(&["which", "work"]);
    assert_eq!(rc2, 0);
    let text_keys: Vec<&str> = plain.lines().map(|l| l.split_whitespace().next().unwrap()).collect();
    let json_keys: Vec<&str> = data["settings"].as_object().unwrap().keys().map(|k| k.as_str()).collect();
    assert_eq!(text_keys, json_keys);

    let (rc3, so3, _) = b.run(&["which", "--json", "work"]);
    assert_eq!(rc3, 0);
    assert_eq!(so, so3);
}

#[test]
fn what_is_left_from_before_0_6_is_named_never_read() {
    // the pre-0.6 folders and variables: a warning for each, with what to do; nothing is
    // read or moved
    let b = Box_::new("old", Some(TWO));
    let old_state = b.t.join("home/.local/share/zones");
    fs::create_dir_all(old_state.join("ta/home")).unwrap();
    fs::create_dir_all(b.t.join("home/.config/zones")).unwrap();
    fs::write(b.t.join("home/.config/zones/zones.toml"), "version = 1\n").unwrap();
    let (rc, _, err) = b.cmd(&["list"]).env("ZONES_STATE", "/nonexistent").output().map(out).unwrap();
    assert_eq!(rc, 0, "{err}");
    assert!(err.contains("$ZONES_STATE is from before appzones 0.6 and is not read: set $APPZONES_STATE instead"), "{err}");
    assert!(err.contains(&format!("{} (zone homes from before appzones 0.6) is not used: appzones stop --all && mv ",
                                  old_state.display())), "{err}");
    assert!(!err.contains(".config/zones"), "{err}"); // ($APPZONES_CONFIG is set: the old file is not looked at)
    assert!(old_state.join("ta/home").is_dir());
    // at the default config path: the old file is named, never read
    let (_, _, err) = b.cmd(&["check"]).env_remove("APPZONES_CONFIG").output().map(out).unwrap();
    assert!(err.contains("/home/.config/zones/zones.toml is from before appzones 0.6 and is not read: mkdir -p "), "{err}");
    // the new folder there too: the old one is to be emptied by hand
    fs::create_dir_all(b.t.join("home/.local/share/appzones")).unwrap();
    let (_, _, err) = b.run(&["list"]);
    assert!(err.contains("is not used (") && err.contains("move what you still need from it, then remove it"), "{err}");
    // a typo warns about nothing
    let (_, _, err) = b.run(&["lsit"]);
    assert!(!err.contains("before appzones 0.6"), "{err}");
}

#[test]
fn logs_follow_who_runs_the_zone() {
    // no unit, no server: the log file (tail); systemd's choice or a running unit: the journal
    let b = Box_::new("logs", Some(TWO));
    fs::create_dir_all(b.logs()).unwrap();
    fs::write(b.logs().join("aa.log"), "a line\n").unwrap();
    b.tool("tail", "echo tail \"$@\"");
    b.tool("journalctl", "echo journalctl \"$@\"");
    for (systemctl, supervisor, want) in [("echo inactive", "spawn", "tail"), ("echo inactive", "systemd", "journalctl"),
                                          ("echo active", "spawn", "journalctl"), ("exit 1", "spawn", "tail")] {
        b.tool("systemctl", systemctl);
        let (rc, so, err) = b.cmd(&["logs", "aa"]).env("APPZONES_SUPERVISOR", supervisor).output().map(out).unwrap();
        assert_eq!(rc, 0, "{err}");
        assert!(so.starts_with(want), "{systemctl} {supervisor}: {so}");
    }
    // the zone's own supervisor beats $APPZONES_SUPERVISOR (unless it is auto)
    fs::write(b.t.join("zones.toml"), format!("{}supervisor = \"spawn\"\n", TWO.replace("version = 1\n", "version = 1\nrequires = 6\n")))
        .unwrap();
    b.tool("systemctl", "echo inactive");
    let (rc, so, err) = b.cmd(&["logs", "bb"]).env("APPZONES_SUPERVISOR", "systemd").output().map(out).unwrap();
    assert_eq!(rc, 0, "{err}");
    assert!(so.starts_with("tail"), "{so}");
}

/// (text, class) of `appzones active` and `appzones active --json`, in B's environment plus ENV.
fn active(b: &Box_, env: &[(&str, &str)]) -> (String, (String, String)) {
    let run = |json: bool| {
        let mut c = b.cmd(if json { &["active", "--json"][..] } else { &["active"][..] });
        for (k, v) in env {
            c.env(k, v);
        }
        let (rc, so, err) = out(c.output().unwrap());
        assert_eq!(rc, 0, "{err}");
        so
    };
    let text = run(false).trim().to_string();
    let j: serde_json::Value = serde_json::from_str(&run(true)).unwrap();
    (text, (j["text"].as_str().unwrap().into(), j["class"].as_str().unwrap().into()))
}

#[test]
fn active_on_hyprland() {
    // the zone from the window's tag; zone:? is a sandbox it cannot name
    let b = Box_::new("active-hypr", Some(TWO));
    for (tags, text, want) in [("[\"zone:ta\"]", "[ta]", ("[ta]", "zone-ta")), ("[\"zone:?\"]", "[?]", ("[?]", "zone-unidentified")),
                               ("[]", "", ("", "host")), ("[\"other\", \"zone:tb*\"]", "[tb]", ("[tb]", "zone-tb"))] {
        b.tool("hyprctl", &format!("echo '{{\"tags\": {tags}}}'"));
        let got = active(&b, &[("HYPRLAND_INSTANCE_SIGNATURE", "x")]);
        assert_eq!(got, (text.to_string(), (want.0.to_string(), want.1.to_string())), "{tags}");
    }
}

#[test]
fn active_on_sway() {
    // the zone from the focused window's mark zone:NAME:ID; a malformed one is "?", never a name
    use std::io::{Read, Write};
    let b = Box_::new("active-sway", Some(TWO));
    let path = b.t.join("sway-ipc.sock");
    let l = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let marks = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let m2 = marks.clone();
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { return };
            let mut head = [0u8; 14];
            if c.read_exact(&mut head).is_err() {
                continue;
            }
            let n = u32::from_ne_bytes(head[6..10].try_into().unwrap()) as usize;
            let mut body = vec![0u8; n];
            let _ = c.read_exact(&mut body);
            let tree = serde_json::json!({"nodes": [{"nodes": [
                {"pid": 10, "id": 1, "focused": false, "marks": ["zone:other:1"], "nodes": []},
                {"pid": 11, "id": 5, "focused": true, "marks": *m2.lock().unwrap(), "nodes": []}]}]});
            let body = tree.to_string().into_bytes();
            let mut reply = b"i3-ipc".to_vec();
            reply.extend_from_slice(&(body.len() as u32).to_ne_bytes());
            reply.extend_from_slice(&head[10..14]);
            reply.extend_from_slice(&body);
            let _ = c.write_all(&reply);
        }
    });
    let sock = path.to_str().unwrap().to_string();
    let q = ("[?]", ("[?]", "zone-unidentified"));
    for (m, (text, want)) in [(&["zone:work:5"][..], ("[work]", ("[work]", "zone-work"))), (&["zone:?:5"][..], q),
                              (&[][..], ("", ("", "host"))), (&["mine"][..], ("", ("", "host"))),
                              (&["zone:Bad_Name:5"][..], q), (&["zone:work"][..], q), (&["zone:work:x"][..], q),
                              (&["zone:work:5:6"][..], q), (&["zone:work:7"][..], q), // another window's
                              (&["zone:work:5", "zone:home:5"][..], q),
                              (&["zone:work:5", "mine"][..], ("[work]", ("[work]", "zone-work")))] {
        *marks.lock().unwrap() = m.iter().map(|s| s.to_string()).collect();
        let got = active(&b, &[("SWAYSOCK", &sock)]);
        assert_eq!(got, (text.to_string(), (want.0.to_string(), want.1.to_string())), "{m:?}");
    }
    // sway gone: no label, no failure
    let gone = b.t.join("gone.sock");
    assert_eq!(active(&b, &[("SWAYSOCK", gone.to_str().unwrap())]).0, "");
}

#[test]
#[cfg(target_os = "linux")] // (starts or serves a zone: Linux so far)
fn plan_never_for_a_running_zone() {
    // the standalone plan replaces a zone's runtime directory: refused while its lock is held
    let b = Box_::new("plan-lock", Some(TWO));
    fs::create_dir_all(b.t.join("rt/appzones")).unwrap();
    let held = hold_lock(&b.t.join("rt/appzones/aa.lock"));
    let o = b.raw(&["plan", "aa"]).output().unwrap();
    drop(held);
    let (rc, _, err) = out(o);
    assert_eq!(rc, 1, "{err}");
    assert!(err.contains("zone aa is running"), "{err}");
}

#[test]
fn empty_denied_paths_override() {
    // APPZONES_DENIED_PATHS="" is no override (the host's file, as Python reads it), never an error
    let b = Box_::new("denied-empty", Some(TWO));
    let (rc, so, err) = b.cmd(&["check"]).env("APPZONES_DENIED_PATHS", "").output().map(out).unwrap();
    assert_eq!(rc, 0, "{so}{err}");
}

#[test]
#[cfg(target_os = "linux")]
fn nix_ld_refuses_its_zone_alone() {
    // a host without nix-ld: check names the zone, the rest of the file still works
    let b = Box_::new("nix-ld", Some(&format!("{}[zones.dd]\ncolour = \"#444444\"\nnetwork = \"none\"\nnix_ld = true\n",
        TWO.replace("version = 1\n", "version = 1\nrequires = 3\n"))));
    let env = [("APPZONES_LAYOUT", "nixos"), ("APPZONES_NIX_LD_SHIM", "/nonexistent/ld-linux-x86-64.so.2")];
    let (rc, so, err) = b.cmd(&["check"]).envs(env).output().map(out).unwrap();
    assert_eq!(rc, 1);
    assert!(err.contains("error: [zones.dd] nix_ld = true, but this host has no nix-ld") && !so.contains("ok:"), "{so}{err}");
    let (rc, so, err) = b.cmd(&["list"]).envs(env).output().map(out).unwrap();
    assert_eq!(rc, 0, "{err}");
    assert!(so.contains("aa ") && so.contains("dd "), "{so}");
}

#[test]
#[cfg(not(target_os = "linux"))]
fn what_macos_has_not() {
    // what macOS cannot do (design/macos.md) says so; the rest works
    let b = Box_::new("macos-not", Some(TWO));
    for (cmd, says) in [(&["mount", "aa", "/tmp"][..], "not at run time on macOS"), (&["apps"], "apps: not on macOS yet")] {
        let (rc, _, err) = b.run(cmd);
        assert_eq!(rc, 1, "{cmd:?}");
        assert!(err.contains(says), "{cmd:?}: {err}");
    }
    let (rc, so, err) = b.run(&["check"]);
    assert_eq!(rc, 0, "{so}{err}");
    assert!(so.starts_with("ok: "), "{so}");
    // a zone that asks for what macOS cannot give: an error for it in check, and it does not start
    fs::write(b.t.join("zones.toml"), format!("{TWO}[zones.cc]\ncolour = \"#333333\"\ndns = \"public\"\n")).unwrap();
    let (rc, so, err) = b.run(&["check"]);
    assert_eq!(rc, 1);
    assert!(err.contains("zone cc: dns = \"public\": not on macOS yet"), "{err}");
    assert!(!so.contains("ok:"), "no ok before a refusal: {so}");
    let (rc, _, err) = b.run(&["up", "cc"]);
    assert_eq!(rc, 1);
    assert!(err.contains("cannot run on macOS as configured"), "{err}");
    // nix_ld: refused for its zone alone (never the whole file: other zones still start)
    fs::write(b.t.join("zones.toml"), format!("{}[zones.dd]\ncolour = \"#444444\"\nnetwork = \"none\"\nnix_ld = true\n",
        TWO.replace("version = 1\n", "version = 1\nrequires = 3\n"))).unwrap();
    let (rc, _, err) = b.run(&["check"]);
    assert_eq!(rc, 1);
    assert!(err.contains("zone dd: nix_ld = true: not on macOS") && !err.contains("[zones.dd] nix_ld"), "{err}");
}

/// ARGS run in CWD on a terminal of their own, whose keyboard types ANSWER: (status, what
/// the terminal showed, stdout). Without ANSWER: no terminal at all (a session of its own).
fn at_terminal(b: &Box_, cwd: &Path, args: &[&str], answer: Option<&str>) -> (i32, String, String) {
    use std::io::{Read, Write};
    use std::os::fd::FromRawFd;
    use std::os::unix::process::CommandExt;
    let mut c = b.cmd(args);
    c.current_dir(cwd).stdout(Stdio::piped()).stderr(Stdio::piped());
    let Some(answer) = answer else {
        // Safety: setsid in the child: no controlling terminal, so /dev/tty cannot open.
        unsafe { c.pre_exec(|| { libc::setsid(); Ok(()) }) };
        let (rc, so, se) = out(c.output().unwrap());
        return (rc, se, so);
    };
    let (mut m, mut s) = (0, 0);
    // Safety: openpty into two locals; the master is close-on-exec, the slave is the child's
    // controlling terminal (setsid, TIOCSCTTY) and closed here after the spawn.
    unsafe {
        assert_eq!(libc::openpty(&mut m, &mut s, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()), 0);
        libc::fcntl(m, libc::F_SETFD, libc::FD_CLOEXEC);
        c.pre_exec(move || {
            libc::setsid();
            libc::ioctl(s, libc::TIOCSCTTY as _, 0);
            Ok(())
        });
    }
    let child = c.spawn().unwrap();
    // Safety: our copy of the slave; the master is ours from here on.
    let mut master = unsafe {
        libc::close(s);
        std::fs::File::from_raw_fd(m)
    };
    master.write_all(answer.as_bytes()).unwrap();
    // the terminal read while the child runs: on macOS a process leaving its terminal waits
    // until what it wrote there was read
    let reader = std::thread::spawn(move || {
        let mut shown = Vec::new();
        let mut buf = [0u8; 4096];
        while let Ok(n) = master.read(&mut buf) {
            if n == 0 {
                break;
            }
            shown.extend_from_slice(&buf[..n]);
        }
        shown
    });
    let o = child.wait_with_output().unwrap();
    let shown = reader.join().unwrap();
    let (rc, so, se) = out(o);
    (rc, format!("{}{se}", String::from_utf8_lossy(&shown)), so)
}

#[test]
fn project_zones_are_trusted_at_the_terminal() {
    let b = Box_::new("project", Some(TWO));
    let app = b.t.join("home/src/app");
    fs::create_dir_all(app.join(".git")).unwrap();
    fs::create_dir_all(app.join("deep")).unwrap();
    let file = "version = 1\nrequires = 8\ndefault = \"agent\"\n[zones.agent]\nnetwork = \"internet\"\nallow = [\"github.com\"]\n\
                [zones.dev]\ncheckout = \"rw\"\n";
    fs::write(app.join("appzones.toml"), file).unwrap();
    let store = b.t.join("home/.local/state/appzones/projects");
    // not trusted: @ says so, and starts nothing
    let (rc, se, _) = at_terminal(&b, &app.join("deep"), &["run", "@", "--", "true"], None);
    assert_eq!(rc, 1);
    assert!(se.contains("is not trusted"), "{se}");
    // trust: never without a terminal, never on anything but a yes
    let (rc, se, _) = at_terminal(&b, &app, &["trust"], None);
    assert!(rc == 1 && se.contains("asks at the terminal"), "{se}");
    let (rc, shown, so) = at_terminal(&b, &app, &["trust"], Some("n\n"));
    assert!(rc == 1 && so.contains("not trusted"), "{shown}{so}");
    assert!(!store.exists() || fs::read_dir(&store).unwrap().next().is_none());
    let (rc, shown, so) = at_terminal(&b, &app.join("deep"), &["trust"], Some("y\n"));
    assert_eq!(rc, 0, "{shown}{so}");
    assert!(shown.contains("It asks for:") && shown.contains("! @agent (the default): the internet, only github.com")
            && shown.contains("! @dev: the project folder") && shown.contains("READ-WRITE")
            && shown.contains("@agent (the default): the project folder") && shown.contains(".git/hooks"), "{shown}");
    assert!(so.contains("trusted:") && so.contains("@agent, @dev"), "{so}");
    // the copy is in the store (0700), and the zones are the host's now, shown by project
    let rec: Vec<_> = fs::read_dir(&store).unwrap().flatten().collect();
    assert_eq!(rec.len(), 1);
    assert_eq!(fs::read_to_string(rec[0].path().join("appzones.toml")).unwrap(), file);
    assert_eq!(fs::metadata(rec[0].path()).unwrap().permissions().mode() & 0o777, 0o700);
    let (rc, so, err) = b.run(&["list"]);
    assert!(rc == 0 && so.contains("app/agent") && so.contains("app/dev"), "{so}{err}");
    let (rc, so, err) = b.run(&["check"]);
    assert!(rc == 0 && so.contains("; 2 project zones (app/agent, app/dev)"), "{so}{err}");
    // trusted again unchanged: nothing asked
    let (rc, _, so) = at_terminal(&b, &app, &["trust"], None);
    assert!(rc == 0 && so.contains("trusted already, unchanged"), "{so}");
    // changed: @ refuses to start it; trust shows what changed
    fs::write(app.join("appzones.toml"), file.replace("github.com", "evil.example")).unwrap();
    let (rc, se, _) = at_terminal(&b, &app, &["run", "@", "--", "true"], None);
    assert!(rc == 1 && se.contains("changed since you trusted it"), "{se}");
    let (rc, se, _) = at_terminal(&b, &app, &["run", "@nope", "--", "true"], None);
    assert!(rc == 1 && se.contains("changed since you trusted it"), "{se}");
    let (_, shown, _) = at_terminal(&b, &app, &["trust"], Some("n\n"));
    assert!(shown.contains("Changed since you trusted it:") && shown.contains("+ allow = [\"evil.example\"]")
            && shown.contains("- allow = [\"github.com\"]"), "{shown}");
    // a key a project file may never have: refused before anything is asked
    fs::write(app.join("appzones.toml"), format!("{file}devices = [\"gpu\"]\n")).unwrap();
    let (rc, se, _) = at_terminal(&b, &app, &["trust"], None);
    assert!(rc == 1 && se.contains("devices: never in a project file"), "{se}");
    // the file gone: managing commands still find the trusted project (starting needs the file)
    fs::remove_file(app.join("appzones.toml")).unwrap();
    let (rc, se, _) = at_terminal(&b, &app.join("deep"), &["ps", "@"], None);
    assert!(rc == 1 && se.contains("is not running"), "{se}");
    let (rc, se, _) = at_terminal(&b, &app, &["run", "@", "--", "true"], None);
    assert!(rc == 1 && se.contains("no appzones.toml"), "{se}");
    // a zone that binds the project's file cannot be built without it: its log is still there
    fs::write(app.join("appzones.toml"), "version = 1\nrequires = 8\n[zones.agent]\nbinds = [{ path = \"appzones.toml\" }]\n").unwrap();
    let (rc, shown, so) = at_terminal(&b, &app, &["trust"], Some("y\n"));
    assert_eq!(rc, 0, "{shown}{so}");
    fs::remove_file(app.join("appzones.toml")).unwrap();
    b.tool("tail", "echo tail \"$@\"");
    let (rc, _, so) = at_terminal(&b, &app, &["logs", "@"], None);
    assert!(rc == 0 && so.starts_with("tail -n 200 ") && so.contains("/p"), "{so}");
    // untrust: forgotten, its zones stopped (a systemd unit still starting too), and @ says so again
    fs::write(app.join("appzones.toml"), file).unwrap();
    let (rc, shown, so) = at_terminal(&b, &app, &["trust"], Some("y\n"));
    assert_eq!(rc, 0, "{shown}{so}");
    let calls = b.t.join("systemctl-calls");
    b.tool("systemctl", &format!("echo \"$*\" >> {}; case \"$*\" in *is-active*) echo activating;; esac", calls.display()));
    let (rc, _, so) = at_terminal(&b, &app, &["untrust"], None);
    assert!(rc == 0 && so.contains("not trusted any more"), "{so}");
    let called = fs::read_to_string(&calls).unwrap_or_default();
    assert_eq!(called.lines().filter(|l| l.starts_with("--user stop appzones@p")).count(), 2, "{called}");
    let (rc, se, _) = at_terminal(&b, &app, &["run", "@", "--", "true"], None);
    assert!(rc == 1 && se.contains("is not trusted"), "{se}");
    // the host can turn project zones off
    fs::write(b.t.join("zones.toml"), format!("{}[projects]\nenabled = false\n", TWO.replace("version = 1\n", "version = 1\nrequires = 8\n")))
        .unwrap();
    let (rc, se, _) = at_terminal(&b, &app, &["run", "@", "--", "true"], None);
    assert!(rc == 1 && se.contains("project zones are off"), "{se}");
    // a host zone may not take a project zone's name
    fs::write(b.t.join("zones.toml"), "version = 1\n[zones.p0123456789]\ncolour = \"#111111\"\n").unwrap();
    let (rc, _, err) = b.run(&["check"]);
    assert!(rc == 1 && err.contains("name project zones"), "{err}");
}

#[test]
fn the_example_project_file_is_valid() {
    // examples/project.toml (docs/configuration/projects.md), trusted as a user would; the
    // helper's Nix build has only helper/, so there it is skipped (`just test` runs it)
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/project.toml");
    let file = match fs::read_to_string(&example) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skipped: no {} (the helper's own build)", example.display());
            return;
        }
        Err(e) => panic!("{}: {e}", example.display()),
    };
    let b = Box_::new("example", Some(TWO));
    let app = b.t.join("home/src/app");
    fs::create_dir_all(app.join(".git")).unwrap();
    fs::write(app.join("appzones.toml"), &file).unwrap();
    let (rc, shown, so) = at_terminal(&b, &app, &["trust"], Some("y\n"));
    assert_eq!(rc, 0, "{shown}{so}");
    let (rc, so, err) = b.run(&["check"]);
    assert!(rc == 0 && so.contains("; 2 project zones (app/agent, app/dev)") && err.is_empty(), "{so}{err}");
}
