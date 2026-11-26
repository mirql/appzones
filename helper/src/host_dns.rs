//! A zone's DNS (network = "internet"), the host side: the port of src/appzones/dns.py.
//!
//! zone-init answers DNS inside the zone and hands each query, one connection per query,
//! over the zone's dns.sock to this, in the zone's server on the host. Here the query goes
//! to the host's resolver (dns = "host") or to 1.1.1.1, then 1.0.0.1 (dns = "public"), and
//! the answer goes back. Only standard DNS queries leave the zone this way, and only to
//! that resolver. Framing on dns.sock, both ways: a 2-byte big-endian length, then the
//! message (as DNS over TCP).
//!
//! When nothing answers, the zone only sees timeouts; so the server is told why (Miss):
//! once at the zone's start (a probe query), and when queries start to be refused on the
//! host (a VPN's or a firewall's DNS leak protection lets DNS go to its own resolver only:
//! the send fails with EPERM).

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
        Budget {
            rate,
            burst,
            state: Mutex::new((burst, Instant::now())),
        }
    }

    pub fn take_at(&self, now: Instant) -> bool {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let tokens =
            (st.0 + now.saturating_duration_since(st.1).as_secs_f64() * self.rate).min(self.burst);
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

/// Where queries go, and how long they may take (the constants, but in a test).
#[derive(Clone, Copy)]
struct Limits {
    port: u16,
    deadline: Duration,
    udp_timeout: Duration,
}

const LIMITS: Limits = Limits {
    port: PORT,
    deadline: DEADLINE,
    udp_timeout: UDP_TIMEOUT,
};

fn addrs(server: &str, port: u16) -> Vec<SocketAddr> {
    if let Ok(ip) = server.parse::<IpAddr>() {
        return vec![SocketAddr::new(ip, port)];
    }
    (server, port)
        .to_socket_addrs()
        .map(|a| a.collect())
        .unwrap_or_default()
}

/// The time left before END, at most TCP_TIMEOUT, set on S for its next call; None when
/// none is left (each call gets only what remains of the query's deadline).
fn arm(s: &TcpStream, end: Instant) -> Option<()> {
    let t = left(end)?.min(TCP_TIMEOUT);
    s.set_read_timeout(Some(t)).ok()?;
    s.set_write_timeout(Some(t)).ok()
}

fn write_all_by(s: &mut TcpStream, mut data: &[u8], end: Instant) -> Option<()> {
    while !data.is_empty() {
        arm(s, end)?;
        let n = s.write(data).ok().filter(|n| *n > 0)?;
        data = &data[n..];
    }
    Some(())
}

fn read_exact_by(s: &mut TcpStream, buf: &mut [u8], end: Instant) -> Option<()> {
    let mut got = 0;
    while got < buf.len() {
        arm(s, end)?;
        let n = s.read(&mut buf[got..]).ok().filter(|n| *n > 0)?;
        got += n;
    }
    Some(())
}

fn ask_tcp(addr: SocketAddr, msg: &[u8], end: Instant) -> Option<Vec<u8>> {
    let mut s = TcpStream::connect_timeout(&addr, left(end)?.min(TCP_TIMEOUT)).ok()?;
    let mut out = (msg.len() as u16).to_be_bytes().to_vec();
    out.extend_from_slice(msg);
    write_all_by(&mut s, &out, end)?;
    let mut head = [0u8; 2];
    read_exact_by(&mut s, &mut head, end)?;
    let mut ans = vec![0u8; u16::from_be_bytes(head) as usize];
    read_exact_by(&mut s, &mut ans, end)?;
    (ans.len() >= 2 && ans[..2] == msg[..2]).then_some(ans)
}

/// Why a query got no answer: REFUSED when every attempt was refused on this host
/// (EPERM, EACCES: a firewall); WHY says so, for a person ("1.1.1.1:53 and 1.0.0.1:53
/// refused on this host (Operation not permitted)", "no answer from 1.1.1.1:53").
#[derive(Debug, Clone, PartialEq)]
pub struct Miss {
    pub refused: bool,
    pub why: String,
}

/// What one attempt came to, when no answer: the address, and the error of its send.
type Tried = Vec<(SocketAddr, Option<std::io::Error>)>;

fn os_text(e: &std::io::Error) -> String {
    let t = e.to_string();
    t.split(" (os error").next().unwrap_or(&t).to_string()
}

fn miss(tried: &Tried, servers: &[String]) -> Miss {
    let mut addrs: Vec<String> = Vec::new();
    for (a, _) in tried {
        if !addrs.contains(&a.to_string()) {
            addrs.push(a.to_string());
        }
    }
    if addrs.is_empty() {
        addrs = servers.to_vec(); // (a name that does not resolve: nothing was tried)
    }
    let mut errs: Vec<String> = Vec::new();
    for e in tried.iter().filter_map(|(_, e)| e.as_ref()) {
        if !errs.contains(&os_text(e)) {
            errs.push(os_text(e));
        }
    }
    let refused = !tried.is_empty()
        && tried.iter().all(|(_, e)| {
            e.as_ref()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied)
        });
    let why = if refused {
        format!(
            "{} refused on this host ({})",
            addrs.join(" and "),
            errs.join("; ")
        )
    } else if errs.is_empty() {
        format!("no answer from {}", addrs.join(" or "))
    } else {
        format!(
            "no answer from {} ({})",
            addrs.join(" or "),
            errs.join("; ")
        )
    };
    Miss { refused, why }
}

