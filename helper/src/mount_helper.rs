//! appzones-mount-helper: the root step of `appzones mount`, its own small binary (run
//! through sudo). Linux only: macOS has no mounts at run time (design/macos.md). The code
//! is in mount_helper_linux.rs.
#![deny(unsafe_code)]

#[cfg(target_os = "linux")]
include!("mount_helper_linux.rs");

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!("appzones-mount-helper: Linux only (macOS has no mounts at run time)");
    std::process::ExitCode::from(2)
}
