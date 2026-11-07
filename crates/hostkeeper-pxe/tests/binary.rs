//! The `hostkeeper-pxe` binary forwards to its library: run as a program, it answers
//! `--version` with its name and version and exits 0, and a command line that does not parse
//! exits 2.

use std::process::Command;

/// Catches a binary that does not forward its arguments, or drops the library's exit status.
#[test]
fn the_binary_forwards_its_arguments_and_exit_status() {
    let binary = env!("CARGO_BIN_EXE_hostkeeper-pxe");
    let version = Command::new(binary)
        .arg("--version")
        .output()
        .expect("run it");
    assert_eq!(version.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&version.stdout),
        format!("hostkeeper-pxe {}\n", env!("CARGO_PKG_VERSION"))
    );
    let usage = Command::new(binary).arg("serve").output().expect("run it");
    assert_eq!(usage.status.code(), Some(2));
}