/// MSG to SERVER over UDP, then over TCP when the answer was truncated; the answer, or
/// None. Only an answer with the query's id counts; nothing goes on past END. Each
/// address tried without an answer goes in TRIED.
fn ask(server: &str, msg: &[u8], end: Instant, lim: Limits, tried: &mut Tried) -> Option<Vec<u8>> {
    for addr in addrs(server, lim.port) {
        let bind: SocketAddr = if addr.is_ipv4() {
            "0.0.0.0:0".parse().ok()?
        } else {
            "[::]:0".parse().ok()?
        };
        let Ok(s) = UdpSocket::bind(bind) else {
            continue;
        };
        if let Err(e) = s.connect(addr).and_then(|_| s.send(msg)) {
            tried.push((addr, Some(e)));
            continue; // only the server's own replies come in
        }
        let udp_end = end.min(Instant::now() + lim.udp_timeout);
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
        let Some(ans) = ans else {
            tried.push((addr, None));
            continue;
        };
        if ans[2] & 0x02 != 0 {
            return Some(ask_tcp(addr, msg, end).unwrap_or(ans)); // TC: the whole answer over TCP
        }
        return Some(ans);
    }
    None
}

/// The answer from the first server that gives one (two rounds), within DEADLINE; else why not.
pub fn resolve(servers: &[String], msg: &[u8]) -> Result<Vec<u8>, Miss> {
    resolve_with(servers, msg, LIMITS)
}

fn resolve_with(servers: &[String], msg: &[u8], lim: Limits) -> Result<Vec<u8>, Miss> {
    let end = Instant::now() + lim.deadline;
    let mut tried = Tried::new();
    'rounds: for _ in 0..2 {
        for server in servers {
            if Instant::now() >= end {
                break 'rounds;
            }
            if let Some(ans) = ask(server, msg, end, lim, &mut tried) {
                return Ok(ans);
            }
        }
    }
    Err(miss(&tried, servers))
}

/// A query for the root's name servers: any answer to it (even a refusal) says the
/// resolver is reached.
pub fn probe_query() -> Vec<u8> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let id = (std::process::id() as u16) ^ (nanos >> 8) as u16;
    let mut q = id.to_be_bytes().to_vec();
    q.extend_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 1]);
    q
}

/// SERVERS reached from here: Ok, or why not (the probe query, within DEADLINE).
pub fn reach(servers: &[String]) -> Result<(), Miss> {
    resolve(servers, &probe_query()).map(|_| ())
}

/// MISS for a person, with the zone's DNS setting: `dns = "public": 1.1.1.1:53 and 1.0.0.1:53
/// refused on this host (Operation not permitted); ...` (the zone's log, `check --probe`).
pub fn said(dns: &str, m: &Miss) -> String {
    let hint = if !m.refused {
        ""
    } else if dns == "public" {
        "; a VPN's or a firewall's DNS leak protection? dns = \"host\" asks the host's resolver"
    } else {
        "; a firewall?"
    };
    format!("dns = \"{dns}\": {}{hint}", m.why)
}

/// Told why the zone's DNS gets no answer (the zone's server logs it).
pub type Tell = Arc<dyn Fn(&Miss) + Send + Sync>;

fn handle(
    mut conn: UnixStream,
    servers: &[String],
    budget: &Budget,
    tell: &Tell,
    told: &AtomicBool,
) {
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
    match resolve(servers, &msg) {
        Ok(ans) => {
            told.store(false, Ordering::SeqCst);
            let mut out = (ans.len() as u16).to_be_bytes().to_vec();
            out.extend_from_slice(&ans);
            let _ = conn.write_all(&out);
        }
        // refused on the host: said once, until a query is answered again (no answer in
        // time can be a slow network; that is not said for each query)
        Err(m) if m.refused && !told.swap(true, Ordering::SeqCst) => tell(&m),
        Err(_) => {}
    }
}

