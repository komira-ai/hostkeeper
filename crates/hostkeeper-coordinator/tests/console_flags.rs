//! `hostkeeper-coordinator serve --console-*`: the console's files and the BMC tool's settings.
//! The operators file needs the credential file beside it, and the targets file is optional; a
//! valid set starts the coordinator with them, a broken file refuses the start naming its flag, and the BMC tool's
//! settings reach the console. Driven in-process through `config::{Cli, build}`; nothing binds a
//! socket and no tool is run.

mod support;

use std::ffi::OsString;

use clap::Parser;
use hostkeeper_coordinator::config::{Cli, StartError};
use support::serve::{Files, build_given, flags_of, refusal};

const OPERATORS: &str = "[[operator]]\nname = \"alice\"\n";
const TARGETS: &str = "[[target]]\nmac = \"0c:c4:7a:00:00:01\"\naddress = \"192.0.2.12\"\n";
const CREDENTIAL: &str = "user = \"ADMIN\"\npassword = \"a long random pass\"\n";

/// The arguments of a `serve` that starts, with the three console files of `operators`,
/// `targets` and `credential` (a file of each that is `None` is not named).
fn args_with(
    files: &Files,
    operators: Option<&str>,
    targets: Option<&str>,
    credential: Option<&str>,
) -> Vec<OsString> {
    let mut args = files.args();
    for (flag, name, text, mode) in [
        (
            "--console-operators-file",
            "operators.toml",
            operators,
            0o644,
        ),
        ("--console-targets-file", "targets.toml", targets, 0o644),
        (
            "--console-bmc-credential-file",
            "bmc.toml",
            credential,
            0o600,
        ),
    ] {
        if let Some(text) = text {
            files.put(name, text, mode);
            args.push(flag.into());
            args.push(files.path(name).into());
        }
    }
    args
}

/// Proves: the three files start the coordinator with them in use, and the BMC tool's flags
/// reach its settings; without the flags the console reads no file.
///
/// Catches: a console flag that parses and is not read.
#[test]
fn the_console_files_and_settings_start_the_coordinator() {
    let files = Files::new("console-flags-start");
    let mut args = args_with(&files, Some(OPERATORS), Some(TARGETS), Some(CREDENTIAL));
    for arg in [
        "--console-ipmi-cipher",
        "17",
        "--console-ipmi-privilege",
        "operator",
        "--console-ipmi-timeout-s",
        "4",
        "--console-ipmi-retries",
        "0",
    ] {
        args.push(arg.into());
    }
    let parts = build_given(&flags_of(args)).expect("the start");
    let shown = format!("{:?}", parts.coordinator.console());
    assert!(shown.contains("operators.toml"), "{shown}");
    assert!(
        shown.contains("cipher: 17, privilege: Operator, timeout_secs: 4, retries: 0"),
        "{shown}"
    );
    let none = Files::new("console-flags-none");
    let mut args = none.args();
    args.push("--console-ipmi-privilege".into());
    args.push("user".into());
    let parts = build_given(&flags_of(args)).expect("the start");
    let shown = format!("{:?}", parts.coordinator.console());
    assert!(shown.contains("paths: None"), "{shown}");
    assert!(shown.contains("quiet: 10s"), "{shown}");
    assert!(
        shown.contains("cipher: 3, privilege: User, timeout_secs: 1, retries: 2"),
        "{shown}"
    );
}

