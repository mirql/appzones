//! open-link: the http(s) part of a zone's xdg-open shim (links = "zone" or "zone:NAME").
//!
//!   appzones-helper open-link URL
//!
//! Hands URL to the host through the zone's link socket, /run/zone/links.sock: the URL
//! and a newline; the server answers "ok\n" when a browser takes it. Exit 0 then, 1
//! otherwise, 2 for usage. The port of the shim's Python one-liner, so a zone needs no
//! Python of the host's for clicked links. The server checks the URL; this only carries it.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

const LINKS_SOCK: &str = "/run/zone/links.sock";

pub fn main(args: &[String]) -> u8 {
    let [url] = args else {
        eprintln!("usage: appzones-helper open-link URL");
        return 2;
    };
    send(LINKS_SOCK, url)
}

fn send(sock: &str, url: &str) -> u8 {
    let Ok(mut s) = UnixStream::connect(sock) else {
        eprintln!("open-link: no link socket at {sock}");
        return 1;
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(10)));
    if s.write_all(format!("{url}\n").as_bytes()).is_err() {
        return 1;
    }
    let mut answer = [0u8; 3];
    let mut got = 0;
    while got < answer.len() {
        match s.read(&mut answer[got..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => got += n,
        }
    }
    if &answer[..got] == b"ok\n" { 0 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    fn server(answer: &'static [u8]) -> (String, std::thread::JoinHandle<Vec<u8>>) {
        let dir = std::env::temp_dir().join(format!("open-link-{}-{}", std::process::id(), answer.len()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("links.sock");
        let _ = std::fs::remove_file(&path);
        let l = UnixListener::bind(&path).unwrap();
        let t = std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut got = Vec::new();
            let mut b = [0u8; 1];
            while c.read(&mut b).unwrap_or(0) == 1 && b[0] != b'\n' {
                got.push(b[0]);
            }
            c.write_all(answer).unwrap();
            got
        });
        (path.to_string_lossy().into_owned(), t)
    }

    #[test]
    fn ok_and_no() {
        let (p, t) = server(b"ok\n");
        assert_eq!(send(&p, "https://example.com/a b"), 0);
        assert_eq!(t.join().unwrap(), b"https://example.com/a b");
        let (p, t) = server(b"no\n");
        assert_eq!(send(&p, "https://example.com/"), 1);
        t.join().unwrap();
        assert_eq!(send("/nonexistent/links.sock", "https://x/"), 1);
    }
}
