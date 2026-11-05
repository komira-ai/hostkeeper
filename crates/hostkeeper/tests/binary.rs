//! The binary: it forwards to the library, and its exit status is the library's.

use std::path::Path;
use std::process::Command;

/// Catches logic growing in the binary, where coverage would not judge it with the library's
/// tests: `src/main.rs` stays a few lines that call `hostkeeper_daemon::cli::main` and nothing
/// else.
#[test]
fn the_binary_crate_only_forwards() {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.rs"))
        .expect("main.rs");
    assert!(text.lines().count() < 30, "{text}");
    let code: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("//"))
        .collect();
    assert_eq!(
        code,
        [
            "fn main() -> std::process::ExitCode {",
            "hostkeeper_daemon::cli::main()",
            "}"
        ]
    );
}

/// Catches a binary that exits 0, or with another error's status, when its config is missing:
/// `hostkeeper daemon` exits 3 and names the file.
#[test]
fn a_missing_config_exits_with_the_config_status() {
    let scratch = hostkeeper_testkit::Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "bin")
        .expect("a scratch dir");
    let config = scratch.path().join("missing.toml");
    let out = Command::new(env!("CARGO_BIN_EXE_hostkeeper"))
        .arg("daemon")
        .arg("--config")
        .arg(&config)
        .arg("--state-dir")
        .arg(scratch.path().join("state"))
        .arg("--packages-dir")
        .arg(scratch.path().join("packages"))
        .arg("--admin-socket")
        .arg(scratch.path().join("r/a.sock"))
        .output()
        .expect("run hostkeeper");
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains(&config.display().to_string()), "{stderr}");
    assert!(!scratch.path().join("state").exists());
}

/// Catches `ctl` not reaching `hostkeeper-ctl` through the real binary, or its help going to the
/// wrong stream: `hostkeeper ctl --help` exits 0 and lists every service of the generated table
/// on stdout.
#[test]
fn ctl_is_forwarded_and_main_has_no_logic() {
    let out = Command::new(env!("CARGO_BIN_EXE_hostkeeper"))
        .arg("ctl")
        .arg("--help")
        .output()
        .expect("run hostkeeper");
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    for command in hostkeeper_proto::ctl::COMMANDS {
        assert!(stdout.contains(command.service_command), "{stdout}");
    }
    assert!(out.stderr.is_empty(), "{out:?}");
    let main = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.rs"))
        .expect("main.rs");
    assert!(!main.contains("ctl"), "{main}");
}

/// Catches a binary whose `--version` is not the release stamp: it prints `hostkeeper `, the
/// crate version, and `+` with the first 12 digits of the build's commit when it has one, then
/// exits 0.
#[test]
fn version_prints_the_release_stamp() {
    let out = Command::new(env!("CARGO_BIN_EXE_hostkeeper"))
        .arg("--version")
        .output()
        .expect("run hostkeeper");
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let expected = format!(
        "hostkeeper {}\n",
        hostkeeper_daemon::stamp::version_text(
            env!("CARGO_PKG_VERSION"),
            option_env!("RELEASE_COMMIT")
        )
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), expected);
}