/// Proves: the session bounds and the dial's timeout have the design's defaults (8 sessions, 30
/// seconds to attach, 30 minutes idle, 8 hours in all, 5 seconds to connect to a Mac, 10 seconds of quiet before the page says no output came), each flag
/// sets its own, and the bounds reach the console; a value outside a flag's range is a usage
/// error.
///
/// Catches: a limit flag that parses and is not used, or that takes 0 (a stream closed at once).
#[test]
fn the_stream_limits_have_their_defaults_and_flags() {
    use std::time::Duration;

    let files = Files::new("console-flags-limits");
    let flags = flags_of(files.args());
    assert_eq!(
        flags.console.bounds(),
        hostkeeper_console::session::Bounds {
            max: 8,
            window: Duration::from_secs(30),
            idle: Duration::from_secs(1_800),
            total: Duration::from_secs(28_800),
        }
    );
    assert_eq!(flags.console.connect_timeout(), Duration::from_secs(5));
    assert_eq!(flags.console.console_quiet_s, 10);
    let mut args = files.args();
    for arg in [
        "--console-max-sessions",
        "2",
        "--console-attach-s",
        "7",
        "--console-idle-s",
        "60",
        "--console-total-s",
        "3600",
        "--console-connect-timeout-s",
        "9",
        "--console-quiet-s",
        "3600",
    ] {
        args.push(arg.into());
    }
    let flags = flags_of(args.clone());
    assert_eq!(flags.console.connect_timeout(), Duration::from_secs(9));
    let parts = build_given(&flags).expect("the start");
    let shown = format!("{:?}", parts.coordinator.console());
    assert!(
        shown.contains("bounds: Bounds { max: 2, window: 7s, idle: 60s, total: 3600s }"),
        "{shown}"
    );
    assert!(shown.contains("quiet: 3600s"), "{shown}");
    for (flag, value) in [
        ("--console-idle-s", "0"),
        ("--console-total-s", "0"),
        ("--console-connect-timeout-s", "0"),
        ("--console-connect-timeout-s", "61"),
        ("--console-quiet-s", "0"),
        ("--console-quiet-s", "3601"),
    ] {
        let mut args = files.args();
        args.push(flag.into());
        args.push(value.into());
        assert!(Cli::try_parse_from(args).is_err(), "{flag} {value}");
    }
}

/// Proves: the operators file without the credential file is a usage error naming it, with or
/// without the targets file; the operators and credential files without the targets file start
/// the coordinator with them in use and no targets file (every BMC's address is then found).
///
/// Catches: a console that grants access and has no credential to act with, and a targets file
/// still required although the addresses are found without it.
#[test]
fn the_operators_file_needs_the_credential_file_and_not_the_targets_file() {
    let files = Files::new("console-flags-requires");
    for targets in [Some(TARGETS), None] {
        let args = args_with(&files, Some(OPERATORS), targets, None);
        let refused = Cli::try_parse_from(args).expect_err("refused");
        let said = refused.to_string();
        assert!(said.contains("--console-bmc-credential-file"), "{said}");
    }
    let files = Files::new("console-flags-no-targets");
    let args = args_with(&files, Some(OPERATORS), None, Some(CREDENTIAL));
    let parts = build_given(&flags_of(args)).expect("the start");
    let shown = format!("{:?}", parts.coordinator.console());
    assert!(shown.contains("operators.toml"), "{shown}");
    assert!(shown.contains("bmc.toml"), "{shown}");
    assert!(shown.contains("targets: None"), "{shown}");
}

/// Proves: a broken file refuses the start with its flag's error, and a credential file open to
/// others is refused as a secret file is, its text in no error.
///
/// Catches: a coordinator that starts with a console file it cannot read, and a BMC password
/// printed in a start error.
#[test]
fn a_broken_console_file_refuses_the_start_naming_its_flag() {
    let files = Files::new("console-flags-broken");
    let why = refusal(args_with(
        &files,
        Some("[[operator]]\nname = \"NOT\"\n"),
        Some(TARGETS),
        Some(CREDENTIAL),
    ));
    assert!(matches!(why, StartError::Console(_)), "{why}");
    assert!(
        why.to_string().starts_with("--console-operators-file: "),
        "{why}"
    );
    let files = Files::new("console-flags-open-credential");
    let mut args = args_with(&files, Some(OPERATORS), Some(TARGETS), None);
    files.put("bmc.toml", CREDENTIAL, 0o644);
    args.push("--console-bmc-credential-file".into());
    args.push(files.path("bmc.toml").into());
    let said = refusal(args).to_string();
    assert!(said.starts_with("--console-bmc-credential-file"), "{said}");
    assert!(!said.contains("a long random pass"), "{said}");
}

/// Proves: the usage of `serve` names each console flag.
///
/// Catches: a flag that works and is not in the help.
#[test]
fn the_help_names_the_console_flags() {
    let help =
        Cli::try_parse_from(["hostkeeper-coordinator", "serve", "--help"]).expect_err("help");
    let text = help.to_string();
    for flag in [
        "--console-operators-file",
        "--console-targets-file",
        "--console-bmc-credential-file",
        "--console-ipmitool",
        "--console-ipmi-cipher",
        "--console-ipmi-privilege",
        "--console-ipmi-timeout-s",
        "--console-ipmi-retries",
        "--console-attach-s",
        "--console-max-sessions",
        "--console-idle-s",
        "--console-total-s",
        "--console-connect-timeout-s",
        "--console-quiet-s",
    ] {
        assert!(text.contains(flag), "{flag}");
    }
}