/// Listen on PATH (the zone's dns.sock, host side) and answer in threads until the
/// process ends. TELL is told when SERVERS do not answer the probe query at the start,
/// and when queries start to be refused on this host.
pub fn serve(path: &Path, servers: Vec<String>, tell: Tell) -> std::io::Result<()> {
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    let servers = Arc::new(servers);
    let budget = Arc::new(Budget::new(RATE, BURST));
    let inflight = Arc::new(AtomicUsize::new(0));
    let told = Arc::new(AtomicBool::new(false));
    let (servers0, tell0, told0) = (servers.clone(), tell.clone(), told.clone());
    thread::Builder::new().name("dns".into()).spawn(move || {
        for conn in listener.incoming() {
            let conn = match conn {
                Ok(c) => c,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    // out of descriptors or the like: wait a moment, never spin
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }
            };
            if inflight.fetch_add(1, Ordering::SeqCst) >= MAX_INFLIGHT {
                inflight.fetch_sub(1, Ordering::SeqCst);
                continue; // too many at once: the client retries
            }
            let (servers2, budget2, inflight2) =
                (servers.clone(), budget.clone(), inflight.clone());
            let (tell2, told2) = (tell.clone(), told.clone());
            let spawned = thread::Builder::new().spawn(move || {
                handle(conn, &servers2, &budget2, &tell2, &told2);
                inflight2.fetch_sub(1, Ordering::SeqCst);
            });
            if spawned.is_err() {
                inflight.fetch_sub(1, Ordering::SeqCst); // no worker: its slot back
            }
        }
    })?;
    // the start's probe, after the listener: only a diagnosis, so a thread that cannot
    // be had never stops the zone
    let _ = thread::Builder::new()
        .name("dns-probe".into())
        .spawn(move || {
            match reach(&servers0) {
                Err(m) if m.refused && told0.swap(true, Ordering::SeqCst) => {} // (a query said it first)
                Err(m) => tell0(&m),
                Ok(()) => {}
            }
        });
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

    /// Q answered with N A records (a response; TC: truncated).
    fn answer(q: &[u8], n: u16, tc: bool) -> Vec<u8> {
        let qend = 12 + q[12..].iter().position(|b| *b == 0).unwrap() + 1 + 4;
        let mut a = q[..2].to_vec();
        a.extend_from_slice(&(0x8180u16 | if tc { 0x0200 } else { 0 }).to_be_bytes());
        for x in [1u16, n, 0, 0] {
            a.extend_from_slice(&x.to_be_bytes());
        }
        a.extend_from_slice(&q[12..qend]);
        for i in 0..n {
            a.extend_from_slice(&[
                0xc0,
                0x0c,
                0,
                1,
                0,
                1,
                0,
                0,
                0,
                60,
                0,
                4,
                192,
                0,
                2,
                (i % 250) as u8,
            ]);
        }
        a
    }

    fn named(id: u16, name: &str) -> Vec<u8> {
        let mut q = id.to_be_bytes().to_vec();
        q.extend_from_slice(&[1, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
        for part in name.split('.') {
            q.push(part.len() as u8);
            q.extend_from_slice(part.as_bytes());
        }
        q.extend_from_slice(&[0, 0, 1, 0, 1]);
        q
    }

    #[test]
    fn resolver() {
        // a fake resolver: UDP truncates big answers, TCP gives them whole; a wrong id first
        // (a free UDP port need not be free for TCP: tried again until both are)
        let (udp, tcp, port) = (0..50)
            .find_map(|_| {
                let udp = UdpSocket::bind("127.0.0.1:0").ok()?;
                let port = udp.local_addr().ok()?.port();
                let tcp = std::net::TcpListener::bind(("127.0.0.1", port)).ok()?;
                Some((udp, tcp, port))
            })
            .expect("a port free for UDP and TCP");
        let count = |q: &[u8]| {
            if q.windows(3).any(|w| w == b"big") {
                200
            } else {
                2
            }
        };
        thread::spawn(move || loop {
            let mut buf = [0u8; 4096];
            let Ok((n, from)) = udp.recv_from(&mut buf) else {
                return;
            };
            let q = buf[..n].to_vec();
            let mut wrong = answer(&q, 1, false);
            wrong[..2].copy_from_slice(&[0xff, 0xff]); // someone else's id: ignored
            let _ = udp.send_to(&wrong, from);
            let n = count(&q);
            let _ = udp.send_to(
                &if n > 20 {
                    answer(&q, 0, true)
                } else {
                    answer(&q, n, false)
                },
                from,
            );
        });
        thread::spawn(move || {
            for c in tcp.incoming() {
                let Ok(mut c) = c else { return };
                let mut head = [0u8; 2];
                c.read_exact(&mut head).unwrap();
                let mut q = vec![0u8; u16::from_be_bytes(head) as usize];
                c.read_exact(&mut q).unwrap();
                let a = answer(&q, count(&q), false);
                c.write_all(&(a.len() as u16).to_be_bytes()).unwrap();
                c.write_all(&a).unwrap();
            }
        });
        let lim = Limits { port, ..LIMITS };
        let local = vec!["127.0.0.1".to_string()];
        assert!(
            reach_with(&local, lim).is_ok(),
            "the probe query: any answer"
        );
        let r = resolve_with(&local, &named(0x1111, "example.com"), lim).unwrap();
        assert_eq!(
            (&r[..2], u16::from_be_bytes([r[6], r[7]])),
            (&[0x11u8, 0x11][..], 2)
        );
        let big = resolve_with(&local, &named(0x2222, "big.example"), lim).unwrap(); // whole over TCP
        assert_eq!(u16::from_be_bytes([big[6], big[7]]), 200);
        assert!(big.len() > 512);
        // one query never takes longer than the deadline, also when nothing answers
        let silent = UdpSocket::bind("127.0.0.1:0").unwrap();
        let lim = Limits {
            port: silent.local_addr().unwrap().port(),
            deadline: Duration::from_millis(600),
            udp_timeout: Duration::from_millis(250),
        };
        let t0 = Instant::now();
        let m = resolve_with(
            &[local[0].clone(), local[0].clone()],
            &named(5, "example.com"),
            lim,
        )
        .unwrap_err();
        assert!(
            t0.elapsed() < Duration::from_millis(900),
            "{:?}",
            t0.elapsed()
        );
        assert_eq!(
            m,
            Miss {
                refused: false,
                why: format!("no answer from 127.0.0.1:{}", lim.port)
            }
        );
    }

    fn reach_with(servers: &[String], lim: Limits) -> Result<(), Miss> {
        resolve_with(servers, &probe_query(), lim).map(|_| ())
    }

    #[test]
    fn misses() {
        use std::io::Error;
        let a: SocketAddr = "1.1.1.1:53".parse().unwrap();
        let b: SocketAddr = "1.0.0.1:53".parse().unwrap();
        let eperm = || Some(Error::from_raw_os_error(libc::EPERM));
        let servers = ["1.1.1.1".to_string(), "1.0.0.1".to_string()];
        // a VPN's leak protection: every send refused, both rounds
        let m = miss(
            &vec![(a, eperm()), (b, eperm()), (a, eperm()), (b, eperm())],
            &servers,
        );
        assert_eq!(
            m,
            Miss {
                refused: true,
                why: "1.1.1.1:53 and 1.0.0.1:53 refused on this host (Operation not permitted)"
                    .into()
            }
        );
        // one of them answers nothing: not refused (that one may be a slow network)
        let m = miss(&vec![(a, eperm()), (b, None)], &servers);
        assert_eq!(
            m,
            Miss {
                refused: false,
                why: "no answer from 1.1.1.1:53 or 1.0.0.1:53 (Operation not permitted)".into()
            }
        );
        let m = miss(
            &vec![(a, Some(Error::from_raw_os_error(libc::ENETUNREACH)))],
            &servers[..1],
        );
        assert!(
            !m.refused && m.why.starts_with("no answer from 1.1.1.1:53 ("),
            "{m:?}"
        );
        let m = miss(&vec![], &["dns.invalid".to_string()]);
        assert_eq!(
            m,
            Miss {
                refused: false,
                why: "no answer from dns.invalid".into()
            }
        );
        assert_eq!(probe_query().len(), 17);
        assert!(is_query(&probe_query()));
        let m = Miss {
            refused: true,
            why: "1.1.1.1:53 refused on this host (Operation not permitted)".into(),
        };
        assert_eq!(said("public", &m), "dns = \"public\": 1.1.1.1:53 refused on this host (Operation not permitted); \
                                       a VPN's or a firewall's DNS leak protection? dns = \"host\" asks the host's resolver");
        assert_eq!(
            said(
                "host",
                &Miss {
                    refused: false,
                    why: "no answer from 127.0.0.53:53".into()
                }
            ),
            "dns = \"host\": no answer from 127.0.0.53:53"
        );
    }

    #[test]
    fn budget() {
        let b = Budget::new(10.0, 3.0);
        let t = Instant::now();
        assert!(b.take_at(t) && b.take_at(t) && b.take_at(t), "the burst");
        assert!(!b.take_at(t), "then none");
        assert!(
            b.take_at(t + Duration::from_millis(150)),
            "one more after 0.1 s at 10/s"
        );
        assert!(!b.take_at(t + Duration::from_millis(150)));
    }
}
