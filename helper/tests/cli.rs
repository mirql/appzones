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
        let mut c = Command::new(HELPER);
        c.arg("cli").args(args).env_clear().stdin(Stdio::null())
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
    // the pre-0.6 state stays where it is: a typo migrates nothing
    fs::create_dir_all(b.t.join("home/.local/share/zones/ta")).unwrap();
    assert_eq!(b.run(&["lsit"]), (2, String::new(),
               "appzones: unknown command 'lsit' (did you mean 'list'?); see appzones --help\n".into()));
    assert_eq!(b.run(&["zzzzzz"]), (2, String::new(), "appzones: unknown command 'zzzzzz'; see appzones --help\n".into()));
    let (rc, _, err) = b.run(&["netsetup"]);
    assert_eq!(rc, 2);
    assert!(!err.contains("_netsetup"), "an internal command suggested: {err}");
    assert!(b.t.join("home/.local/share/zones/ta").is_dir());
    assert!(!b.t.join("home/.local/share/appzones").exists());
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
fn supervisor_setting() {
    let b = Box_::new("supervisor", Some(TWO));
    let (rc, _, err) = b.cmd(&["up", "aa"]).env("APPZONES_SUPERVISOR", "runit").output().map(out).unwrap();
    assert_eq!(rc, 1);
    assert!(err.contains("use one of auto, systemd, spawn"), "{err}");
}

#[test]
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
fn state_moves_once_nothing_runs() {
    // the pre-0.6 ~/.local/share/zones moves to ~/.local/share/appzones when no zone runs
    let b = Box_::new("migrate", Some(TWO));
    let legacy = b.t.join("home/.local/share/zones");
    fs::create_dir_all(legacy.join("ta/home")).unwrap();
    fs::write(legacy.join("ta/home/kept"), "x").unwrap();
    // a server holds a zone's lock: it runs on the old directory, which stays
    fs::create_dir_all(b.t.join("rt/appzones")).unwrap();
    let held = hold_lock(&b.t.join("rt/appzones/ta.lock"));
    let (rc, _, err) = b.run(&["list"]);
    assert_eq!(rc, 0, "{err}");
    assert!(err.contains("once no zone runs"), "{err}");
    assert!(legacy.join("ta/home/kept").exists());
    drop(held);
    // a server of this version holds the state lock (shared) from before it picks its
    // state directory: the move waits for it too, so none starts in between
    let state = fs::OpenOptions::new().read(true).write(true).create(true).truncate(false)
        .open(b.t.join("rt/appzones/.state.lock")).unwrap();
    // Safety: flock on our own file.
    assert_eq!(unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&state), libc::LOCK_SH | libc::LOCK_NB) }, 0);
    let (rc, _, err) = b.run(&["list"]);
    assert_eq!(rc, 0, "{err}");
    assert!(err.contains("once no zone runs"), "{err}");
    assert!(legacy.join("ta/home/kept").exists());
    drop(state);
    let (rc, _, err) = b.run(&["list"]);
    assert_eq!(rc, 0, "{err}");
    if err.contains("once no zone runs") {
        // this machine runs zones of an appzones from before the lock (seen by their command
        // line, as they must be): the move waits for them, rightly; the Nix build checks it
        eprintln!("skipped the move: zones of an older appzones run here");
        return;
    }
    assert!(err.contains("moved"), "{err}");
    assert_eq!(fs::read_to_string(b.t.join("home/.local/share/appzones/ta/home/kept")).unwrap(), "x");
    assert!(!legacy.exists());
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
