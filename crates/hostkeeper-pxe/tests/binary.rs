//! The `pxe` binary forwards to its library: run as a program, it answers `--version` with its
//! name and version and exits 0, a command line that does not parse exits 2, an `update-hook` step reaches its
//! code with the exit status and the report of the contract, and `serve` reads the one thing it
//! takes from the environment, systemd's `NOTIFY_SOCKET`.

use std::process::Command;

/// The program, with the environment of a user who has no systemd notify socket.
fn pxe() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_pxe"));
    command.env_remove("NOTIFY_SOCKET");
    command
}

/// Catches a binary that does not forward its arguments, or drops the library's exit status.
#[test]
fn the_binary_forwards_its_arguments_and_exit_status() {
    let version = pxe().arg("--version").output().expect("run it");
    assert_eq!(version.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&version.stdout),
        format!("pxe {}\n", env!("CARGO_PKG_VERSION"))
    );
    let usage = pxe().arg("serve").output().expect("run it");
    assert_eq!(usage.status.code(), Some(2));
    let gone = pxe().arg("fetch").output().expect("run it");
    assert_eq!(gone.status.code(), Some(2));
    let bare = pxe()
        .args(["uninstall", "--params", "p"])
        .output()
        .expect("run it");
    assert_eq!(bare.status.code(), Some(2));
}

/// Run as a program, an `update-hook` command line the contract cannot read exits 2 with the one
/// line `pxe update-hook: <why>` and nothing on standard output.
///
/// Catches a binary whose hook parse refusal exits 1 or prints a report.
#[test]
fn the_binary_refuses_an_unreadable_update_hook_command_line() {
    let output = pxe()
        .args(["update-hook", "prepare"])
        .output()
        .expect("run it");
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "pxe update-hook: prepare needs the staged release directory\n"
    );
}

/// Run as a program, a step reaches the hook: a params file that is not there fails the step
/// with exit status 1, one line on standard error naming the step and the file, and the report
/// of the contract as the last line of standard output; nothing is made.
///
/// Catches a binary whose steps never read the updater's params, or print the report elsewhere.
#[test]
fn the_binary_fails_a_step_whose_params_are_missing() {
    let base = std::env::current_exe().expect("the test binary");
    let missing = base.with_file_name("pxe-binary-scratch-missing.toml");
    let output = pxe()
        .args(["update-hook", "uninstall", "--params"])
        .arg(&missing)
        .output()
        .expect("run it");
    assert_eq!(output.status.code(), Some(1));
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(
        err.starts_with(&format!("pxe uninstall: {}: ", missing.display())),
        "{err}"
    );
    assert_eq!(err.lines().count(), 1, "{err}");
    let out = String::from_utf8_lossy(&output.stdout);
    assert_eq!(out.lines().count(), 1, "{out}");
    assert!(out.contains("\"class\":\"environment\""), "{out}");
    assert!(out.contains("\"reason\":\"bad_params\""), "{out}");
}

/// `serve --notify-systemd` takes the notify socket from the environment systemd sets: with none,
/// the start is refused naming it; with one, the start goes on to read its daemon config, which
/// is not there, and the line is about that file instead.
///
/// Catches a binary that never reads `NOTIFY_SOCKET`, or reads it when the flag is not given.
#[test]
fn the_binary_reads_the_notify_socket_for_the_flag() {
    let base = std::env::current_exe().expect("the test binary");
    let config = base.with_file_name("pxe-binary-scratch-daemon.toml");
    let serve = |socket: Option<&str>, flag: bool| {
        let mut command = pxe();
        command.args([
            "serve",
            "--mode",
            "router",
            "--address",
            "192.0.2.10",
            "--coordinator",
            "https://192.0.2.1:8444",
            "--ca-file",
            "ca",
            "--token-file",
            "token",
            "--dir",
            "d",
            "--ssh-authorized-keys",
            "keys",
            "--user-id",
            "65534",
            "--group-id",
            "65534",
            "--daemon-config",
        ]);
        command.arg(&config);
        if flag {
            command.arg("--notify-systemd");
        }
        if let Some(socket) = socket {
            command.env("NOTIFY_SOCKET", socket);
        }
        let output = command.output().expect("run it");
        assert_eq!(output.status.code(), Some(1));
        String::from_utf8_lossy(&output.stderr).into_owned()
    };
    assert_eq!(
        serve(None, true),
        "pxe serve: --notify-systemd was given, but systemd set no notify socket (NOTIFY_SOCKET)\n"
    );
    let with_socket = serve(Some("/run/none"), true);
    assert!(
        with_socket.starts_with(&format!("pxe serve: {}: ", config.display())),
        "{with_socket}"
    );
    let no_flag = serve(Some("/run/none"), false);
    assert!(
        no_flag.starts_with(&format!("pxe serve: {}: ", config.display())),
        "{no_flag}"
    );
}
