//! appzones' own proxy for `network = "internet"` where a zone has no network namespace of
//! its own (macOS, design/macos.md, stage 3). The zone's profile lets it connect to this
//! one port on localhost and nothing else; the proxy reaches the internet for it, never
//! the host, the networks the host is directly connected to, private, CGNAT, link-local or
//! multicast ranges, nor those through NAT64: the list of the nft rules on Linux. Names are
//! resolved here, and the address connected to is the one checked (no second lookup).
//!
//! One port, three ways in (told by the first byte): HTTP CONNECT (HTTPS_PROXY), a plain
//! HTTP request with an absolute URL (HTTP_PROXY), SOCKS5 (ALL_PROXY=socks5h://). A
//! program that ignores the proxy variables gets no network at all: it fails closed.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Connections open at once, at most (more: refused until some end).
const MAX_CONNS: usize = 512;
/// A request's head: this long and this big, at most.
const HEAD_TIMEOUT: Duration = Duration::from_secs(15);
const HEAD_MAX: usize = 16 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Fixed ranges a zone never reaches (the nft rules' list), as (network, prefix).
const REFUSED4: &[([u8; 4], u8)] = &[([0, 0, 0, 0], 8), ([10, 0, 0, 0], 8), ([100, 64, 0, 0], 10), ([127, 0, 0, 0], 8),
    ([169, 254, 0, 0], 16), ([172, 16, 0, 0], 12), ([192, 0, 0, 0], 24), ([192, 168, 0, 0], 16), ([198, 18, 0, 0], 15),
    ([224, 0, 0, 0], 4), ([240, 0, 0, 0], 4)];
const NAT64: u128 = 0x0064_ff9b_u128 << 96;

fn in4(ip: Ipv4Addr, net: Ipv4Addr, prefix: u8) -> bool {
    let mask = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix) };
    u32::from(ip) & mask == u32::from(net) & mask
}

fn in6(ip: Ipv6Addr, net: Ipv6Addr, prefix: u8) -> bool {
    let mask = if prefix == 0 { 0 } else { u128::MAX << (128 - prefix) };
    u128::from(ip) & mask == u128::from(net) & mask
}

/// The host's addresses and the networks it is directly connected to: (address, prefix);
/// None when they cannot be read (then nothing is let through: never only the fixed list).
pub fn host_nets() -> Option<Vec<(IpAddr, u8)>> {
    let mut out = Vec::new();
    let mut ifa: *mut libc::ifaddrs = std::ptr::null_mut();
    // Safety: getifaddrs fills a list we walk and free; each sockaddr is read by its family.
    unsafe {
        if libc::getifaddrs(&mut ifa) != 0 {
            return None;
        }
        let mut p = ifa;
        while !p.is_null() {
            let (a, m) = ((*p).ifa_addr, (*p).ifa_netmask);
            if !a.is_null() {
                let ip_mask = match (*a).sa_family as libc::c_int {
                    libc::AF_INET => {
                        let ip = Ipv4Addr::from(u32::from_be((*a.cast::<libc::sockaddr_in>()).sin_addr.s_addr));
                        let mask = if m.is_null() { u32::MAX }
                                   else { u32::from_be((*m.cast::<libc::sockaddr_in>()).sin_addr.s_addr) };
                        Some((IpAddr::V4(ip), mask.count_ones() as u8))
                    }
                    libc::AF_INET6 => {
                        let ip = Ipv6Addr::from((*a.cast::<libc::sockaddr_in6>()).sin6_addr.s6_addr);
                        let mask = if m.is_null() { 128 }
                                   else { u128::from_be_bytes((*m.cast::<libc::sockaddr_in6>()).sin6_addr.s6_addr).count_ones() };
                        Some((IpAddr::V6(ip), mask as u8))
                    }
                    _ => None,
                };
                out.extend(ip_mask);
            }
            p = (*p).ifa_next;
        }
        libc::freeifaddrs(ifa);
    }
    Some(out)
}

