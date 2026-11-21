//! Talking to zone-init, the agent inside a zone, over its host-made socket: the port of
//! src/appzones/agent.py (a JSON line each way; descriptors for `run --wait` as SCM_RIGHTS).

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Value};

use crate::policy::{py_repr, zone_name_ok};

/// Where the host side of zone NAME's agent socket is (never in the zone).
pub fn agent_path(rt: &std::path::Path, name: &str) -> Result<PathBuf, String> {
    if !zone_name_ok(name) {
        return Err(format!("not a zone name: {}", py_repr(name)));
    }
    Ok(rt.join(name).join("agent.sock"))
}

/// A connection with REQ sent (and FDS with it); None when the zone does not answer.
pub fn request(rt: &std::path::Path, name: &str, req: &Value, fds: &[RawFd], timeout: Option<Duration>)
    -> Result<UnixStream, String> {
    let path = agent_path(rt, name)?;
    let s = match timeout {
        Some(t) => connect_within(&path, t)?,
        None => UnixStream::connect(&path).map_err(|e| e.to_string())?,
    };
    s.set_read_timeout(timeout).map_err(|e| e.to_string())?;
    s.set_write_timeout(timeout).map_err(|e| e.to_string())?;
    let data = format!("{req}\n");
    if fds.is_empty() {
        (&s).write_all(data.as_bytes()).map_err(|e| e.to_string())?;
    } else {
        send_with_fds(&s, data.as_bytes(), fds).map_err(|e| e.to_string())?;
    }
    Ok(s)
}

/// A connection within T: a full queue (a stalled zone-init) is waited for that long, never
/// forever (a blocking connect would wait for zone-init to accept).
fn connect_within(path: &std::path::Path, t: Duration) -> Result<UnixStream, String> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    let end = std::time::Instant::now() + t;
    loop {
        // Safety: a sockaddr_un filled within its size; the socket is ours (wrapped or closed).
        let r = unsafe {
            let mut sa: libc::sockaddr_un = std::mem::zeroed();
            if bytes.len() >= sa.sun_path.len() {
                return Err("path too long".into());
            }
            sa.sun_family = libc::AF_UNIX as libc::sa_family_t;
            for (d, b) in sa.sun_path.iter_mut().zip(bytes) {
                *d = *b as libc::c_char;
            }
            let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC, 0);
            if fd < 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            let len = (std::mem::size_of::<libc::sa_family_t>() + bytes.len() + 1) as libc::socklen_t;
            if libc::connect(fd, (&sa as *const libc::sockaddr_un).cast(), len) == 0 {
                Ok(<UnixStream as std::os::fd::FromRawFd>::from_raw_fd(fd))
            } else {
                let e = std::io::Error::last_os_error();
                libc::close(fd);
                Err(e)
            }
        };
        match r {
            Ok(s) => {
                s.set_nonblocking(false).map_err(|e| e.to_string())?;
                return Ok(s);
            }
            Err(e) if e.raw_os_error() == Some(libc::EAGAIN) && std::time::Instant::now() < end => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn send_with_fds(s: &UnixStream, data: &[u8], fds: &[RawFd]) -> std::io::Result<()> {
    let mut iov = libc::iovec { iov_base: data.as_ptr() as *mut _, iov_len: data.len() };
    let mut cmsg = vec![0u64; 8];
    let len = std::mem::size_of_val(fds) as u32;
    // Safety: a msghdr over our buffers with one SCM_RIGHTS message of FDS.
    let n = unsafe {
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cmsg.as_mut_ptr().cast();
        msg.msg_controllen = libc::CMSG_SPACE(len) as _;
        let c = libc::CMSG_FIRSTHDR(&msg);
        (*c).cmsg_level = libc::SOL_SOCKET;
        (*c).cmsg_type = libc::SCM_RIGHTS;
        (*c).cmsg_len = libc::CMSG_LEN(len) as _;
        std::ptr::copy_nonoverlapping(fds.as_ptr(), libc::CMSG_DATA(c).cast::<RawFd>(), fds.len());
        libc::sendmsg(s.as_raw_fd(), &msg, libc::MSG_NOSIGNAL)
    };
    if n < 0 { Err(std::io::Error::last_os_error()) } else { Ok(()) }
}

/// One JSON line from S; it and the bytes after it (pass them back as BUF for the next).
pub fn readline(mut s: &UnixStream, mut buf: Vec<u8>) -> Result<(Value, Vec<u8>), String> {
    while !buf.contains(&b'\n') {
        let mut chunk = [0u8; 65536];
        let n = s.read(&mut chunk).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let (line, rest) = match buf.iter().position(|b| *b == b'\n') {
        Some(i) => (buf[..i].to_vec(), buf[i + 1..].to_vec()),
        None => (buf, Vec::new()),
    };
    let text = String::from_utf8_lossy(&line);
    let v = if text.trim().is_empty() { json!({}) } else { serde_json::from_str(text.trim()).map_err(|e| e.to_string())? };
    Ok((v, rest))
}

/// One request, its first reply line.
pub fn ask(rt: &std::path::Path, name: &str, req: &Value, timeout: Duration) -> Result<Value, String> {
    let s = request(rt, name, req, &[], Some(timeout))?;
    Ok(readline(&s, Vec::new())?.0)
}

/// zone-init's answer to ping, None when it does not answer (agent.ping).
pub fn ping(rt: &std::path::Path, name: &str) -> Option<Value> {
    ask(rt, name, &json!({"op": "ping"}), Duration::from_secs(1)).ok()
}

/// The zone answers: a non-empty answer (Python's truth of the reply).
pub fn answers(rt: &std::path::Path, name: &str) -> bool {
    ping(rt, name).is_some_and(|v| v.as_object().is_none_or(|o| !o.is_empty()))
}

/// Processes in the zone other than bwrap (pid 1) and zone-init (agent.zone_procs).
pub fn zone_procs(rt: &std::path::Path, name: &str) -> Result<Vec<Value>, String> {
    let r = ask(rt, name, &json!({"op": "ps"}), Duration::from_secs(5))?;
    let init = r.get("init").cloned().unwrap_or(Value::Null);
    Ok(r.get("procs").and_then(Value::as_array).cloned().unwrap_or_default().into_iter()
        .filter(|p| p.get(0).is_some_and(|pid| *pid != json!(1) && *pid != init)).collect())
}

/// One request; {} when zone-init does not answer or does not know the op (agent.zone_request).
pub fn zone_request(rt: &std::path::Path, name: &str, req: &Value) -> Value {
    match ask(rt, name, req, Duration::from_secs(10)) {
        Ok(r) if r.get("ok") == Some(&Value::Bool(true)) => r,
        _ => json!({}),
    }
}
