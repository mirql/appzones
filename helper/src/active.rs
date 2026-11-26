//! `appzones active [--follow] [--json]`: "[zone]" for the focused window, on Hyprland or
//! sway, only from what the appzones hooks set (the tag zone:NAME, the mark zone:NAME:ID),
//! never from the window's class, title or app id (the port of client.active).

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};

use crate::policy::zone_name_ok;
use crate::pyfmt::py_json;

const MAGIC: &[u8] = b"i3-ipc";
const RUN_COMMAND: u32 = 0;
const SUBSCRIBE: u32 = 2;
const GET_TREE: u32 = 4;

fn send(s: &mut UnixStream, kind: u32, payload: &str) -> std::io::Result<()> {
    let mut m = MAGIC.to_vec();
    m.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
    m.extend_from_slice(&kind.to_ne_bytes());
    m.extend_from_slice(payload.as_bytes());
    s.write_all(&m)
}

fn recv(s: &mut UnixStream) -> std::io::Result<Value> {
    let mut head = [0u8; 14];
    s.read_exact(&mut head)?;
    if &head[..6] != MAGIC {
        return Err(std::io::Error::other("not an i3-ipc message"));
    }
    let n = u32::from_ne_bytes([head[6], head[7], head[8], head[9]]) as usize;
    let mut body = vec![0u8; n];
    s.read_exact(&mut body)?;
    serde_json::from_slice(&body).map_err(std::io::Error::other)
}

fn sway() -> Option<String> {
    let s = std::env::var("SWAYSOCK").ok().filter(|s| !s.is_empty())?;
    std::env::var("HYPRLAND_INSTANCE_SIGNATURE")
        .ok()
        .filter(|h| !h.is_empty())
        .is_none()
        .then_some(s)
}