/// Why the zone may not reach IP (a short reason for the log), None when it may. HOST: the
/// host's addresses and prefixes (host_nets): the host itself, and every network it is
/// directly connected to, whatever its addresses (a LAN with public ones too).
pub fn refused(ip: IpAddr, host: &[(IpAddr, u8)]) -> Option<&'static str> {
    let ip = match ip {
        // an IPv4-mapped address is the IPv4 address
        IpAddr::V6(a) => a.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    };
    match ip {
        IpAddr::V4(a) => {
            if host.iter().any(|(h, _)| *h == IpAddr::V4(a)) {
                return Some("the host");
            }
            if host.iter().any(|(h, p)| matches!(h, IpAddr::V4(n) if !n.is_loopback() && !n.is_link_local() && in4(a, *n, *p))) {
                return Some("a network the host is on");
            }
            REFUSED4.iter().any(|(n, p)| in4(a, Ipv4Addr::from(*n), *p)).then_some("a private or special range")
        }
        IpAddr::V6(a) => {
            let v = u128::from(a);
            // through NAT64 (64:ff9b::/96, RFC 6052): what is refused over IPv4 is refused here
            if v >> 32 == NAT64 >> 32 {
                return refused(IpAddr::V4(Ipv4Addr::from(v as u32)), host);
            }
            if host.iter().any(|(h, _)| *h == IpAddr::V6(a)) {
                return Some("the host");
            }
            if host.iter().any(|(h, p)| matches!(h, IpAddr::V6(n) if !n.is_loopback() && !n.is_unicast_link_local()
                                                                   && in6(a, *n, *p))) {
                return Some("a network the host is on");
            }
            let special = a.is_unspecified() || a.is_loopback() || a.is_multicast() || a.is_unicast_link_local()
                || in6(a, Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 0), 7)       // unique local
                || in6(a, Ipv6Addr::new(0x64, 0xff9b, 1, 0, 0, 0, 0, 0), 48)   // local-use NAT64
                || in6(a, Ipv6Addr::new(0xfec0, 0, 0, 0, 0, 0, 0, 0), 10);    // site-local (old)
            special.then_some("a private or special range")
        }
    }
}

/// Where a request wants to go: a name or an address, and a port.
#[derive(Debug, PartialEq)]
pub struct Target {
    pub host: String,
    pub port: u16,
}

/// HOST:PORT (an IPv6 address in brackets), or HOST alone with DEFAULT.
fn parse_authority(s: &str, default: Option<u16>) -> Option<Target> {
    let (host, port) = if let Some(rest) = s.strip_prefix('[') {
        let (h, after) = rest.split_once(']')?;
        (h, after.strip_prefix(':').map(|p| p.parse().ok()).unwrap_or(default)?)
    } else {
        match s.rsplit_once(':') {
            Some((h, p)) if !h.contains(':') => (h, p.parse().ok()?),
            _ if !s.contains(':') => (s, default?),
            _ => return None,
        }
    };
    let ok = !host.is_empty() && host.len() <= 253
        && host.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._:".contains(&b));
    (ok && port != 0).then(|| Target { host: host.to_ascii_lowercase(), port })
}

/// The proxy's view of one connection's first request.
#[derive(Debug, PartialEq)]
pub enum Request {
    /// CONNECT host:port: a tunnel
    Connect(Target),
    /// METHOD http://host/path: the request, rewritten for the origin server
    Http(Target, Vec<u8>),
}

/// A request's head (up to and without the empty line): CONNECT, or a plain request with an
/// absolute http:// URL, rewritten to the origin's form with `Connection: close` (one
/// request per connection: the next one might name another host), the Proxy-* headers
/// dropped. Err: the status line to answer with.
pub fn parse_head(head: &[u8]) -> Result<Request, &'static str> {
    let text = std::str::from_utf8(head).map_err(|_| "400 Bad Request")?;
    let mut lines = text.split("\r\n");
    let first = lines.next().unwrap_or("");
    let mut parts = first.split(' ');
    let (Some(method), Some(target), Some(version), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return Err("400 Bad Request");
    };
    if !version.starts_with("HTTP/1.") || method.is_empty() || !method.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err("400 Bad Request");
    }
    if method == "CONNECT" {
        return parse_authority(target, None).map(Request::Connect).ok_or("400 Bad Request");
    }
    let Some(rest) = target.strip_prefix("http://") else {
        return Err("400 Bad Request"); // (https:// goes through CONNECT)
    };
    // the authority ends at the path, the query or the fragment (RFC 3986); an empty path is /
    let (authority, path) = match rest.find(['/', '?', '#']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let path = path.split('#').next().unwrap_or("");
    let path = if path.starts_with('/') { path.to_string() } else { format!("/{path}") };
    let t = parse_authority(authority, Some(80)).ok_or("400 Bad Request")?;
    // Host: the target's authority, never the one the client sent (it would choose another
    // site on the server connected to; RFC 9112, 3.2.2)
    let mut out = format!("{method} {path} {version}\r\nHost: {authority}\r\n");
    for l in lines {
        let name = l.split(':').next().unwrap_or("").trim().to_ascii_lowercase();
        if name.starts_with("proxy-") || name == "connection" || name == "keep-alive" || name == "host" || l.is_empty() {
            continue;
        }
        out += l;
        out += "\r\n";
    }
    out += "Connection: close\r\n\r\n";
    Ok(Request::Http(t, out.into_bytes()))
}

