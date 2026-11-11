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
        .output()
        .expect("run hostkeeper");
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains(&config.display().to_string()), "{stderr}");
    assert!(!scratch.path().join("state").exists());
}

/// Proves: the real binary answers `hostkeeper ctl --help` as a usage error: exit status 2,
/// clap's unknown verb naming `ctl` on stderr, and nothing on stdout.
///
/// Catches: a `ctl` verb left in the binary, or one that prints help and succeeds.
#[test]
fn ctl_is_a_usage_error() {
    let out = Command::new(env!("CARGO_BIN_EXE_hostkeeper"))
        .arg("ctl")
        .arg("--help")
        .output()
        .expect("run hostkeeper");
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.starts_with("error: unrecognized subcommand 'ctl'\n"),
        "{stderr}"
    );
}

/// Proves: the real binary answers `hostkeeper ci set-desired --help` as a usage error: exit
/// status 2, clap's unknown verb naming `ci` on stderr, and nothing on stdout.
///
/// Catches: a `ci` verb left in the binary, or one that prints help and succeeds.
#[test]
fn ci_is_a_usage_error() {
    let out = Command::new(env!("CARGO_BIN_EXE_hostkeeper"))
        .args(["ci", "set-desired", "--help"])
        .output()
        .expect("run hostkeeper");
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.starts_with("error: unrecognized subcommand 'ci'\n"),
        "{stderr}"
    );
}

/// Proves: the real binary answers `hostkeeper admin` and `hostkeeper admin status` as usage
/// errors: exit status 2, clap's unknown verb naming `admin` on stderr, and nothing on stdout.
///
/// Catches: an `admin` verb left in the binary, or one that still dials the daemon's socket.
#[test]
fn admin_is_a_usage_error() {
    for args in [&["admin"][..], &["admin", "status"][..]] {
        let out = Command::new(env!("CARGO_BIN_EXE_hostkeeper"))
            .args(args)
            .output()
            .expect("run hostkeeper");
        assert_eq!(out.status.code(), Some(2), "{args:?}: {out:?}");
        assert!(out.stdout.is_empty(), "{args:?}: {out:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.starts_with("error: unrecognized subcommand 'admin'\n"),
            "{args:?}: {stderr}"
        );
    }
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

/// Proves: the real binary answers each verb of the update before the hand-over as a usage
/// error: exit status 2, clap's unknown verb naming it on stderr, and nothing on stdout, with
/// the arguments the units, the pkg and the releases called it with.
///
/// Catches: a removed verb (`self-update-protocol`, `self-update-fallback`, `self-update-seed`,
/// `mac-config`, `update-hook` or one of its step words) left in the binary, or one that does
/// something and exits 0.
#[test]
fn the_verbs_of_the_update_before_the_hand_over_are_usage_errors() {
    for args in [
        &["self-update-protocol"][..],
        &["self-update-fallback", "--dir", "x"],
        &["self-update-seed", "--state-dir", "x", "--binary", "y"],
        &["mac-config"],
        &["update-hook", "prepare"],
        &["prepare"],
        &["cutover"],
        &["verify"],
        &["rollback"],
        &["cleanup"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_hostkeeper"))
            .args(args)
            .output()
            .expect("run hostkeeper");
        assert_eq!(out.status.code(), Some(2), "{args:?}: {out:?}");
        assert!(out.stdout.is_empty(), "{args:?}: {out:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.starts_with(&format!("error: unrecognized subcommand '{}'\n", args[0])),
            "{args:?}: {stderr}"
        );
    }
}

/// Proves: the real binary answers `hostkeeper daemon --admin-socket <path>`, the flag of the
/// units before stage 2, as a usage error: exit status 2, clap's unexpected argument naming the
/// flag on stderr, nothing on stdout, and nothing made on disk.
///
/// Catches: the flag accepted and ignored again, so that a daemon started by such a unit goes
/// on to read its config.
#[test]
fn the_admin_socket_flag_is_a_usage_error() {
    let scratch = hostkeeper_testkit::Scratch::under(
        Path::new(env!("CARGO_TARGET_TMPDIR")),
        "bin-admin-socket",
    )
    .expect("a scratch dir");
    let out = Command::new(env!("CARGO_BIN_EXE_hostkeeper"))
        .arg("daemon")
        .arg("--config")
        .arg(scratch.path().join("missing.toml"))
        .arg("--state-dir")
        .arg(scratch.path().join("state"))
        .arg("--admin-socket")
        .arg(scratch.path().join("r/a.sock"))
        .output()
        .expect("run hostkeeper");
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.starts_with("error: unexpected argument '--admin-socket' found\n"),
        "{stderr}"
    );
    assert!(!scratch.path().join("state").exists());
    assert!(!scratch.path().join("r").exists());
}

/// Catches `--trusted-root-max-age-days` still taken by the real binary: the daemon checks no
/// attestation, so the flag is one it does not have, a usage error, exit 2, naming the flag, and
/// nothing is made.
#[test]
fn the_trusted_root_age_limit_flag_is_a_usage_error() {
    let scratch =
        hostkeeper_testkit::Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "bin-root-age")
            .expect("a scratch dir");
    let out = Command::new(env!("CARGO_BIN_EXE_hostkeeper"))
        .arg("daemon")
        .arg("--config")
        .arg(scratch.path().join("missing.toml"))
        .arg("--state-dir")
        .arg(scratch.path().join("state"))
        .arg("--packages-dir")
        .arg(scratch.path().join("packages"))
        .arg("--trusted-root-max-age-days")
        .arg("30")
        .output()
        .expect("run hostkeeper");
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--trusted-root-max-age-days"), "{stderr}");
    assert!(!scratch.path().join("state").exists());
}

