//! appzones-helper: all of appzones in one binary (the mount helper is its own).
//!
//!   appzones-helper cli ARGS...        `appzones` itself: every command, `serve` included
//!   appzones-helper zone-init          the agent inside a zone (pid 2, after bwrap)
//!   appzones-helper open-link URL      in a zone: hand an http(s) link to the host
//!   appzones-helper zone-wayland SOCKET APP_ID INSTANCE
//!   appzones-helper serve --plan FILE --lock-fd L [--notify-fd N]
//!                                      run a plan written by `appzones-helper plan ZONE`
//!   appzones-helper config-json FILE, check-mount-source ...   for tests: the config and
//!                                      the never-list as JSON
//!
//! The interfaces are those of the Python implementation it replaced in 0.12: the config,
//! the commands and their outputs, the runtime layout, the agent protocol, settings_hash
//! byte for byte (helper/tests/golden).

use std::process::ExitCode;

mod active;
mod agent;
mod cli;
mod config;
mod host_dns;
#[cfg(target_os = "linux")]
mod netsetup;
mod open_link;
// (on macOS the bwrap plan and its server are not used: design/macos.md)
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod plan;
mod policy;
mod project;
// (appzones' proxy for internet zones on macOS; tested on every system)
#[cfg_attr(target_os = "linux", allow(dead_code))]
mod proxy;
mod pyfmt;
// (a zone on macOS; its profile is tested on every system)
#[cfg_attr(target_os = "linux", allow(dead_code))]
mod seatbelt;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod serve;
mod sys;
// what runs in a zone and beside it on Linux
#[cfg(target_os = "linux")]
mod zone_dns;
mod zone_init;
#[cfg(target_os = "linux")]
mod zone_wayland;

const USAGE: &str = "usage: appzones-helper cli ARGS... | zone-init | open-link URL | zone-wayland SOCKET APP_ID INSTANCE | \
                     plan ZONE | serve --plan FILE --lock-fd L [--notify-fd N] | --version | --commands";

/// The commands this build implements (`--commands`).
#[cfg(target_os = "linux")]
const IMPLEMENTED: &[&str] = &[
    "zone-wayland",
    "zone-init",
    "open-link",
    "serve",
    "plan",
    "cli",
];
#[cfg(not(target_os = "linux"))]
const IMPLEMENTED: &[&str] = &["zone-init", "open-link", "serve", "plan", "cli"];

fn main() -> ExitCode {
    // never a panic on an argument that is not UTF-8 (std::env::args would): lossy here,
    // and the commands that take paths say so
    let raw: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if raw
        .first()
        .map(|a| a == "check-mount-source")
        .unwrap_or(false)
    {
        return ExitCode::from(check_mount_source(&raw[1..])); // paths as given, byte for byte
    }
    if raw.first().map(|a| a == "config-json").unwrap_or(false) {
        return ExitCode::from(config::config_json_main(&raw[1..])); // the differential test's view
    }
    if raw.first().map(|a| a == "cli").unwrap_or(false) {
        // `appzones` itself (its wrapper runs this when installed with the Rust helper)
        // the low byte, as an exit status is (Python's sys.exit(-15) is 241: killed by
        // SIGTERM), never a negative clamped to success
        return ExitCode::from((cli::main(&raw[1..]) & 0xff) as u8);
    }
    if raw.first().map(|a| a == "plan").unwrap_or(false) {
        return ExitCode::from(plan::plan_main(&raw[1..])); // a zone's plan, as appzones serve's first part
    }
    if raw.first().map(|a| a == "serve").unwrap_or(false) {
        return ExitCode::from(serve::main(&raw[1..])); // the plan's path byte for byte
    }
    let args: Vec<String> = raw
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    match args.first().map(String::as_str) {
        Some("--version") | Some("-V") => {
            println!("appzones-helper {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        // what this build implements, one per line: the Python side and `make install`
        // use only these (an older build may lack some)
        Some("--commands") => {
            for c in IMPLEMENTED {
                println!("{c}");
            }
            ExitCode::SUCCESS
        }
        #[cfg(target_os = "linux")]
        Some("zone-wayland") => ExitCode::from(zone_wayland::main(&args[1..])),
        Some("zone-init") => ExitCode::from(zone_init::main(&args[1..])),
        Some("open-link") => ExitCode::from(open_link::main(&args[1..])),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// `check-mount-source --home H --code-dir C [--denied-file F] [--list-denied] PATH...`:
/// for each PATH, a JSON line: null (allowed) or the refusal, as policy::mount_source_error
/// says it; with --list-denied first the never-list itself, as one JSON list.
fn check_mount_source(args: &[std::ffi::OsString]) -> u8 {
    use std::os::unix::ffi::OsStrExt;
    const USAGE: &str =
        "usage: appzones-helper check-mount-source --home H --code-dir C [--denied-file F] \
                         [--list-denied] PATH...";
    let usage = || {
        eprintln!("{USAGE}");
        2
    };
    let mut policy = policy::Policy {
        home: "".into(),
        code_dir: "".into(),
        denied_file: None,
        state_dir: None,
        extra: vec![],
    };
    let (mut paths, mut list) = (Vec::new(), false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let key = a.to_str().unwrap_or("");
        if matches!(key, "--home" | "--code-dir" | "--denied-file") {
            // a value, never the next option (a file named so: ./--list-denied)
            let Some(v) = it.next().filter(|v| !v.as_bytes().starts_with(b"--")) else {
                return usage();
            };
            match key {
                "--home" => policy.home = v.clone(),
                "--code-dir" => policy.code_dir = v.clone(),
                _ => policy.denied_file = Some(v.clone()),
            }
        } else if key == "--list-denied" {
            list = true;
        } else {
            paths.push(a.clone());
        }
    }
    if policy.home.is_empty() || policy.code_dir.is_empty() {
        return usage();
    }
    if list {
        let denied: Vec<String> = match policy.denied_paths() {
            Ok(d) => d
                .iter()
                .map(|p| String::from_utf8_lossy(p).into_owned())
                .collect(),
            Err(e) => vec![format!("error: {e}")],
        };
        println!(
            "{}",
            serde_json::to_string(&denied).unwrap_or_else(|_| "[]".into())
        );
    }
    for p in paths {
        println!(
            "{}",
            serde_json::to_string(&policy.mount_source_error(p.as_bytes()))
                .unwrap_or_else(|_| "null".into())
        );
    }
    0
}

#[cfg(test)]
mod tests {
    #[test]
    fn sparkle() {
        // mirql's rule: every manifest depends on love-sparkle; here it is used once
        assert!(mirql_love_sparkle::sparkle_of_love().contains("Sparkle of Love"));
    }
}
