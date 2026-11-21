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
    std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok().filter(|h| !h.is_empty()).is_none().then_some(s)
}

fn sway_focused(path: &str) -> std::io::Result<(Option<Value>, Vec<String>)> {
    let mut s = UnixStream::connect(path)?;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    send(&mut s, GET_TREE, "")?;
    let tree = recv(&mut s)?;
    let mut stack = vec![tree];
    while let Some(n) = stack.pop() {
        let truthy = |v: Option<&Value>| v.is_some_and(|v| !matches!(v, Value::Null | Value::Bool(false)) && *v != json!(0));
        if truthy(n.get("focused")) && truthy(n.get("pid")) {
            let marks = n.get("marks").and_then(Value::as_array).map(|m| m.iter().filter_map(|x| x.as_str().map(String::from))
                .collect()).unwrap_or_default();
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
        let Ok((con, marks)) = sway_focused(&path) else { return String::new() };
        let con = con.map(|c| c.to_string()).unwrap_or_else(|| "None".into());
        let zones: Vec<String> = marks.iter().filter(|m| m.starts_with("zone:")).map(|m| {
            let parts: Vec<&str> = m.split(':').collect();
            let ok = parts.len() == 3 && parts[2] == con && (parts[1] == "?" || zone_name_ok(parts[1]));
            if ok { parts[1].to_string() } else { "?".to_string() }
        }).collect();
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
    let tags: Vec<String> = std::process::Command::new("hyprctl").args(["-j", "activewindow"]).output().ok()
        .and_then(|o| serde_json::from_slice::<Value>(if o.stdout.is_empty() { b"{}" } else { &o.stdout }).ok())
        .and_then(|v| v.get("tags").and_then(Value::as_array).cloned())
        .map(|t| t.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    tags.iter().find_map(|t| t.strip_prefix("zone:").map(|z| z.trim_end_matches('*').to_string())).unwrap_or_default()
}

fn current(as_json: bool) -> String {
    let z = focused_zone();
    if !as_json {
        return if z.is_empty() { String::new() } else { format!("[{z}]") };
    }
    if z == "?" {
        return py_json(&json!({"text": "[?]", "class": "zone-unidentified",
                               "tooltip": "active window runs in a sandbox appzones cannot name"}));
    }
    let (text, class, tip) = if z.is_empty() { (String::new(), "host".to_string(), String::new()) }
        else { (format!("[{z}]"), format!("zone-{z}"), format!("active window runs in app zone {z}")) };
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
        let Ok(mut s) = UnixStream::connect(&path) else { return 1 };
        if send(&mut s, SUBSCRIBE, "[\"window\", \"workspace\"]").is_err() || recv(&mut s).is_err() {
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
    let Some(sig) = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok().filter(|s| !s.is_empty()) else {
        say(&current(as_json));
        eprintln!("appzones: no HYPRLAND_INSTANCE_SIGNATURE or SWAYSOCK");
        return 1;
    };
    let Ok(mut s) = UnixStream::connect(host_rt.join("hypr").join(sig).join(".socket2.sock")) else { return 1 };
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
        let Some(cut) = buf.iter().rposition(|b| *b == b'\n') else { continue };
        let lines: Vec<Vec<u8>> = buf[..cut].split(|b| *b == b'\n').map(<[u8]>::to_vec).collect();
        buf.drain(..=cut);
        if lines.iter().any(|l| l.starts_with(b"activewindowv2>>") || l.starts_with(b"windowtitlev2>>")
            || l.starts_with(b"closewindow>>")) {
            let cur = current(as_json);
            if cur != last {
                say(&cur);
                last = cur;
            }
        }
    }
}
