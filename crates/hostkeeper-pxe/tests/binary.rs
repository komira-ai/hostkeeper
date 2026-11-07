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

/// Run as a program, `fetch` reaches the verb: a daemon config that is not there is refused
/// with exit status 1 and one line naming it, and nothing is made.
///
/// Catches a binary whose `fetch` never loads the operator's daemon config.
#[test]
fn the_binary_refuses_a_fetch_whose_daemon_config_is_missing() {
    let base = std::env::current_exe().expect("the test binary");
    let dir = base.with_file_name("pxe-binary-scratch-boot");
    let missing = base.with_file_name("pxe-binary-scratch-missing.toml");
    let output = Command::new(env!("CARGO_BIN_EXE_hostkeeper-pxe"))
        .args(["fetch", "--release", "https://r.example", "--dir"])
        .arg(&dir)
        .arg("--daemon-config")
        .arg(&missing)
        .output()
        .expect("run it");
    assert_eq!(output.status.code(), Some(1));
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(
        err.starts_with(&format!("hostkeeper-pxe fetch: {}: ", missing.display())),
        "{err}"
    );
    assert_eq!(err.lines().count(), 1, "{err}");
    assert!(!dir.exists());
}