fn sway_focused(path: &str) -> std::io::Result<(Option<Value>, Vec<String>)> {
    let mut s = UnixStream::connect(path)?;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    send(&mut s, GET_TREE, "")?;
    let tree = recv(&mut s)?;
    let mut stack = vec![tree];
    while let Some(n) = stack.pop() {
        let truthy = |v: Option<&Value>| {
            v.is_some_and(|v| !matches!(v, Value::Null | Value::Bool(false)) && *v != json!(0))
        };
        if truthy(n.get("focused")) && truthy(n.get("pid")) {
            let marks = n
                .get("marks")
                .and_then(Value::as_array)
                .map(|m| {
                    m.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            return Ok((n.get("id").cloned(), marks));
        }
        for k in ["nodes", "floating_nodes"] {
            if let Some(a) = n.get(k).and_then(Value::as_array) {
                stack.extend(a.iter().cloned());
            }
        }
    }
    Ok((None, vec![]))
}

fn focused_zone() -> String {
    if let Some(path) = sway() {
        let Ok((con, marks)) = sway_focused(&path) else {
            return String::new();
        };
        let con = con.map(|c| c.to_string()).unwrap_or_else(|| "None".into());
        let zones: Vec<String> = marks
            .iter()
            .filter(|m| m.starts_with("zone:"))
            .map(|m| {
                let parts: Vec<&str> = m.split(':').collect();
                let ok = parts.len() == 3
                    && parts[2] == con
                    && (parts[1] == "?" || zone_name_ok(parts[1]));
                if ok {
                    parts[1].to_string()
                } else {
                    "?".to_string()
                }
            })
            .collect();
        let mut uniq = zones.clone();
        uniq.dedup();
        uniq.sort();
        uniq.dedup();
        return match (zones.first(), uniq.len()) {
            (Some(z), 1) => z.clone(),
            (Some(_), _) => "?".into(),
            (None, _) => String::new(),
        };
    }
    let tags: Vec<String> = std::process::Command::new("hyprctl")
        .args(["-j", "activewindow"])
        .output()
        .ok()
        .and_then(|o| {
            serde_json::from_slice::<Value>(if o.stdout.is_empty() {
                b"{}"
            } else {
                &o.stdout
            })
            .ok()
        })
        .and_then(|v| v.get("tags").and_then(Value::as_array).cloned())
        .map(|t| {
            t.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    tags.iter()
        .find_map(|t| {
            t.strip_prefix("zone:")
                .map(|z| z.trim_end_matches('*').to_string())
        })
        .unwrap_or_default()
}

fn current(as_json: bool) -> String {
    let z = focused_zone();
    if !as_json {
        return if z.is_empty() {
            String::new()
        } else {
            format!("[{z}]")
        };
    }
    if z == "?" {
        return py_json(&json!({"text": "[?]", "class": "zone-unidentified",
                               "tooltip": "active window runs in a sandbox appzones cannot name"}));
    }
    let (text, class, tip) = if z.is_empty() {
        (String::new(), "host".to_string(), String::new())
    } else {
        (
            format!("[{z}]"),
            format!("zone-{z}"),
            format!("active window runs in app zone {z}"),
        )
    };
    py_json(&json!({"text": text, "class": class, "tooltip": tip}))
}

fn say(line: &str) {
    let mut o = std::io::stdout().lock();
    let _ = writeln!(o, "{line}");
    let _ = o.flush();
}

pub fn main(follow: bool, as_json: bool, host_rt: &Path) -> i32 {
    if !follow {
        say(&current(as_json));
        return 0;
    }
    // subscribed first, then the first label: no change in between is missed
    if let Some(path) = sway() {
        let Ok(mut s) = UnixStream::connect(&path) else {
            return 1;
        };
        if send(&mut s, SUBSCRIBE, "[\"window\", \"workspace\"]").is_err() || recv(&mut s).is_err()
        {
            return 1;
        }
        let mut last = current(as_json);
        say(&last);
        while recv(&mut s).is_ok() {
            let cur = current(as_json);
            if cur != last {
                say(&cur);
                last = cur;
            }
        }
        return 0;
    }
    let Some(sig) = std::env::var("HYPRLAND_INSTANCE_SIGNATURE")
        .ok()
        .filter(|s| !s.is_empty())
    else {
        say(&current(as_json));
        eprintln!("appzones: no HYPRLAND_INSTANCE_SIGNATURE or SWAYSOCK");
        return 1;
    };
    let Ok(mut s) = UnixStream::connect(host_rt.join("hypr").join(sig).join(".socket2.sock"))
    else {
        return 1;
    };
    let mut last = current(as_json);
    say(&last);
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = match s.read(&mut chunk) {
            Ok(0) | Err(_) => return 0,
            Ok(n) => n,
        };
        buf.extend_from_slice(&chunk[..n]);
        let Some(cut) = buf.iter().rposition(|b| *b == b'\n') else {
            continue;
        };
        let lines: Vec<Vec<u8>> = buf[..cut]
            .split(|b| *b == b'\n')
            .map(<[u8]>::to_vec)
            .collect();
        buf.drain(..=cut);
        if lines.iter().any(|l| {
            l.starts_with(b"activewindowv2>>")
                || l.starts_with(b"windowtitlev2>>")
                || l.starts_with(b"closewindow>>")
        }) {
            let cur = current(as_json);
            if cur != last {
                say(&cur);
                last = cur;
            }
        }
    }
}

// ---- the sway hook (contrib/sway/appzones-sway runs `appzones _sway-marks`) ----

const ENGINE: &str = "dev.mirql.zones";
const APP_PREFIX: &str = "dev.mirql.zone.";

/// The zone's name, "?" for a window from a zone (or another sandbox) that cannot be
/// named, None for a host window: from what a zone cannot change, never from the window's
/// title or app id. The window's security context (sway 1.11+: engine and app id, set by
/// zone-wayland on the host), and its process's cgroup (appzones@NAME.service, or below it,
/// under appzones.slice); both there: they must agree. Fails closed: another sandbox, a
/// process under appzones.slice not a zone's unit, a nested pid namespace, an unreadable pid.
pub fn zone_of(pid: Option<i64>, window: &Value, proc: &Path) -> Option<String> {
    let mut by_ctx = None;
    let engine = window
        .get("sandbox_engine")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !engine.is_empty() {
        let app = window
            .get("sandbox_app_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        let name = app.strip_prefix(APP_PREFIX).unwrap_or("");
        if engine != ENGINE || !zone_name_ok(name) {
            return Some("?".into());
        }
        by_ctx = Some(name.to_string());
    }
    let pid = match pid {
        Some(p) if p > 0 => p,
        _ => return by_ctx,
    };
    let read = |f: &str| {
        std::fs::read(proc.join(pid.to_string()).join(f))
            .ok()
            .map(|b| String::from_utf8_lossy(&b).into_owned())
    };
    let (Some(cgroup), Some(status)) = (read("cgroup"), read("status")) else {
        return Some("?".into()); // gone already, or unreadable
    };
    let mut by_cgroup = None;
    for line in cgroup.lines() {
        if let Some(i) = line.find("/appzones.slice/") {
            let rest = &line[i + "/appzones.slice/".len()..];
            let name = rest.strip_prefix("appzones@").and_then(|r| {
                let end = r.find(".service")?;
                let after = &r[end + ".service".len()..];
                (after.is_empty() || after.starts_with('/')).then(|| &r[..end])
            });
            match name {
                Some(n) if zone_name_ok(n) => by_cgroup = Some(n.to_string()),
                _ => return Some("?".into()),
            }
        }
    }
    if let (Some(a), Some(b)) = (&by_ctx, &by_cgroup) {
        if a != b {
            return Some("?".into()); // two answers: name neither
        }
    }
    if by_ctx.is_some() || by_cgroup.is_some() {
        return by_ctx.or(by_cgroup);
    }
    let nested = status
        .lines()
        .find_map(|l| l.strip_prefix("NSpid:"))
        .is_some_and(|v| v.split_whitespace().count() > 1);
    nested.then(|| "?".into())
}

/// The sway command for window CON_ID of ZONE (a name or "?": nothing to quote).
pub fn mark_command(con_id: &Value, zone: &str) -> String {
    format!("[con_id={con_id}] mark --add \"zone:{zone}:{con_id}\", title_format \"[{zone}] %title\", shortcuts_inhibitor disable")
}

fn ask(s: &mut UnixStream, kind: u32, payload: &str) -> std::io::Result<Value> {
    send(s, kind, payload)?;
    recv(s)
}

fn mark(cmds: &mut UnixStream, w: &Value) -> std::io::Result<()> {
    let pid = w.get("pid").and_then(Value::as_i64);
    if let Some(z) = zone_of(pid, w, Path::new("/proc")) {
        if let Some(id) = w.get("id") {
            ask(cmds, RUN_COMMAND, &mark_command(id, &z))?;
        }
    }
    Ok(())
}

/// `appzones _sway-marks`: mark every sway window by its zone, those open now and each new one.
pub fn sway_hook() -> i32 {
    let Some(path) = std::env::var("SWAYSOCK").ok().filter(|s| !s.is_empty()) else {
        eprintln!("appzones-sway: no $SWAYSOCK (start it from the sway config)");
        return 1;
    };
    let run = || -> std::io::Result<i32> {
        let (mut cmds, mut events) = (UnixStream::connect(&path)?, UnixStream::connect(&path)?);
        if ask(&mut events, SUBSCRIBE, "[\"window\"]")?.get("success") != Some(&Value::Bool(true)) {
            eprintln!("appzones-sway: could not subscribe to window events");
            return Ok(1);
        }
        let mut stack = vec![ask(&mut cmds, GET_TREE, "")?];
        let mut open = Vec::new();
        while let Some(n) = stack.pop() {
            for k in ["floating_nodes", "nodes"] {
                if let Some(a) = n.get(k).and_then(Value::as_array) {
                    stack.extend(a.iter().rev().cloned());
                }
            }
            if n.get("pid").and_then(Value::as_i64).is_some_and(|p| p != 0) {
                open.push(n);
            }
        }
        for w in &open {
            if let Err(e) = mark(&mut cmds, w) {
                eprintln!("appzones-sway: marking a window: {e}");
            }
        }
        loop {
            let ev = match recv(&mut events) {
                Ok(v) => v,
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(0), // sway exited
                Err(e) => return Err(e),
            };
            if ev.get("change").and_then(Value::as_str) == Some("new") {
                if let Some(c) = ev.get("container").filter(|c| c.is_object()) {
                    // one window's failure is not the others': logged, and the hook goes on
                    if let Err(e) = mark(&mut cmds, c) {
                        eprintln!("appzones-sway: marking a window: {e}");
                    }
                }
            }
        }
    };
    match run() {
        Ok(rc) => rc,
        Err(e) => {
            eprintln!("appzones-sway: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zone_of_a_window() {
        let tmp = std::env::temp_dir().join(format!("az-sway-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let pid = |n: i64, cgroup: &str, nspid: &str| {
            let d = tmp.join(n.to_string());
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("cgroup"), format!("{cgroup}\n")).unwrap();
            std::fs::write(d.join("status"), format!("Name:\tfoot\nNSpid:\t{nspid}\n")).unwrap();
            Some(n)
        };
        let unit =
            "0::/user.slice/user-1000.slice/user@1000.service/appzones.slice/appzones@work.service";
        let host = pid(10, "0::/user.slice/user-1000.slice/session-2.scope", "7");
        let zone = pid(11, unit, "7");
        let below = pid(12, &format!("{unit}/app.scope"), "7");
        let odd = pid(
            13,
            "0::/user.slice/user@1000.service/appzones.slice/other.service",
            "7",
        );
        let nested = pid(14, "0::/user.slice/session-2.scope", "900 3");
        let ctx = |e: &str, a: &str| json!({"sandbox_engine": e, "sandbox_app_id": a});
        let work = ctx("dev.mirql.zones", "dev.mirql.zone.work");
        let none = json!({});
        let q = Some("?".to_string());
        let w = Some("work".to_string());
        let z = |p, win: &Value| zone_of(p, win, &tmp);
        for (got, want, why) in [
            (z(host, &none), None, "a host window"),
            (z(zone, &none), w.clone(), "the cgroup of a zone's unit"),
            (z(below, &none), w.clone(), "a cgroup below it"),
            (
                z(odd, &none),
                q.clone(),
                "under appzones.slice but not a zone's unit",
            ),
            (z(nested, &none), q.clone(), "a nested pid namespace"),
            (z(Some(99), &none), q.clone(), "a pid that is gone"),
            (z(nested, &work), w.clone(), "the security context names it"),
            (
                z(None, &work),
                w.clone(),
                "the security context without a pid",
            ),
            (z(zone, &work), w.clone(), "both agree"),
            (
                z(zone, &ctx("dev.mirql.zones", "dev.mirql.zone.home")),
                q.clone(),
                "the two disagree",
            ),
            (
                z(host, &ctx("org.flatpak", "org.mozilla.firefox")),
                q.clone(),
                "another sandbox",
            ),
            (
                z(host, &ctx("dev.mirql.zones", "dev.mirql.zone.Bad_Name")),
                q.clone(),
                "not a zone name",
            ),
            (z(None, &none), None, "no pid, no context"),
        ] {
            assert_eq!(got, want, "{why}");
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn the_mark_command() {
        assert_eq!(mark_command(&json!(42), "work"),
                   "[con_id=42] mark --add \"zone:work:42\", title_format \"[work] %title\", shortcuts_inhibitor disable");
    }
}
