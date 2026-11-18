//! `hostkeeper-coordinator serve --console-*`: the console's files and the BMC tool's settings.
//! The operators file needs the targets and credential files beside it; a valid set starts the
//! coordinator with them, a broken file refuses the start naming its flag, and the BMC tool's
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
    assert!(
        shown.contains("cipher: 3, privilege: User, timeout_secs: 1, retries: 2"),
        "{shown}"
    );
}

/// Proves: the operators file without the targets file or the credential file is a usage error,
/// naming the flag that is missing.
///
/// Catches: a console that grants access and has no address or credential to act with.
#[test]
fn the_operators_file_needs_the_other_two() {
    let files = Files::new("console-flags-requires");
    for (targets, credential, missing) in [
        (None, Some(CREDENTIAL), "--console-targets-file"),
        (Some(TARGETS), None, "--console-bmc-credential-file"),
    ] {
        let args = args_with(&files, Some(OPERATORS), targets, credential);
        let refused = Cli::try_parse_from(args).expect_err("refused");
        assert!(refused.to_string().contains(missing), "{refused}");
    }
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
    ] {
        assert!(text.contains(flag), "{flag}");
    }
}
