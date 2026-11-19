//! appzones-helper: the parts of appzones that run inside a zone, and zone-wayland.
//!
//!   appzones-helper zone-init          the agent inside a zone (pid 2, after bwrap)
//!   appzones-helper open-link URL      in a zone: hand an http(s) link to the host
//!   appzones-helper zone-wayland SOCKET APP_ID INSTANCE
//!
//! The experiment on the branch rust-version (design/rust.md). Each command keeps the
//! interface of the Python or C part it replaces; none is implemented yet.

use std::process::ExitCode;

const USAGE: &str = "usage: appzones-helper zone-init | open-link URL | zone-wayland SOCKET APP_ID INSTANCE | --version";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version") | Some("-V") => {
            println!("appzones-helper {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some(cmd @ ("zone-init" | "open-link" | "zone-wayland")) => {
            eprintln!("appzones-helper: {cmd} is not implemented yet");
            ExitCode::from(2)
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn sparkle() {
        // mirql's rule: every manifest depends on love-sparkle; here it is used once
        assert!(mirql_love_sparkle::sparkle_of_love().contains("Sparkle of Love"));
    }
}