/// What the proxy says in the zone's log (rate-limited by the caller).
pub type Log = Arc<dyn Fn(&str) + Send + Sync>;

/// A zone's proxy: how often it may write to the log, and its allow list (None: any name).
pub struct Ctx {
    budget: crate::host_dns::Budget,
    allow: Option<Vec<String>>,
}

/// Whether HOST (a name, lowercase) is on ALLOW: a name equal to an entry, or under a
/// `*.domain` entry at any depth (never the domain itself). An address never is.
pub fn allowed(host: &str, allow: &[String]) -> bool {
    let host = host.trim_end_matches('.');
    if host.parse::<IpAddr>().is_ok() {
        return false;
    }
    allow.iter().any(|a| match a.strip_prefix("*.") {
        Some(domain) => host.len() > domain.len() + 1 && host.ends_with(domain)
            && host.as_bytes()[host.len() - domain.len() - 1] == b'.',
        None => host == a,
    })
}

/// The first address of T the zone may reach, connected; Err: why not (for the client and
/// the log).
fn open(t: &Target, log: &Log, cx: &Ctx) -> Result<TcpStream, (&'static str, String)> {
    // the allow list first: a name off it is never looked up
    if cx.allow.as_deref().is_some_and(|a| !allowed(&t.host, a)) {
        let msg = format!("refused {}:{} (not on the zone's allow list)", t.host, t.port);
        if cx.budget.take_at(Instant::now()) {
            log(&format!("[proxy] {msg}"));
        }
        return Err(("403 Forbidden", msg));
    }
    let addrs: Vec<SocketAddr> = (t.host.as_str(), t.port).to_socket_addrs()
        .map_err(|_| ("502 Bad Gateway", format!("{}: no such name", t.host)))?.collect();
    let Some(host) = host_nets() else {
        if cx.budget.take_at(Instant::now()) {
            log("[proxy] the host's networks cannot be read: nothing is let through");
        }
        return Err(("502 Bad Gateway", "the host's networks cannot be read".into()));
    };
    let mut why = None;
    for a in &addrs {
        if let Some(r) = refused(a.ip(), &host) {
            why.get_or_insert(r);
            continue;
        }
        if let Ok(s) = TcpStream::connect_timeout(a, CONNECT_TIMEOUT) {
            return Ok(s);
        }
    }
    match why {
        Some(r) if !addrs.iter().any(|a| refused(a.ip(), &host).is_none()) => {
            let msg = format!("refused {}:{} ({r})", t.host, t.port);
            if cx.budget.take_at(Instant::now()) {
                log(&format!("[proxy] {msg}"));
            }
            Err(("403 Forbidden", msg))
        }
        _ => Err(("502 Bad Gateway", format!("{}:{}: could not connect", t.host, t.port))),
    }
}

/// Bytes both ways until either side ends.
fn splice(a: TcpStream, b: TcpStream) {
    let (Ok(a2), Ok(b2)) = (a.try_clone(), b.try_clone()) else { return };
    let one = std::thread::spawn(move || {
        let (mut from, mut to) = (a2, b2);
        let _ = std::io::copy(&mut from, &mut to);
        let _ = to.shutdown(Shutdown::Write);
    });
    let (mut from, mut to) = (b, a);
    let _ = std::io::copy(&mut from, &mut to);
    let _ = to.shutdown(Shutdown::Write);
    let _ = one.join();
}

fn http(mut c: TcpStream, first: u8, log: &Log, cx: &Ctx) {
    let end = Instant::now() + HEAD_TIMEOUT;
    let mut buf = vec![first];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() || buf.len() > HEAD_MAX || c.set_read_timeout(Some(left)).is_err() {
            return;
        }
        let mut chunk = [0u8; 4096];
        match c.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    };
    let _ = c.set_read_timeout(None);
    let rest = buf[head_end + 4..].to_vec();
    let answer = |c: &mut TcpStream, status: &str, msg: &str| {
        let body = format!("appzones: {msg}\n");
        let _ = c.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\
                                     Connection: close\r\n\r\n{body}", body.len()).as_bytes());
    };
    let (target, send) = match parse_head(&buf[..head_end]) {
        Ok(Request::Connect(t)) => (t, None),
        Ok(Request::Http(t, head)) => (t, Some(head)),
        Err(status) => return answer(&mut c, status, "the zone's proxy takes CONNECT and http:// URLs"),
    };
    match open(&target, log, cx) {
        Err((status, msg)) => answer(&mut c, status, &msg),
        Ok(mut up) => {
            let sent = match &send {
                None => c.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n"),
                Some(head) => up.write_all(head),
            };
            if sent.is_ok() && (rest.is_empty() || up.write_all(&rest).is_ok()) {
                splice(c, up);
            }
        }
    }
}

