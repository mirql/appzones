//! The zone's DNS (network = "internet"), the zone side: zone-init answers at $ZONE_DNS
//! (127.0.0.53), UDP and TCP, and hands each query over /run/zone/dns.sock to the zone's
//! server on the host (appzones/dns.py), which asks the host's resolver. The port of the
//! DNS part of zone_init.py: same framing (a 2-byte big-endian length, then the message),
//! same limits.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

const DNS_SOCK: &str = "/run/zone/dns.sock";
const DNS_MAX: usize = 4096;
const MAX_INFLIGHT: usize = 64;
static INFLIGHT: AtomicUsize = AtomicUsize::new(0);

fn take_slot() -> bool {
    let mut n = INFLIGHT.load(Ordering::Acquire);
    while n < MAX_INFLIGHT {
        match INFLIGHT.compare_exchange_weak(n, n + 1, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return true,
            Err(now) => n = now,
        }
    }
    false
}

fn give_slot() {
    INFLIGHT.fetch_sub(1, Ordering::AcqRel);
}

/// The answer to MSG from the host side, or None.
pub fn forward(msg: &[u8]) -> Option<Vec<u8>> {
    // longer than the host side's deadline for one query (8 s); also for the connect
    let mut s = connect(DNS_SOCK, Duration::from_secs(12))?;
    s.set_read_timeout(Some(Duration::from_secs(12))).ok()?;
    let len = u16::try_from(msg.len()).ok()?;
    s.write_all(&len.to_be_bytes()).ok()?;
    s.write_all(msg).ok()?;
    let mut head = [0u8; 2];
    s.read_exact(&mut head).ok()?;
    let mut ans = vec![0u8; u16::from_be_bytes(head) as usize];
    s.read_exact(&mut ans).ok()?;
    (!ans.is_empty()).then_some(ans)
}

/// A unix stream socket connected to PATH within TIMEOUT: a connect to a listener whose
/// backlog is full waits only as long as the socket's send timeout, so it is set first.
fn connect(path: &str, timeout: Duration) -> Option<UnixStream> {
    use std::os::fd::FromRawFd;
    let bytes = path.as_bytes();
    // Safety: a new socket we own, a sockaddr_un filled within its bounds, a timeval.
    unsafe {
        let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0);
        if fd < 0 {
            return None;
        }
        let s = UnixStream::from_raw_fd(fd); // closes fd on every return below
        let tv = libc::timeval { tv_sec: timeout.as_secs() as libc::time_t, tv_usec: 0 };
        libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_SNDTIMEO, (&tv as *const libc::timeval).cast(),
                         std::mem::size_of::<libc::timeval>() as libc::socklen_t);
        let mut addr: libc::sockaddr_un = std::mem::zeroed();
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        if bytes.len() >= addr.sun_path.len() {
            return None;
        }
        for (d, b) in addr.sun_path.iter_mut().zip(bytes) {
            *d = *b as libc::c_char;
        }
        let len = (std::mem::size_of::<libc::sa_family_t>() + bytes.len() + 1) as libc::socklen_t;
        if libc::connect(fd, (&addr as *const libc::sockaddr_un).cast(), len) != 0 {
            return None;
        }
        Some(s)
    }
}

/// The offset after the domain name at I (labels, or a compression pointer).
fn skip_name(msg: &[u8], mut i: usize) -> Option<usize> {
    while i < msg.len() {
        let n = msg[i] as usize;
        if n == 0 {
            return Some(i + 1);
        }
        if n & 0xC0 == 0xC0 {
            return Some(i + 2);
        }
        i += 1 + n;
    }
    None
}

fn u16_at(msg: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*msg.get(i)?, *msg.get(i + 1)?]))
}

/// The largest UDP answer the client takes: its EDNS size (OPT record), else 512.
pub fn udp_limit(query: &[u8]) -> usize {
    fn opt_size(q: &[u8]) -> Option<usize> {
        let qd = u16_at(q, 4)?;
        let rest = u16_at(q, 6)? as usize + u16_at(q, 8)? as usize + u16_at(q, 10)? as usize;
        let mut i = 12;
        for _ in 0..qd {
            i = skip_name(q, i)? + 4;
        }
        for _ in 0..rest {
            let j = skip_name(q, i)?;
            let (rtype, rclass) = (u16_at(q, j)?, u16_at(q, j + 2)?);
            if rtype == 41 {
                // OPT: the class is the UDP size
                return Some((rclass as usize).clamp(512, DNS_MAX));
            }
            i = j + 8 + 2 + u16_at(q, j + 8)? as usize;
        }
        None
    }
    opt_size(query).unwrap_or(512)
}

/// ANSWER cut to its header and question, with TC set: the client asks again over TCP.
pub fn truncate(answer: &[u8]) -> Vec<u8> {
    let flags = [answer.get(2).copied().unwrap_or(0) | 0x02, answer.get(3).copied().unwrap_or(0)];
    let head = |qd: u16| {
        let mut out = answer.get(..2).unwrap_or(&[0, 0]).to_vec();
        out.extend_from_slice(&flags);
        for n in [qd, 0, 0, 0] {
            out.extend_from_slice(&n.to_be_bytes());
        }
        out
    };
    let question = || -> Option<(u16, usize)> {
        let qd = u16_at(answer, 4)?;
        let mut i = 12;
        for _ in 0..qd {
            i = skip_name(answer, i)? + 4;
        }
        Some((qd, i))
    };
    match question() {
        Some((qd, end)) => {
            let mut out = head(qd);
            out.extend_from_slice(answer.get(12..end.min(answer.len())).unwrap_or(&[]));
            out
        }
        None => head(0),
    }
}

