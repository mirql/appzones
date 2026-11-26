//! The golden tests: what the Python implementation answered, before it was removed
//! (tests/golden/README.md), and what this helper answers now, must be the same:
//!
//! - config.json: every configuration the Python unit tests loaded, and the examples
//!   (errors, warnings, every zone's settings, origins and settings_hash);
//! - plan.json: make_plan for three zones on both layouts (plan.json, the files written,
//!   what it printed);
//! - policy.json: the never-list for runtime mounts, and its answer for 169 paths.
//!
//! Each case rebuilds the file tree Python saw ("{T}" in the files is its root). The
//! answers about system paths depend on the file system, so this runs where they were
//! captured, in a Nix build (nix/helper.nix sets APPZONES_GOLDEN=1); elsewhere it is skipped.

use serde_json::Value;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

const HELPER: &str = env!("CARGO_BIN_EXE_appzones-helper");

fn enabled() -> bool {
    if std::env::var_os("APPZONES_GOLDEN").is_none() {
        eprintln!("skipped: set APPZONES_GOLDEN=1 (the answers need the Nix build's file system)");
        return false;
    }
    true
}

/// APPZONES_GOLDEN_WRITE=DIR: write this helper's answers there, as golden files, instead
/// of comparing (helper/tests/golden/update.nix; then review the diff before copying them).
fn write_to() -> Option<PathBuf> {
    std::env::var_os("APPZONES_GOLDEN_WRITE").map(PathBuf::from)
}

fn write_golden(name: &str, v: &Value) {
    let dir = write_to().unwrap();
    fs::create_dir_all(&dir).unwrap();
    let text = if name == "config.json" {
        serde_json::to_string(v).unwrap()
    } else {
        serde_json::to_string_pretty(v).unwrap()
    };
    fs::write(dir.join(name), text + "\n").unwrap();
}

fn golden(name: &str) -> Value {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    serde_json::from_slice(&fs::read(&p).unwrap()).unwrap()
}

fn b64(s: &str) -> Vec<u8> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => panic!("not base64: {c}"),
    } as u32;
    let mut out = Vec::new();
    for chunk in s.as_bytes().chunks(4) {
        let n = chunk.iter().take_while(|&&c| c != b'=').count();
        let mut acc = 0u32;
        for (i, &c) in chunk[..n].iter().enumerate() {
            acc |= val(c) << (18 - 6 * i);
        }
        out.extend_from_slice(&acc.to_be_bytes()[1..n]);
    }
    out
}

struct Tmp(PathBuf);