/// SOCKS5, no authentication, CONNECT only (a name, IPv4 or IPv6).
fn socks5(mut c: TcpStream, log: &Log, cx: &Ctx) {
    let _ = c.set_read_timeout(Some(HEAD_TIMEOUT));
    let mut b = [0u8; 2];
    if c.read_exact(&mut b[..1]).is_err() {
        return;
    }
    let mut methods = vec![0u8; b[0] as usize];
    if c.read_exact(&mut methods).is_err() {
        return;
    }
    if !methods.contains(&0) {
        let _ = c.write_all(&[5, 0xff]);
        return;
    }
    let mut h = [0u8; 4];
    if c.write_all(&[5, 0]).is_err() || c.read_exact(&mut h).is_err() || h[0] != 5 {
        return;
    }
    let reply = |c: &mut TcpStream, rep: u8| c.write_all(&[5, rep, 0, 1, 0, 0, 0, 0, 0, 0]);
    let host = match h[3] {
        1 => {
            let mut a = [0u8; 4];
            if c.read_exact(&mut a).is_err() {
                return;
            }
            Ipv4Addr::from(a).to_string()
        }
        3 => {
            let mut n = [0u8; 1];
            if c.read_exact(&mut n).is_err() {
                return;
            }
            let mut name = vec![0u8; n[0] as usize];
            if c.read_exact(&mut name).is_err() {
                return;
            }
            String::from_utf8_lossy(&name).into_owned()
        }
        4 => {
            let mut a = [0u8; 16];
            if c.read_exact(&mut a).is_err() {
                return;
            }
            format!("[{}]", Ipv6Addr::from(a))
        }
        _ => {
            let _ = reply(&mut c, 8);
            return;
        }
    };
    let mut p = [0u8; 2];
    if c.read_exact(&mut p).is_err() {
        return;
    }
    if h[1] != 1 {
        let _ = reply(&mut c, 7); // only CONNECT
        return;
    }
    let Some(target) = parse_authority(&format!("{host}:{}", u16::from_be_bytes(p)), None) else {
        let _ = reply(&mut c, 1);
        return;
    };
    let _ = c.set_read_timeout(None);
    match open(&target, log, cx) {
        Err((status, _)) => {
            let _ = reply(&mut c, if status.starts_with("403") { 2 } else { 4 });
        }
        Ok(up) => {
            if reply(&mut c, 0).is_ok() {
                splice(c, up);
            }
        }
    }
}

fn handle(c: TcpStream, log: &Log, cx: &Ctx) {
    let mut first = [0u8; 1];
    let _ = c.set_read_timeout(Some(HEAD_TIMEOUT));
    if (&c).read_exact(&mut first).is_err() {
        return;
    }
    if first[0] == 5 {
        socks5(c, log, cx);
    } else {
        http(c, first[0], log, cx);
    }
}

/// A new port of 127.0.0.1, and the same port of ::1: a zone's profile allows the port on
/// every loopback address, so the proxy holds both and nothing else can listen there. A
/// port taken on ::1: another one. (No ::1 at all: nothing can listen there either.)
fn listeners() -> std::io::Result<Vec<TcpListener>> {
    let mut last = None;
    for _ in 0..32 {
        let v4 = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = v4.local_addr()?.port();
        match TcpListener::bind((Ipv6Addr::LOCALHOST, port)) {
            Ok(v6) => return Ok(vec![v4, v6]),
            // taken on ::1 by another process: another port
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => last = Some(e),
            // no IPv6 here (no ::1, the family off: ipv6.disable=1): nothing can listen there
            Err(_) => return Ok(vec![v4]),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("no free port")))
}

