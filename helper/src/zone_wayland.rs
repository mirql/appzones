//! zone-wayland: a wp_security_context_v1 listener for one zone (the C helper's port).
//!
//!   appzones-helper zone-wayland SOCKET_PATH APP_ID INSTANCE_ID
//!
//! Connects to the host compositor ($WAYLAND_DISPLAY), listens on SOCKET_PATH and hands
//! that socket to the compositor as a security context (engine "dev.mirql.zones"):
//! clients connecting there are sandboxed, and the compositor hides privileged globals
//! from them. The context lives as long as this process: its close fd is the read end
//! of a pipe whose write end only this process holds. Prints "ready" once committed.
//! With $ZONE_WAYLAND_PARENT (the server's pid) it gets SIGTERM when that process dies
//! (PR_SET_PDEATHSIG), and exits at once if it died already.

use std::io::Write;
use std::os::fd::AsFd;
use std::os::unix::net::UnixListener;

use rustix::pipe::{pipe_with, PipeFlags};
use rustix::process::{getppid, set_parent_process_death_signal, Signal};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::wp::security_context::v1::client::{
    wp_security_context_manager_v1::WpSecurityContextManagerV1, wp_security_context_v1::WpSecurityContextV1,
};

struct State;

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection,
             _: &QueueHandle<Self>) {}
}

impl Dispatch<WpSecurityContextManagerV1, ()> for State {
    fn event(_: &mut Self, _: &WpSecurityContextManagerV1,
             _: <WpSecurityContextManagerV1 as wayland_client::Proxy>::Event, _: &(), _: &Connection,
             _: &QueueHandle<Self>) {}
}

impl Dispatch<WpSecurityContextV1, ()> for State {
    fn event(_: &mut Self, _: &WpSecurityContextV1, _: <WpSecurityContextV1 as wayland_client::Proxy>::Event,
             _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

fn fail(msg: impl std::fmt::Display) -> u8 {
    eprintln!("zone-wayland: {msg}");
    1
}

/// SIGTERM unblocked (a blocked mask is inherited across exec), then PDEATHSIG, then the
/// parent checked: no gap in which the server could die unnoticed.
fn die_with_parent(parent: &str) -> Result<(), String> {
    let want: i32 = parent.parse().map_err(|_| format!("ZONE_WAYLAND_PARENT {parent:?} is not a pid"))?;
    unblock_sigterm();
    set_parent_process_death_signal(Some(Signal::TERM)).map_err(|e| format!("prctl: {e}"))?;
    match getppid() {
        Some(p) if p.as_raw_nonzero().get() == want => Ok(()),
        _ => Err("server gone".into()),
    }
}

fn unblock_sigterm() {
    // Safety: a sigset_t on the stack, filled by sigemptyset/sigaddset, and passed by
    // pointer to sigprocmask, which only reads it; no other thread runs yet.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigprocmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
    }
}

pub fn main(args: &[String]) -> u8 {
    let [path, app_id, instance] = args else {
        eprintln!("usage: appzones-helper zone-wayland SOCKET_PATH APP_ID INSTANCE_ID");
        return 2;
    };
    if let Ok(parent) = std::env::var("ZONE_WAYLAND_PARENT") {
        if let Err(e) = die_with_parent(&parent) {
            return fail(e);
        }
    }
    let conn = match Connection::connect_to_env() {
        Ok(c) => c,
        Err(e) => return fail(format!("cannot connect to the compositor: {e}")),
    };
    let (globals, mut queue) = match registry_queue_init::<State>(&conn) {
        Ok(g) => g,
        Err(e) => return fail(format!("the compositor's globals: {e}")),
    };
    let qh = queue.handle();
    let mgr: WpSecurityContextManagerV1 = match globals.bind(&qh, 1..=1, ()) {
        Ok(m) => m,
        Err(_) => return fail("compositor has no wp_security_context_manager_v1"),
    };
    let _ = std::fs::remove_file(path);
    let listener = match UnixListener::bind(path) {
        Ok(l) => l, // close-on-exec, listening
        Err(e) => return fail(format!("{path}: {e}")),
    };
    // the context ends when the compositor sees the write end close: held until we exit
    let (close_r, _close_w) = match pipe_with(PipeFlags::CLOEXEC) {
        Ok(p) => p,
        Err(e) => return fail(format!("pipe: {e}")),
    };
    let ctx = mgr.create_listener(listener.as_fd(), close_r.as_fd(), &qh, ());
    ctx.set_sandbox_engine("dev.mirql.zones".into());
    ctx.set_app_id(app_id.clone());
    ctx.set_instance_id(instance.clone());
    ctx.commit();
    ctx.destroy();
    if let Err(e) = queue.roundtrip(&mut State) {
        return fail(format!("protocol error: {e}"));
    }
    drop(listener); // the compositor has its own copies now
    drop(close_r);
    let mut out = std::io::stdout();
    if writeln!(out, "ready").and_then(|_| out.flush()).is_err() {
        return 1;
    }
    // until we are told to stop or the compositor goes away
    while queue.blocking_dispatch(&mut State).is_ok() {}
    0
}