impl Tmp {
    fn new(tag: &str) -> Tmp {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("az-golden-{}-{tag}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p.canonicalize().unwrap())
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = Command::new("chmod")
            .arg("-R")
            .arg("u+rwx")
            .arg(&self.0)
            .status();
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The tree Python saw, under ROOT: directories, files (contents, modes, ages), links.
fn build(root: &Path, tree: &Value) {
    let t = root.to_str().unwrap();
    let mut modes = Vec::new();
    for e in tree.as_array().unwrap() {
        let p = root.join(e["path"].as_str().unwrap());
        if let Some(l) = e.get("link") {
            symlink(l.as_str().unwrap().replace("{T}", t), &p).unwrap();
        } else if let Some(k) = e.get("special") {
            if k == "socket" {
                // (the socket file stays after the listener is closed: only the file is needed)
                drop(std::os::unix::net::UnixListener::bind(&p).unwrap());
            } else {
                assert!(Command::new("mkfifo").arg(&p).status().unwrap().success());
            }
        } else if let Some(m) = e.get("dir") {
            fs::create_dir_all(&p).unwrap();
            modes.push((p, m.as_u64().unwrap() as u32));
        } else {
            let data = b64(e["data"].as_str().unwrap());
            let data = String::from_utf8(data.clone())
                .map(|s| s.replace("{T}", t).into_bytes())
                .unwrap_or(data);
            fs::write(&p, data).unwrap();
            fs::set_permissions(
                &p,
                fs::Permissions::from_mode(e["mode"].as_u64().unwrap() as u32),
            )
            .unwrap();
            let when = SystemTime::now() - Duration::from_secs(e["age"].as_u64().unwrap());
            // (both times: "unused" is by the later of them)
            let times = fs::FileTimes::new().set_accessed(when).set_modified(when);
            fs::File::options()
                .write(true)
                .open(&p)
                .unwrap()
                .set_times(times)
                .unwrap();
        }
    }
    for (p, m) in modes.into_iter().rev() {
        fs::set_permissions(&p, fs::Permissions::from_mode(m)).unwrap();
    }
}

/// V with each A replaced by B in every string (JSON-escaped both).
fn subst(v: &Value, from: &[(&str, &str)]) -> Value {
    let esc = |x: &str| {
        let j = serde_json::to_string(x).unwrap();
        j[1..j.len() - 1].to_string()
    };
    let mut s = v.to_string();
    for (a, b) in from {
        s = s.replace(&esc(a), &esc(b));
    }
    serde_json::from_str(&s).unwrap()
}

fn env_of(cmd: &mut Command, env: &Value, from: &[(&str, &str)]) {
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default());
    for (k, v) in env.as_object().unwrap() {
        let mut v = v.as_str().unwrap().to_string();
        for (a, b) in from {
            v = v.replace(a, b);
        }
        cmd.env(k, v);
    }
}

#[test]
fn same_configs() {
    if !enabled() {
        return;
    }
    let mut g = golden("config.json");
    let trees = g["trees"].as_array().unwrap().clone();
    let cases = g["cases"].as_array().unwrap().clone();
    let mut diffs = Vec::new();
    let mut new_wants = Vec::new();
    for case in &cases {
        let tmp = Tmp::new("config");
        let t = tmp.0.to_str().unwrap();
        let tree = match &case["tree"] {
            Value::Number(n) => &trees[n.as_u64().unwrap() as usize],
            v => v,
        };
        build(&tmp.0, tree);
        let text = case["text"].as_str().unwrap().replace("{T}", t);
        let file = tmp.0.join(".golden-config.toml");
        fs::write(&file, &text).unwrap();
        let name = case["path"].as_str().unwrap().replace("{T}", t);
        let mut cmd = Command::new(HELPER);
        cmd.arg("config-json")
            .arg(&file)
            .arg("--name")
            .arg(&name)
            .arg("--code-dir")
            .arg(tmp.0.join("code"));
        env_of(&mut cmd, &case["env"], &[("{T}", t)]);
        let out = cmd.output().unwrap();
        let got: Value = match serde_json::from_slice(&out.stdout) {
            Ok(v) => subst(&v, &[(t, "{T}")]),
            Err(_) => {
                diffs.push(format!(
                    "{text}\n  no JSON: {}",
                    String::from_utf8_lossy(&out.stderr)
                ));
                new_wants.push(case["want"].clone());
                continue;
            }
        };
        let want = &case["want"];
        // (a parse error, and an integer beyond 64 bits, keep Python's answer: the accepted
        // differences; anything else is this helper's answer now)
        let accepted = case["parsed"] == Value::Bool(false)
            || (!want["errors"].as_array().unwrap().is_empty()
                && got["errors"].as_array().unwrap().len() == 1
                && got["errors"][0]
                    .as_str()
                    .unwrap()
                    .contains("number too large"));
        new_wants.push(if accepted { want.clone() } else { got.clone() });
        if case["parsed"] == Value::Bool(false) {
            // the TOML parsers word their errors apart: both refuse it, naming the file
            let e = got["errors"].as_array().unwrap();
            let ok = e.len() == 1
                && e[0]
                    .as_str()
                    .unwrap()
                    .starts_with(&format!("{}: ", case["path"].as_str().unwrap()))
                && got["zones"].as_object().unwrap().is_empty();
            if !ok {
                diffs.push(format!("{text}\n  python: {want}\n  rust:   {got}"));
            }
            continue;
        }
        let too_large = !want["errors"].as_array().unwrap().is_empty()
            && got["errors"].as_array().unwrap().len() == 1
            && got["errors"][0]
                .as_str()
                .unwrap()
                .contains("number too large");
        if &got != want && !too_large {
            diffs.push(format!("{text}\n  python: {want}\n  rust:   {got}"));
        }
    }
    if write_to().is_some() {
        // (never a golden file with an old answer kept for a case that gave none)
        let missing: Vec<&String> = diffs
            .iter()
            .filter(|d| d.contains("\n  no JSON: "))
            .collect();
        assert!(
            missing.is_empty(),
            "{} configurations gave no JSON, nothing written:\n{}",
            missing.len(),
            missing
                .iter()
                .take(3)
                .map(|d| d.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        );
        for (c, w) in g["cases"].as_array_mut().unwrap().iter_mut().zip(new_wants) {
            c["want"] = w;
        }
        write_golden("config.json", &g);
        return;
    }
    for d in diffs.iter().take(5) {
        eprintln!("--- differs:\n{d}");
    }
    assert!(cases.len() > 900, "{} configurations", cases.len());
    assert!(
        diffs.is_empty(),
        "{} of {} configurations differ",
        diffs.len(),
        cases.len()
    );
}

/// Every file, link and directory the plan maker left (test_zonecfg._snapshot, as JSON).
fn snapshot(t: &Path) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    fn walk(p: &Path, all: &mut Vec<PathBuf>) {
        let Ok(rd) = fs::read_dir(p) else { return };
        for e in rd.flatten() {
            let q = e.path();
            all.push(q.clone());
            if e.file_type().map(|f| f.is_dir()).unwrap_or(false) {
                walk(&q, all);
            }
        }
    }
    for root in [t.join("rt"), t.join("home/.local/share/appzones")] {
        let mut all = Vec::new();
        walk(&root, &mut all);
        all.sort_by(|a, b| {
            a.as_os_str()
                .as_encoded_bytes()
                .cmp(b.as_os_str().as_encoded_bytes())
        });
        for p in all {
            let rel = p.strip_prefix(t).unwrap().to_str().unwrap().to_string();
            let name = p.file_name().unwrap().to_str().unwrap();
            if name.starts_with(".colours.")
                || (name.ends_with(".lock") && rel.matches('/').count() == 2)
            {
                continue; // (a zone's lock: the plan command takes it, as the server does)
            }
            let md = fs::symlink_metadata(&p).unwrap();
            let mode = format!("0o{:o}", md.permissions().mode() & 0o777);
            let v = if md.file_type().is_symlink() {
                serde_json::json!(["link", fs::read_link(&p).unwrap().to_str().unwrap()])
            } else if md.is_dir() {
                serde_json::json!(["dir", mode])
            } else if name == "plan.json" {
                serde_json::json!([
                    "plan",
                    serde_json::from_slice::<Value>(&fs::read(&p).unwrap()).unwrap()
                ])
            } else {
                serde_json::json!([
                    "file",
                    mode,
                    String::from_utf8_lossy(&fs::read(&p).unwrap())
                ])
            };
            out.insert(rel, v);
        }
    }
    out
}

#[test]
fn same_plans() {
    if !enabled() {
        return;
    }
    let mut cases = golden("plan.json");
    let path = std::env::var("PATH").unwrap_or_default();
    let mut n = 0;
    let mut news = Vec::new();
    for case in cases.as_array().unwrap() {
        let tmp = Tmp::new("plan");
        let t = tmp.0.join("f");
        fs::create_dir(&t).unwrap();
        let ts = t.to_str().unwrap();
        build(&t, &case["tree"]);
        let zone = case["zone"].as_str().unwrap();
        let mut cmd = Command::new(HELPER);
        cmd.arg("plan").arg(zone);
        env_of(
            &mut cmd,
            &case["env"],
            &[("{T}", ts), ("{PATH}", &path), ("{HELPER}", HELPER)],
        );
        let out = cmd.output().unwrap();
        let lines = |b: &[u8]| -> Vec<Value> {
            let s = String::from_utf8_lossy(b);
            s.lines().map(|l| Value::String(l.to_string())).collect()
        };
        let got = serde_json::json!({
            "rc": out.status.code().unwrap_or(-1),
            "stdout": lines(&out.stdout),
            "stderr": lines(&out.stderr),
            "files": snapshot(&t),
        });
        let got = subst(
            &got,
            &[(HELPER, "{HELPER}"), (&path, "{PATH}"), (ts, "{T}")],
        );
        if write_to().is_some() {
            news.push(got);
            continue;
        }
        let want = &case["want"];
        let what = format!("{} {}", case["layout"], zone);
        assert_eq!(
            got["rc"], want["rc"],
            "{what}: exit status; stderr {}",
            got["stderr"]
        );
        assert_eq!(got["stdout"], want["stdout"], "{what}: stdout");
        assert_eq!(got["stderr"], want["stderr"], "{what}: stderr");
        let (g, w) = (
            got["files"].as_object().unwrap(),
            want["files"].as_object().unwrap(),
        );
        let mut keys: Vec<&String> = g.keys().chain(w.keys()).collect();
        keys.sort();
        keys.dedup();
        for k in keys {
            assert_eq!(g.get(k), w.get(k), "{what}: {k}");
        }
        n += 1;
    }
    if write_to().is_some() {
        for (c, w) in cases.as_array_mut().unwrap().iter_mut().zip(news) {
            c["want"] = w;
        }
        write_golden("plan.json", &cases);
        return;
    }
    assert_eq!(n, 6);
}

#[test]
fn same_policy() {
    if !enabled() {
        return;
    }
    let mut g = golden("policy.json");
    let tmp = Tmp::new("policy");
    let t = tmp.0.to_str().unwrap();
    build(&tmp.0, &g["tree"]);
    let sub = |v: &Value| v.as_str().unwrap().replace("{T}", t);
    let args: Vec<String> = g["args"].as_array().unwrap().iter().map(sub).collect();
    let paths: Vec<String> = g["paths"].as_array().unwrap().iter().map(sub).collect();
    let out = Command::new(HELPER)
        .args(&args)
        .args(&paths)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<Value> = text
        .trim_end_matches('\n')
        .split('\n')
        .map(|l| subst(&serde_json::from_str(l).unwrap(), &[(t, "{T}")]))
        .collect();
    if write_to().is_some() {
        g["denied"] = lines[0].clone();
        g["want"] = Value::Array(lines[1..].to_vec());
        write_golden("policy.json", &g);
        return;
    }
    assert_eq!(lines[0], g["denied"], "the never-list");
    let want = g["want"].as_array().unwrap();
    assert_eq!(lines.len() - 1, want.len());
    let diffs: Vec<String> = g["paths"]
        .as_array()
        .unwrap()
        .iter()
        .zip(want)
        .zip(&lines[1..])
        .filter(|((_, w), r)| w != r)
        .map(|((p, w), r)| format!("{p}:\n  python: {w}\n  rust:   {r}"))
        .collect();
    assert!(diffs.is_empty(), "{}", diffs.join("\n"));
}