/// The proxy on a new port of loopback (listeners), in threads of its own for as long as
/// this process runs; its port. LOG: a line for the zone's log (refusals, a few a second).
pub fn start(log: Log, allow: Option<Vec<String>>) -> std::io::Result<u16> {
    let ls = listeners()?;
    let port = ls[0].local_addr()?.port();
    let open = Arc::new(AtomicUsize::new(0));
    let budget = Arc::new(Ctx { budget: crate::host_dns::Budget::new(2.0, 20.0), allow });
    /// One connection's place in the count, given back when it ends (or its thread could
    /// not be started: the closure holding it is dropped then).
    struct Slot(Arc<AtomicUsize>);
    impl Drop for Slot {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    for l in ls {
        let (open, log, budget) = (open.clone(), log.clone(), budget.clone());
        std::thread::Builder::new().name("proxy".into()).spawn(move || {
            for c in l.incoming().flatten() {
                let slot = Slot(open.clone());
                if open.fetch_add(1, Ordering::SeqCst) >= MAX_CONNS {
                    continue; // (dropped: closed at once, its slot given back)
                }
                let (log, budget) = (log.clone(), budget.clone());
                let _ = std::thread::Builder::new().spawn(move || {
                    let _slot = slot;
                    handle(c, &log, &budget);
                });
            }
        })?;
    }
    Ok(port)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn refusals() {
        let host = vec![(ip("127.0.0.1"), 8), (ip("203.0.113.7"), 24), (ip("2001:db8:5::7"), 64), (ip("fe80::1"), 64)];
        for (a, want) in [("1.1.1.1", None), ("8.8.8.8", None), ("2606:4700::1111", None),
                          ("203.0.113.7", Some("the host")), ("203.0.113.99", Some("a network the host is on")),
                          ("2001:db8:5::99", Some("a network the host is on")), ("2001:db8:6::1", None),
                          ("192.168.1.1", Some("a private or special range")), ("10.1.2.3", Some("a private or special range")),
                          ("100.91.0.5", Some("a private or special range")), ("127.0.0.1", Some("the host")),
                          ("127.0.0.2", Some("a private or special range")), ("169.254.1.1", Some("a private or special range")),
                          ("::1", Some("a private or special range")), ("fd00::1", Some("a private or special range")),
                          ("fe80::2", Some("a private or special range")), ("::ffff:192.168.1.1", Some("a private or special range")),
                          ("::ffff:1.1.1.1", None), ("64:ff9b::c0a8:101", Some("a private or special range")),
                          ("64:ff9b::cb00:7163", Some("a network the host is on")), ("64:ff9b::101:101", None),
                          ("64:ff9b:1::1", Some("a private or special range")), ("224.0.0.251", Some("a private or special range")),
                          ("0.0.0.0", Some("a private or special range")), ("::", Some("a private or special range"))] {
            assert_eq!(refused(ip(a), &host), want, "{a}");
        }
    }

    #[test]
    fn heads() {
        assert_eq!(parse_head(b"CONNECT example.com:443 HTTP/1.1\r\nHost: example.com:443"),
                   Ok(Request::Connect(Target { host: "example.com".into(), port: 443 })));
        assert_eq!(parse_head(b"CONNECT [2606:4700::1111]:443 HTTP/1.1"),
                   Ok(Request::Connect(Target { host: "2606:4700::1111".into(), port: 443 })));
        let Ok(Request::Http(t, head)) = parse_head(b"GET http://Example.com/a?b HTTP/1.1\r\nHost: other.example\r\n\
                                                     Proxy-Connection: keep-alive\r\nConnection: keep-alive\r\nAccept: */*")
        else { panic!() };
        assert_eq!(t, Target { host: "example.com".into(), port: 80 });
        // (the Host the client sent is replaced by the target's)
        assert_eq!(String::from_utf8(head).unwrap(),
                   "GET /a?b HTTP/1.1\r\nHost: Example.com\r\nAccept: */*\r\nConnection: close\r\n\r\n");
        // a query without a path, a fragment: never part of the authority
        let Ok(Request::Http(t, head)) = parse_head(b"GET http://example.com?x=1#f HTTP/1.1") else { panic!() };
        assert_eq!(t.host, "example.com");
        assert!(String::from_utf8(head).unwrap().starts_with("GET /?x=1 HTTP/1.1\r\nHost: example.com\r\n"));
        let Ok(Request::Http(t, head)) = parse_head(b"GET http://example.com:8080 HTTP/1.0") else { panic!() };
        assert_eq!(t.port, 8080);
        assert!(String::from_utf8(head).unwrap().starts_with("GET / HTTP/1.0\r\nHost: example.com:8080\r\n"));
        for bad in [&b"GET https://example.com/ HTTP/1.1"[..], b"CONNECT example.com HTTP/1.1", b"GET /x HTTP/1.1",
                    b"CONNECT a b:1 HTTP/1.1", b"CONNECT ex\x01.com:1 HTTP/1.1", b"get http://a/ HTTP/1.1",
                    b"CONNECT example.com:0 HTTP/1.1", b"CONNECT example.com:443 SPDY/3"] {
            assert_eq!(parse_head(bad), Err("400 Bad Request"), "{}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn allow_lists() {
        let a: Vec<String> = ["api.anthropic.com", "*.githubusercontent.com", "github.com"].iter().map(|x| x.to_string()).collect();
        for (h, want) in [("api.anthropic.com", true), ("api.anthropic.com.", true), ("anthropic.com", false),
                          ("x.api.anthropic.com", false), ("raw.githubusercontent.com", true),
                          ("a.b.githubusercontent.com", true), ("githubusercontent.com", false),
                          ("evilgithubusercontent.com", false), ("github.com", true), ("github.com.evil.org", false),
                          ("140.82.112.3", false), ("::1", false)] {
            assert_eq!(allowed(h, &a), want, "{h}");
        }
        // the proxy: a name off the list is refused before it is looked up, by every way in
        let lines = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let l2 = lines.clone();
        let port = start(Arc::new(move |s: &str| l2.lock().unwrap().push(s.to_string())), Some(a)).unwrap();
        let talk = |send: &[u8], want: usize| {
            let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
            c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            c.write_all(send).unwrap();
            let mut got = vec![0u8; want];
            c.read_exact(&mut got).unwrap();
            got
        };
        assert!(talk(b"CONNECT not-allowed.appzones.invalid:443 HTTP/1.1\r\n\r\n", 12).starts_with(b"HTTP/1.1 403"));
        assert!(talk(b"GET http://example.com/ HTTP/1.1\r\n\r\n", 12).starts_with(b"HTTP/1.1 403"));
        // SOCKS5 to an address (1.1.1.1): never on a list of names
        assert_eq!(&talk(&[5, 1, 0, 5, 1, 0, 1, 1, 1, 1, 1, 1, 187], 4)[..4], &[5, 0, 5, 2]);
        let l = lines.lock().unwrap();
        assert!(l.iter().any(|x| x.contains("refused example.com:80 (not on the zone's allow list)")), "{l:?}");
    }

    #[test]
    fn refuses_through_every_way_in() {
        // the proxy itself: loopback and the host refused by CONNECT, plain HTTP and SOCKS5
        let lines = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let l2 = lines.clone();
        let port = start(Arc::new(move |s: &str| l2.lock().unwrap().push(s.to_string())), None).unwrap();
        let talk = |send: &[u8], want: usize| {
            let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
            c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            c.write_all(send).unwrap();
            let mut got = vec![0u8; want];
            c.read_exact(&mut got).unwrap();
            got
        };
        assert!(talk(b"CONNECT 127.0.0.1:22 HTTP/1.1\r\n\r\n", 12).starts_with(b"HTTP/1.1 403"));
        // the same port on ::1 is the proxy's too (where this host has IPv6 loopback)
        if TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).is_ok() {
            let mut c = TcpStream::connect((Ipv6Addr::LOCALHOST, port)).unwrap();
            c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            c.write_all(b"CONNECT [::1]:22 HTTP/1.1\r\n\r\n").unwrap();
            let mut got = [0u8; 12];
            c.read_exact(&mut got).unwrap();
            assert!(got.starts_with(b"HTTP/1.1 403"));
        }
        assert!(talk(b"GET http://127.0.0.1:1/ HTTP/1.1\r\n\r\n", 12).starts_with(b"HTTP/1.1 403"));
        assert!(talk(b"GET http://localhost:1/ HTTP/1.1\r\n\r\n", 12).starts_with(b"HTTP/1.1 403"));
        assert!(talk(b"GET /x HTTP/1.1\r\n\r\n", 12).starts_with(b"HTTP/1.1 400"));
        // SOCKS5: greeting, then CONNECT 10.0.0.1:80: "not allowed by the rules"
        let got = talk(&[5, 1, 0, 5, 1, 0, 1, 10, 0, 0, 1, 0, 80], 4);
        assert_eq!(&got[..4], &[5, 0, 5, 2]);
        assert!(lines.lock().unwrap().iter().any(|l| l.contains("refused 127.0.0.1:22 (the host)")), "{:?}", lines.lock().unwrap());
    }
}