fn udp_one(sock: UdpSocket, query: Vec<u8>, from: SocketAddr) {
    if let Some(ans) = forward(&query) {
        let ans = if ans.len() <= udp_limit(&query) { ans } else { truncate(&ans) };
        let _ = sock.send_to(&ans, from);
    }
    give_slot();
}

fn udp_loop(sock: UdpSocket) {
    let mut buf = vec![0u8; DNS_MAX];
    loop {
        let Ok((n, from)) = sock.recv_from(&mut buf) else { continue };
        if n < 12 || !take_slot() {
            continue; // too many at once: the client asks again
        }
        let (Ok(s), query) = (sock.try_clone(), buf[..n].to_vec()) else {
            give_slot();
            continue;
        };
        if thread::Builder::new().spawn(move || udp_one(s, query, from)).is_err() {
            give_slot();
        }
    }
}

fn tcp_conn(mut conn: TcpStream) {
    let _ = conn.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = conn.set_write_timeout(Some(Duration::from_secs(10)));
    loop {
        let mut head = [0u8; 2];
        if conn.read_exact(&mut head).is_err() {
            break;
        }
        let n = u16::from_be_bytes(head) as usize;
        if n > DNS_MAX {
            break;
        }
        let mut query = vec![0u8; n];
        if conn.read_exact(&mut query).is_err() {
            break;
        }
        let Some(ans) = forward(&query) else { break };
        let Ok(len) = u16::try_from(ans.len()) else { break };
        if conn.write_all(&len.to_be_bytes()).and_then(|_| conn.write_all(&ans)).is_err() {
            break;
        }
    }
    give_slot();
}

fn tcp_loop(listener: TcpListener) {
    loop {
        let Ok((conn, _)) = listener.accept() else { continue };
        if !take_slot() {
            continue;
        }
        if thread::Builder::new().spawn(move || tcp_conn(conn)).is_err() {
            give_slot();
        }
    }
}

/// Answer DNS at ADDR:53, UDP and TCP, in threads. Says so when it cannot.
pub fn start(addr: &str, log: fn(&str)) {
    let bound = (|| -> std::io::Result<(UdpSocket, TcpListener)> {
        Ok((UdpSocket::bind((addr, 53))?, TcpListener::bind((addr, 53))?))
    })();
    let (udp, tcp) = match bound {
        Ok(b) => b,
        Err(e) => {
            log(&format!("DNS at {addr}:53 not started: {e}; names do not resolve in this zone"));
            return;
        }
    };
    let _ = thread::Builder::new().name("dns-udp".into()).spawn(move || udp_loop(udp));
    let _ = thread::Builder::new().name("dns-tcp".into()).spawn(move || tcp_loop(tcp));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(id: u16, name: &str, edns: Option<u16>) -> Vec<u8> {
        let mut q = id.to_be_bytes().to_vec();
        q.extend_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, edns.is_some() as u8]);
        for p in name.split('.') {
            q.push(p.len() as u8);
            q.extend_from_slice(p.as_bytes());
        }
        q.extend_from_slice(&[0, 0, 1, 0, 1]);
        if let Some(size) = edns {
            q.push(0);
            q.extend_from_slice(&41u16.to_be_bytes());
            q.extend_from_slice(&size.to_be_bytes());
            q.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        }
        q
    }

    #[test]
    fn udp_limit_from_edns() {
        assert_eq!(udp_limit(&query(1, "example.com", None)), 512);
        assert_eq!(udp_limit(&query(1, "example.com", Some(1232))), 1232);
        assert_eq!(udp_limit(&query(1, "example.com", Some(65000))), 4096);
        assert_eq!(udp_limit(&query(1, "example.com", Some(100))), 512);
        assert_eq!(udp_limit(&[0u8; 5]), 512);
    }

    #[test]
    fn truncate_keeps_header_and_question() {
        let q = query(0x2222, "big.example", None);
        let mut a = q.clone();
        a[2] |= 0x80;
        a[7] = 200; // an answer count
        a.extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 192, 0, 2, 1]);
        let t = truncate(&a);
        assert!(t[2] & 0x02 != 0);
        assert_eq!(&t[4..12], &[0, 1, 0, 0, 0, 0, 0, 0]);
        assert_eq!(&t[12..], &q[12..]);
        assert_eq!(&t[..2], &[0x22, 0x22]);
    }

    #[test]
    fn truncate_bad_question() {
        let t = truncate(&[1, 2, 3, 4, 0, 1, 0, 0, 0, 0, 0, 0, 63]);
        assert_eq!(t, vec![1, 2, 3 | 2, 4, 0, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn slots_limit() {
        let taken: Vec<bool> = (0..MAX_INFLIGHT + 1).map(|_| take_slot()).collect();
        assert!(taken[..MAX_INFLIGHT].iter().all(|t| *t));
        assert!(!taken[MAX_INFLIGHT]);
        for _ in 0..MAX_INFLIGHT {
            give_slot();
        }
    }
}
