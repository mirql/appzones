//! A zone's DNS (network = "internet"), the host side: the port of src/appzones/dns.py.
//!
//! zone-init answers DNS inside the zone and hands each query, one connection per query,
//! over the zone's dns.sock to this, in the zone's server on the host. Here the query goes
//! to the host's resolver (dns = "host") or to 1.1.1.1, then 1.0.0.1 (dns = "public"), and
//! the answer goes back. Only standard DNS queries leave the zone this way, and only to
//! that resolver. Framing on dns.sock, both ways: a 2-byte big-endian length, then the
//! message (as DNS over TCP).

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const PORT: u16 = 53;
const MAX_QUERY: usize = 4096; // a query is small; anything bigger is refused
const MAX_INFLIGHT: usize = 64; // queries answered at once, per zone
const RATE: f64 = 100.0; // queries a second, per zone, on average
const BURST: f64 = 200.0; // ... and at once after a quiet while
const UDP_TIMEOUT: Duration = Duration::from_secs(2);
const TCP_TIMEOUT: Duration = Duration::from_secs(4);
const DEADLINE: Duration = Duration::from_secs(8); // one query, every attempt included
const CONN_TIMEOUT: Duration = Duration::from_secs(5); // for zone-init to send its query

/// A standard query: a header, QR = 0, opcode QUERY, at least one question.
pub fn is_query(msg: &[u8]) -> bool {
    (12..=MAX_QUERY).contains(&msg.len())
        && msg[2] & 0x80 == 0
        && (msg[2] >> 3) & 0xF == 0
        && u16::from_be_bytes([msg[4], msg[5]]) >= 1
}

/// A token bucket: RATE queries a second on average, BURST at once.
pub struct Budget {
    rate: f64,
    burst: f64,
    state: Mutex<(f64, Instant)>,
}

impl Budget {
    pub fn new(rate: f64, burst: f64) -> Budget {
        Budget { rate, burst, state: Mutex::new((burst, Instant::now())) }
    }

    pub fn take_at(&self, now: Instant) -> bool {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let tokens = (st.0 + now.saturating_duration_since(st.1).as_secs_f64() * self.rate).min(self.burst);
        st.1 = now;
        if tokens < 1.0 {
            st.0 = tokens;
            return false;
        }
        st.0 = tokens - 1.0;
        true
    }
}

fn left(end: Instant) -> Option<Duration> {
    let d = end.saturating_duration_since(Instant::now());
    (!d.is_zero()).then_some(d)
}

fn addrs(server: &str) -> Vec<SocketAddr> {
    if let Ok(ip) = server.parse::<IpAddr>() {
        return vec![SocketAddr::new(ip, PORT)];
    }
    (server, PORT).to_socket_addrs().map(|a| a.collect()).unwrap_or_default()
}

fn ask_tcp(addr: SocketAddr, msg: &[u8], end: Instant) -> Option<Vec<u8>> {
    let t = left(end)?.min(TCP_TIMEOUT);
    let mut s = TcpStream::connect_timeout(&addr, t).ok()?;
    s.set_read_timeout(Some(t)).ok()?;
    s.set_write_timeout(Some(t)).ok()?;
    let mut out = (msg.len() as u16).to_be_bytes().to_vec();
    out.extend_from_slice(msg);
    s.write_all(&out).ok()?;
    let mut head = [0u8; 2];
    s.read_exact(&mut head).ok()?;
    let mut ans = vec![0u8; u16::from_be_bytes(head) as usize];
    s.read_exact(&mut ans).ok()?;
    (ans.len() >= 2 && ans[..2] == msg[..2]).then_some(ans)
}

/// MSG to SERVER over UDP, then over TCP when the answer was truncated; the answer, or
/// None. Only an answer with the query's id counts; nothing goes on past END.
fn ask(server: &str, msg: &[u8], end: Instant) -> Option<Vec<u8>> {
    for addr in addrs(server) {
        let bind: SocketAddr = if addr.is_ipv4() { "0.0.0.0:0".parse().ok()? } else { "[::]:0".parse().ok()? };
        let Ok(s) = UdpSocket::bind(bind) else { continue };
        if s.connect(addr).is_err() || s.send(msg).is_err() {
            continue; // only the server's own replies come in
        }
        let udp_end = end.min(Instant::now() + UDP_TIMEOUT);
        let mut buf = vec![0u8; 65535];
        let ans = loop {
            let Some(t) = left(udp_end) else { break None };
            if s.set_read_timeout(Some(t)).is_err() {
                break None;
            }
            match s.recv(&mut buf) {
                Ok(n) if n >= 12 && buf[..2] == msg[..2] => break Some(buf[..n].to_vec()),
                Ok(_) => continue,
                Err(_) => break None,
            }
        };
        let Some(ans) = ans else { continue };
        if ans[2] & 0x02 != 0 {
            return Some(ask_tcp(addr, msg, end).unwrap_or(ans)); // TC: the whole answer over TCP
        }
        return Some(ans);
    }
    None
}

