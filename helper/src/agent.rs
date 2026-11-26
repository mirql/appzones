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
pub fn request(
    rt: &std::path::Path,
    name: &str,
    req: &Value,
    fds: &[RawFd],
    timeout: Option<Duration>,
) -> Result<UnixStream, String> {
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
pub fn connect_within(path: &std::path::Path, t: Duration) -> Result<UnixStream, String> {
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
            let fd = crate::sys::unix_stream_nonblock();
            if fd < 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            let len =
                (std::mem::size_of::<libc::sa_family_t>() + bytes.len() + 1) as libc::socklen_t;
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

/// The most descriptors one request passes (the control buffer below holds them).
const MAX_FDS: usize = 8;
/// The longest reply line read (a ps of a busy zone is far below it).
const MAX_REPLY: usize = 8 << 20;

/// DATA with FDS (SCM_RIGHTS) on S, all of it: what sendmsg did not take goes after it,
/// without the descriptors again.
fn send_with_fds(s: &UnixStream, data: &[u8], fds: &[RawFd]) -> std::io::Result<()> {
    if fds.is_empty() || fds.len() > MAX_FDS {
        return Err(std::io::Error::other(format!(
            "{} descriptors in one request (1 to {MAX_FDS})",
            fds.len()
        )));
    }
    let mut iov = libc::iovec {
        iov_base: data.as_ptr() as *mut _,
        iov_len: data.len(),
    };
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
        loop {
            let n = libc::sendmsg(s.as_raw_fd(), &msg, libc::MSG_NOSIGNAL);
            if n >= 0 || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                break n;
            }
        }
    };
    if n < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut w = s;
    w.write_all(&data[n as usize..])
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
        if buf.len() > MAX_REPLY && !buf.contains(&b'\n') {
            return Err(format!("a reply longer than {} MiB", MAX_REPLY >> 20));
        }
    }
    let (line, rest) = match buf.iter().position(|b| *b == b'\n') {
        Some(i) => (buf[..i].to_vec(), buf[i + 1..].to_vec()),
        None => (buf, Vec::new()),
    };
    let text = String::from_utf8_lossy(&line);
    let v = if text.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(text.trim()).map_err(|e| e.to_string())?
    };
    Ok((v, rest))
}

/// One request, its first reply line.
pub fn ask(
    rt: &std::path::Path,
    name: &str,
    req: &Value,
    timeout: Duration,
) -> Result<Value, String> {
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
    Ok(r.get("procs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p.get(0).is_some_and(|pid| *pid != json!(1) && *pid != init))
        .collect())
}

/// One request; {} when zone-init does not answer or does not know the op (agent.zone_request).
pub fn zone_request(rt: &std::path::Path, name: &str, req: &Value) -> Value {
    match ask(rt, name, req, Duration::from_secs(10)) {
        Ok(r) if r.get("ok") == Some(&Value::Bool(true)) => r,
        _ => json!({}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn readline_keeps_the_rest() {
        // two replies in one read: the second is not lost (appzones run --wait's exit code)
        let (mut a, b) = UnixStream::pair().unwrap();
        a.write_all(b"{\"ok\": true, \"pid\": 7}\n{\"exit\": 0}\n")
            .unwrap();
        a.shutdown(std::net::Shutdown::Write).unwrap();
        let (first, rest) = readline(&b, Vec::new()).unwrap();
        assert_eq!(first, serde_json::json!({"ok": true, "pid": 7}));
        assert_eq!(
            readline(&b, rest).unwrap().0,
            serde_json::json!({"exit": 0})
        );
    }
}
