//! `serve`'s start over `--macos-update-via`, the way a Mac's macOS update of a maintenance run
//! is applied: without the flag, and with `daemon`, the start is as before; `apple-business`,
//! which the design names and this build does not take, and any other value stop the start
//! (exit 2), from the command line and from the `--config` file alike.

mod support;

use std::ffi::OsString;
use std::process::ExitCode;

use clap::Parser;
use clap::error::ErrorKind;
use hostkeeper_coordinator::config::Cli;
use hostkeeper_coordinator::config_maintenance::MacosUpdateVia;
use support::serve::{Files, build_given, exit_code, flags_of};

/// Proves: a start without the flag, and one with `--macos-update-via daemon`, are accepted, the
/// flag is the daemon in both, and the usage lists the flag.
///
/// Catches: a default that refuses every start, and a flag missing from the usage.
#[test]
fn a_start_without_the_flag_or_with_daemon_is_accepted() {
    let files = Files::new("start-maintenance-daemon");
    let mut given = files.args();
    given.extend([
        OsString::from("--macos-update-via"),
        OsString::from("daemon"),
    ]);
    for args in [files.args(), given] {
        let flags = flags_of(args);
        assert_eq!(flags.maintenance.macos_update_via, MacosUpdateVia::Daemon);
        build_given(&flags).expect("a start with the daemon");
    }
    let help =
        Cli::try_parse_from(["hostkeeper-coordinator", "serve", "--help"]).expect_err("help");
    assert!(help.to_string().contains("--macos-update-via"), "{help}");
}

/// Proves: `serve --macos-update-via apple-business` is refused by the parser as a value that
/// fails validation, naming the flag and the reason, and the binary's entry exits 2.
///
/// Catches: a start that accepts a way no call can carry out.
#[test]
fn apple_business_stops_the_start_with_exit_2() {
    let files = Files::new("start-maintenance-apple-business");
    let mut given = files.args();
    given.extend([
        OsString::from("--macos-update-via"),
        OsString::from("apple-business"),
    ]);
    let refused = Cli::try_parse_from(given.clone()).expect_err("refused");
    assert_eq!(refused.kind(), ErrorKind::ValueValidation);
    let text = refused.to_string();
    assert!(text.contains("--macos-update-via"), "{text}");
    assert!(text.contains("Apple Business offers no call"), "{text}");
    assert_eq!(exit_code(given), ExitCode::from(2));
}

/// Proves: another value (`mdm`, the empty one) stops the start with exit 2 too.
///
/// Catches: a value that falls through to the daemon.
#[test]
fn another_value_stops_the_start_with_exit_2() {
    let files = Files::new("start-maintenance-other");
    for value in ["mdm", ""] {
        let mut given = files.args();
        given.push(OsString::from(format!("--macos-update-via={value}")));
        let refused = Cli::try_parse_from(given.clone()).expect_err("refused");
        assert_eq!(refused.kind(), ErrorKind::ValueValidation, "{value:?}");
        assert_eq!(exit_code(given), ExitCode::from(2), "{value:?}");
    }
}

/// Proves: `macos-update-via = "apple-business"` in the `--config` file stops the start with
/// exit 2, as the flag does.
///
/// Catches: a file that sets the way around the flag's check.
#[test]
fn the_config_file_key_is_refused_like_the_flag() {
    let files = Files::new("start-maintenance-config");
    files.put(
        "serve.toml",
        "macos-update-via = \"apple-business\"\n",
        0o644,
    );
    let mut given = files.args();
    given.extend([
        OsString::from("--config"),
        files.path("serve.toml").into_os_string(),
    ]);
    assert_eq!(exit_code(given), ExitCode::from(2));
}
