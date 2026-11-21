//! (Linux: zone-init runs on macOS from stage 2 of design/macos.md.)
//! The agent protocol of zone-init (helper/src/zone_init.rs), as a specification: zone-init
//! runs outside a zone here, gets its control socket and its server's pidfd as the host
//! gives them, and is spoken to over the socket as `appzones` does.

#![cfg(target_os = "linux")]

use serde_json::{json, Value};
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const HELPER: &str = env!("CARGO_BIN_EXE_appzones-helper");

fn pidfd_open(pid: u32) -> RawFd {
    // Safety: the pidfd_open system call, no pointers.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::c_int, 0) } as RawFd;
    assert!(fd >= 0, "pidfd_open: {}", std::io::Error::last_os_error());
    fd
}

fn close(fd: RawFd) {
    // Safety: a descriptor of ours.
    unsafe { libc::close(fd) };
}

fn pipe() -> (RawFd, RawFd) {
    let mut p = [0; 2];
    // Safety: pipe2 into a local array.
    assert_eq!(unsafe { libc::pipe2(p.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
    (p[0], p[1])
}

fn devnull(write: bool) -> RawFd {
    // Safety: open of a NUL-terminated path.
    unsafe { libc::open(c"/dev/null".as_ptr(), if write { libc::O_WRONLY } else { libc::O_RDONLY } | libc::O_CLOEXEC) }
}

/// zone-init with listener L (as ZONE_CTL_FD) and SERVER (a pidfd, as ZONE_SERVER_FD).
fn start(dir: &PathBuf, zone: &str, l: &UnixListener, server: RawFd) -> Child {
    let (lf, sf) = (l.as_raw_fd(), server);
    let mut c = Command::new(HELPER);
    c.arg("zone-init").env_clear().env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir).env("ZONE", zone).env("XDG_RUNTIME_DIR", dir)
        .env("ZONE_CTL_FD", lf.to_string()).env("ZONE_SERVER_FD", sf.to_string())
        // not UTF-8 (a Latin-1 locale's): zone-init starts and passes it on
        .env("LC_TEST", OsStr::from_bytes(b"caf\xe9"))
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    // Safety: only fcntl in the child, before exec: the two fds are inherited.
    unsafe {
        c.pre_exec(move || {
            for fd in [lf, sf] {
                if libc::fcntl(fd, libc::F_SETFD, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    c.spawn().unwrap()
}

struct Init {
    dir: PathBuf,
    sock: PathBuf,
    child: Child,
}

impl Init {
    fn new(tag: &str) -> Init {
        let dir = std::env::temp_dir().join(format!("az-agent-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("agent.sock");
        let l = UnixListener::bind(&sock).unwrap();
        let me = pidfd_open(std::process::id()); // its "server": this test process
        let child = start(&dir, "tt", &l, me);
        close(me);
        drop(l);
        let init = Init { dir, sock, child };
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end {
            if init.try_request(&json!({"op": "ping"}), &[], 1).is_some_and(|r| r[0]["ok"] == json!(true)) {
                return init;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("zone-init did not answer");
    }

    fn connect_raw(&self, data: &[u8], fds: &[RawFd]) -> Option<UnixStream> {
        let s = UnixStream::connect(&self.sock).ok()?;
        s.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
        send(&s, data, fds).ok()?;
        Some(s)
    }

    fn connect(&self, req: &Value, fds: &[RawFd]) -> UnixStream {
        self.connect_raw(format!("{req}\n").as_bytes(), fds).expect("connect")
    }

    fn try_request(&self, req: &Value, fds: &[RawFd], n: usize) -> Option<Vec<Value>> {
        let mut s = self.connect_raw(format!("{req}\n").as_bytes(), fds)?;
        Some(lines(&mut s, n))
    }

    fn request(&self, req: Value) -> Value {
        self.try_request(&req, &[], 1).expect("request").remove(0)
    }
}

impl Drop for Init {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// DATA with FDS (SCM_RIGHTS), as `appzones` sends a request.
fn send(s: &UnixStream, data: &[u8], fds: &[RawFd]) -> std::io::Result<()> {
    if fds.is_empty() {
        return (&*s).write_all(data);
    }
    // Safety: a msghdr over our buffers, the control buffer sized by CMSG_SPACE.
    unsafe {
        let mut iov = libc::iovec { iov_base: data.as_ptr() as *mut _, iov_len: data.len() };
        let size = std::mem::size_of_val(fds) as u32;
        let mut cbuf = vec![0u8; libc::CMSG_SPACE(size) as usize];
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cbuf.as_mut_ptr().cast();
        msg.msg_controllen = cbuf.len() as _;
        let c = libc::CMSG_FIRSTHDR(&msg);
        (*c).cmsg_level = libc::SOL_SOCKET;
        (*c).cmsg_type = libc::SCM_RIGHTS;
        (*c).cmsg_len = libc::CMSG_LEN(size) as _;
        std::ptr::copy_nonoverlapping(fds.as_ptr(), libc::CMSG_DATA(c).cast(), fds.len());
        if libc::sendmsg(s.as_raw_fd(), &msg, 0) < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// N reply lines (JSON objects) from S; fewer when it closes first.
fn lines(s: &mut UnixStream, n: usize) -> Vec<Value> {
    let (mut buf, mut out) = (Vec::new(), Vec::new());
    while out.len() < n {
        while !buf.contains(&b'\n') {
            let mut chunk = [0u8; 65536];
            match s.read(&mut chunk) {
                Ok(0) | Err(_) => return out,
                Ok(k) => buf.extend_from_slice(&chunk[..k]),
            }
        }
        let i = buf.iter().position(|b| *b == b'\n').unwrap();
        out.push(serde_json::from_slice(&buf[..i]).unwrap());
        buf.drain(..=i);
    }
    out
}

fn read_all(fd: RawFd) -> String {
    let mut f = unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(fd) };
    let mut s = String::new();
    f.read_to_string(&mut s).unwrap();
    s
}

#[test]
fn ping() {
    let z = Init::new("ping");
    let r = z.request(json!({"op": "ping"}));
    assert_eq!((&r["ok"], &r["zone"]), (&json!(true), &json!("tt")));
    assert!(r["pid"].is_i64(), "{r}");
}

#[test]
fn bad_requests() {
    let z = Init::new("bad");
    let r = z.request(json!({}));
    assert_eq!(r["ok"], json!(false));
    assert!(r["error"].as_str().unwrap().contains("unknown op"), "{r}");
    let mut s = z.connect_raw(b"not json\n", &[]).unwrap();
    assert_eq!(lines(&mut s, 1), [json!({"ok": false, "error": "bad request"})]);
    for argv in [json!([]), json!("sh"), json!([1])] {
        assert_eq!(z.request(json!({"op": "run", "argv": argv}))["ok"], json!(false), "{argv}");
    }
    let r = z.request(json!({"op": "run", "argv": ["/nonexistent/program"]}));
    assert_eq!(r["ok"], json!(false));
    assert!(r["error"].as_str().unwrap().contains("/nonexistent/program"), "{r}");
    assert_eq!(z.request(json!({"op": "run", "argv": ["true"], "bus": "elsewhere"}))["ok"], json!(false));
    for argv in [json!("sh"), json!([1]), json!([["sh"]]), json!([]), json!(""), json!(0), json!({})] {
        assert_eq!(z.request(json!({"op": "shell", "argv": argv}))["ok"], json!(false), "{argv}");
    }
    assert_eq!(z.request(json!({"op": "busy", "path": "/etc"}))["ok"], json!(false));
    assert_eq!(z.request(json!({"op": "mounts"}))["ok"], json!(true));
    assert_eq!(z.request(json!({"op": "status", "pid": 1}))["ok"], json!(false));
    assert_eq!(z.request(json!({"op": "resize", "pid": 1, "rows": 1, "cols": 1})), json!({"ok": false}));
}

#[test]
fn run_wait() {
    // --wait: the client's three fds (pipes) become the command's; the exit code comes last
    let z = Init::new("wait");
    let ((in_r, in_w), (out_r, out_w), (err_r, err_w)) = (pipe(), pipe(), pipe());
    let mut s = z.connect(&json!({"op": "run", "argv": ["sh", "-c", "read x; echo got $x; echo oops >&2; exit 3"],
                                  "wait": true}), &[in_r, out_w, err_w]);
    for f in [in_r, out_w, err_w] {
        close(f);
    }
    assert_eq!(lines(&mut s, 1)[0]["ok"], json!(true));
    // Safety: the write end of our pipe.
    assert_eq!(unsafe { libc::write(in_w, b"hi\n".as_ptr().cast(), 3) }, 3);
    close(in_w);
    assert_eq!(lines(&mut s, 1), [json!({"exit": 3})]);
    drop(s);
    assert_eq!((read_all(out_r), read_all(err_r)), ("got hi\n".to_string(), "oops\n".to_string()));
    // killed by a signal: a negative exit code
    let null = [devnull(false), devnull(true), devnull(true)];
    let r = z.try_request(&json!({"op": "run", "argv": ["sh", "-c", "kill -9 $$"], "wait": true}), &null, 2).unwrap();
    assert_eq!(r[1], json!({"exit": -9}));
    // a variable that is not UTF-8 reaches a command unchanged
    let r = z.try_request(&json!({"op": "run", "argv": ["sh", "-c", "test \"$LC_TEST\" = \"$(printf 'caf\\351')\""],
                                  "wait": true}), &null, 2).unwrap();
    assert_eq!(r[1], json!({"exit": 0}));
    for f in null {
        close(f);
    }
}

#[test]
fn run_wait_refuses() {
    // --wait takes pipes, files or /dev/null, never a tty or a socket, and exactly three
    let z = Init::new("refuses");
    let (mut master, mut slave) = (0, 0);
    // Safety: openpty into locals, no name, termios or size.
    assert_eq!(unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null(),
                                      std::ptr::null()) }, 0);
    let (a, _b) = UnixStream::pair().unwrap();
    let null = devnull(true);
    for fds in [&[slave, null, null][..], &[a.as_raw_fd(), null, null], &[null, null]] {
        let r = z.try_request(&json!({"op": "run", "argv": ["true"], "wait": true}), fds, 1).unwrap();
        assert_eq!(r[0]["ok"], json!(false), "{fds:?}");
    }
    for f in [master, slave, null] {
        close(f);
    }
}

#[test]
fn run_status_ps() {
    // without --wait: a pid at once; status while running and after, with the output's tail; ps
    let z = Init::new("status");
    let go = z.dir.join("go"); // the command waits for it: running until the checks are done
    let r = z.request(json!({"op": "run", "argv": ["sh", "-c", format!(
        "echo line-one; while [ ! -e {} ]; do sleep 0.05; done; echo line-two", go.display())]}));
    let pid = r["pid"].as_i64().unwrap_or_else(|| panic!("{r}"));
    let checks = || {
        let st = z.request(json!({"op": "status", "pid": pid}));
        assert_eq!((&st["ok"], &st["running"], &st["exit"]), (&json!(true), &json!(true), &Value::Null), "{st}");
        let ps = z.request(json!({"op": "ps"}));
        assert!(ps["procs"].as_array().unwrap().iter().any(|p| p[0] == json!(pid)), "{ps}");
        assert!(ps["init"].is_i64(), "{ps}");
        assert_eq!(ps["helpers"], json!([]));
    };
    let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(checks));
    std::fs::write(&go, "").unwrap(); // released also when a check fails
    if let Err(e) = ok {
        std::panic::resume_unwind(e);
    }
    let mut st = json!({});
    for _ in 0..100 {
        st = z.request(json!({"op": "status", "pid": pid}));
        if st["running"] == json!(false) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!((&st["running"], &st["exit"]), (&json!(false), &json!(0)), "{st}");
    assert!(st["tail"].as_array().unwrap().iter().any(|t| t.as_str().unwrap().contains("line-two")), "{st}");
}

#[test]
fn shell() {
    // a pty in the zone; after the first line the connection carries raw bytes
    let z = Init::new("shell");
    let mut s = z.connect(&json!({"op": "shell", "rows": 30, "cols": 100, "term": "dumb",
                                  "argv": ["sh", "-c", "read x; stty size; echo shell-$TERM-ok"]}), &[]);
    let mut first = Vec::new();
    while !first.ends_with(b"\n") {
        let mut c = [0u8; 1];
        assert_eq!(s.read(&mut c).unwrap(), 1, "closed before the reply: {first:?}");
        first.push(c[0]);
    }
    assert_eq!(serde_json::from_slice::<Value>(&first).unwrap()["ok"], json!(true));
    s.write_all(b"go\n").unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    let out = String::from_utf8_lossy(&out);
    assert!(out.contains("30 100") && out.contains("shell-dumb-ok"), "{out}");
}

#[test]
fn output_is_bounded() {
    // an app's output without line ends, or with huge lines, keeps zone-init's memory
    // bounded: each tail entry at most 1 KiB, and the last lines still there
    let z = Init::new("bounded");
    let r = z.request(json!({"op": "run", "argv": ["sh", "-c", "head -c 3000000 /dev/zero | tr '\\0' a; echo; echo done"]}));
    let pid = r["pid"].as_i64().unwrap_or_else(|| panic!("{r}"));
    let mut st = json!({});
    for _ in 0..200 {
        st = z.request(json!({"op": "status", "pid": pid}));
        if st["running"] == json!(false) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(st["running"], json!(false), "{st}");
    let tail = st["tail"].as_array().unwrap();
    assert!(tail.iter().all(|t| t.as_str().unwrap().chars().count() <= 1024));
    assert_eq!(tail.last(), Some(&json!("done")), "{st}");
}

#[test]
fn shell_that_never_reads() {
    // a shell that stops reading its input (and a client that never reads the echo) cannot
    // stall zone-init: it still answers
    let z = Init::new("noread");
    let mut s = z.connect(&json!({"op": "shell", "rows": 24, "cols": 80, "term": "dumb", "argv": ["sh", "-c", "sleep 30"]}), &[]);
    let mut first = Vec::new();
    while !first.ends_with(b"\n") {
        let mut c = [0u8; 1];
        assert_eq!(s.read(&mut c).unwrap(), 1);
        first.push(c[0]);
    }
    let mut w = s.try_clone().unwrap();
    std::thread::spawn(move || {
        let _ = w.write_all(&vec![b'x'; 2 << 20]);
    });
    std::thread::sleep(Duration::from_secs(1));
    let t0 = Instant::now();
    assert_eq!(z.request(json!({"op": "ping"}))["ok"], json!(true));
    assert!(t0.elapsed() < Duration::from_secs(3), "{:?}", t0.elapsed());
}

#[test]
fn ends_with_its_server() {
    // zone-init ends when its server's pidfd says the server is gone
    let dir = std::env::temp_dir().join(format!("az-agent-ends-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Command::new("sleep").arg("30").spawn().unwrap();
    let pidfd = pidfd_open(server.id());
    let l = UnixListener::bind(dir.join("agent.sock")).unwrap();
    let mut init = start(&dir, "t2", &l, pidfd);
    close(pidfd);
    drop(l);
    std::thread::sleep(Duration::from_millis(500));
    assert!(init.try_wait().unwrap().is_none(), "zone-init ended with its server alive");
    server.kill().unwrap();
    server.wait().unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(st) = init.try_wait().unwrap() {
            break st;
        }
        assert!(Instant::now() < end, "zone-init outlived its server");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(status.code(), Some(0));
    let _ = std::fs::remove_dir_all(&dir);
}