/// The answer from the first server that gives one (two rounds), within DEADLINE.
pub fn resolve(servers: &[String], msg: &[u8]) -> Option<Vec<u8>> {
    let end = Instant::now() + DEADLINE;
    for _ in 0..2 {
        for server in servers {
            if Instant::now() >= end {
                return None;
            }
            if let Some(ans) = ask(server, msg, end) {
                return Some(ans);
            }
        }
    }
    None
}

fn handle(mut conn: UnixStream, servers: &[String], budget: &Budget) {
    let _ = conn.set_read_timeout(Some(CONN_TIMEOUT));
    let _ = conn.set_write_timeout(Some(CONN_TIMEOUT));
    let mut head = [0u8; 2];
    if conn.read_exact(&mut head).is_err() {
        return;
    }
    let n = u16::from_be_bytes(head) as usize;
    if n > MAX_QUERY {
        return;
    }
    let mut msg = vec![0u8; n];
    if conn.read_exact(&mut msg).is_err() || !is_query(&msg) || !budget.take_at(Instant::now()) {
        return; // not a query, or over the zone's budget: the client asks again later
    }
    if let Some(ans) = resolve(servers, &msg) {
        let mut out = (ans.len() as u16).to_be_bytes().to_vec();
        out.extend_from_slice(&ans);
        let _ = conn.write_all(&out);
    }
}

/// Listen on PATH (the zone's dns.sock, host side) and answer in threads until the
/// process ends.
pub fn serve(path: &Path, servers: Vec<String>) -> std::io::Result<()> {
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    let servers = Arc::new(servers);
    let budget = Arc::new(Budget::new(RATE, BURST));
    let inflight = Arc::new(AtomicUsize::new(0));
    thread::Builder::new().name("dns".into()).spawn(move || {
        for conn in listener.incoming() {
            let Ok(conn) = conn else { continue };
            if inflight.fetch_add(1, Ordering::SeqCst) >= MAX_INFLIGHT {
                inflight.fetch_sub(1, Ordering::SeqCst);
                continue; // too many at once: the client retries
            }
            let (servers, budget, inflight) = (servers.clone(), budget.clone(), inflight.clone());
            let spawned = thread::Builder::new().spawn(move || {
                handle(conn, &servers, &budget);
                inflight.fetch_sub(1, Ordering::SeqCst);
            });
            if spawned.is_err() {
                // (the closure, and with it the slot, is gone: give the slot back)
            }
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(id: u16, flags: u8, qd: u16) -> Vec<u8> {
        let mut m = id.to_be_bytes().to_vec();
        m.extend_from_slice(&[flags, 0]);
        m.extend_from_slice(&qd.to_be_bytes());
        m.extend_from_slice(&[0; 6]);
        m.extend_from_slice(b"\x07example\x03com\x00\x00\x01\x00\x01");
        m
    }

    #[test]
    fn queries() {
        assert!(is_query(&query(1, 0x01, 1)));
        assert!(!is_query(&query(1, 0x81, 1)), "a response");
        assert!(!is_query(&query(1, 0x10, 1)), "opcode 2 (status)");
        assert!(!is_query(&query(1, 0x01, 0)), "no question");
        assert!(!is_query(&[0u8; 11]), "shorter than a header");
        assert!(!is_query(&vec![0u8; MAX_QUERY + 1]));
    }

    #[test]
    fn budget() {
        let b = Budget::new(10.0, 3.0);
        let t = Instant::now();
        assert!(b.take_at(t) && b.take_at(t) && b.take_at(t), "the burst");
        assert!(!b.take_at(t), "then none");
        assert!(b.take_at(t + Duration::from_millis(150)), "one more after 0.1 s at 10/s");
        assert!(!b.take_at(t + Duration::from_millis(150)));
    }
}