/// The version a hand-over waits for in
/// [`a_hand_over_execs_next_with_the_same_arguments_until_its_count_is_spent`]: not this
/// build's, so every start of the build is the old binary.
const HANDED_TO: &str = "9.9.9";

/// Catches a hand-over that is not a real exec with the process's own arguments, that keeps the
/// state directory's lock across the exec, or that never ends: the record of the service
/// `hostkeeper` waits for a hand-over into 9.9.9, and `versions/9.9.9` is a copy of this build,
/// so each start of it is the old binary again and execs it again. `hostkeeper daemon` with a
/// missing config execs itself with the same flags (a start that parsed others would not find
/// the record), each exec taking the lock again (a lock kept across the exec would leave the
/// record waiting), until the count is spent; that start settles the update `Failed`,
/// `self_update_fallback`, marks 9.9.9 bad, removes `next` and switches nothing, and then stops
/// at the missing config with its status, 3, saying on standard error exactly what the same
/// command line says with no hand-over (an exec that came back would say so).
#[test]
fn a_hand_over_execs_next_with_the_same_arguments_until_its_count_is_spent() {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    let scratch = hostkeeper_testkit::Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "ho")
        .expect("a scratch dir");
    let state = scratch.path().join("state");
    let hk = state.join("hostkeeper");
    let bin = hk.join("bin");
    fs::create_dir_all(bin.join("versions")).expect("the binary directory");
    for dir in [&state, &hk, &bin] {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).expect("its mode");
    }
    fs::copy(
        env!("CARGO_BIN_EXE_hostkeeper"),
        bin.join("versions").join(HANDED_TO),
    )
    .expect("this build as 9.9.9");
    symlink(Path::new("versions").join(HANDED_TO), bin.join("next")).expect("next");
    let record = format!(
        r#"{{"schema":1,"service":"hostkeeper","target_version":"{HANDED_TO}","artifact_digest":"{a}","tarball_digest":"{c}","previous_digest":null,"generation":1,"params":"/state/hostkeeper/params.toml","step":"cleanup","attempt":1,"state":{{"state":"done","code":0,"last_line":null}},"hook":"new","progress":{{"committed":true,"promotion_pending":false,"then":null,"retry":0,"verify_started":null,"watching":null,"reason":null,"awaiting_restart":{{"version":"{HANDED_TO}","verdict_promotion_pending":false,"order_id":"order-1","next":"{HANDED_TO}","starts":0}}}},"order_try":1}}"#,
        a = "ab".repeat(32),
        c = "cd".repeat(32),
    );
    fs::write(hk.join("step-record.json"), record).expect("the record");

    let config = scratch.path().join("missing.toml");
    let daemon = |state: &Path| {
        Command::new(env!("CARGO_BIN_EXE_hostkeeper"))
            .arg("daemon")
            .arg("--config")
            .arg(&config)
            .arg("--state-dir")
            .arg(state)
            .arg("--packages-dir")
            .arg(scratch.path().join("packages"))
            .output()
            .expect("run hostkeeper")
    };
    // What the same command line says with no hand-over: the config error, and nothing else.
    let plain = daemon(&scratch.path().join("no-state"));
    assert_eq!(plain.status.code(), Some(3), "{plain:?}");
    let out = daemon(&state);
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    // No exec came back and nothing went unread: the last start says only what a start with no
    // hand-over says.
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&plain.stderr)
    );

    // The record as the daemon writes it (pretty JSON): finished, failed after the commit of
    // class release with the fallback's reason, the order id kept, no wait left.
    let rec = fs::read_to_string(hk.join("step-record.json")).expect("the record");
    for line in [
        r#""state": "finished""#,
        r#""Failed": {"#,
        r#""phase": "after_commit""#,
        r#""class": "release""#,
        r#""reason": "self_update_fallback""#,
        r#""report_order_id": "order-1""#,
    ] {
        assert!(rec.contains(line), "{line} in {rec}");
    }
    assert!(!rec.contains("awaiting_restart"), "{rec}");
    let bad = fs::read_to_string(hk.join("bad-versions")).expect("the list of bad versions");
    assert!(
        bad.contains(&format!(r#""versions":["{HANDED_TO}"]"#)),
        "{bad}"
    );
    assert!(
        fs::symlink_metadata(bin.join("next")).is_err(),
        "next is removed"
    );
    assert!(
        fs::symlink_metadata(bin.join("current")).is_err(),
        "nothing is switched"
    );
}
