//! `--record-approve-for` and `--record-approve-max` of `serve`: the length of the approval
//! window of a new inventory record and the longest one, as `Enrollment.AddRecord` and
//! `Enrollment.OpenRecord` apply them. They need no enrollment: the inventory lives in the ops
//! file with or without it.

mod support;

use std::ffi::OsString;
use std::process::ExitCode;

use clap::Parser;
use hostkeeper_coordinator::config::{Cli, StartError};
use hostkeeper_coordinator::inventory::{
    DEFAULT_APPROVE_FOR_SECS, DEFAULT_APPROVE_MAX_SECS, RecordWindow,
};
use support::serve::{Files, build_given, exit_code, flags_of, refusal};

/// `args` and then the flag `flag` with `value`, as one argument when it starts with `-`.
fn plus(args: &[OsString], flag: &str, value: &str) -> Vec<OsString> {
    let mut args = args.to_vec();
    if value.starts_with('-') {
        args.push(format!("{flag}={value}").into());
    } else {
        args.extend([OsString::from(flag), OsString::from(value)]);
    }
    args
}

/// Proves: both flags take a whole number and a unit (`s`, `m`, `h` or `d`) of at least one
/// second, and are refused (exit 2) when they are anything else: empty, no unit, no digits, zero,
/// another case or unit, a sign, a space, a fraction, or more seconds than are counted. Their
/// defaults are 24 hours and 7 days, they are in the usage with those, and neither is read from
/// the environment.
#[test]
fn the_window_flags_take_a_whole_time_of_at_least_a_second() {
    let files = Files::new("window-flags-refused");
    for flag in ["--record-approve-for", "--record-approve-max"] {
        for value in [
            "",
            "h",
            "24",
            "0s",
            "0d",
            "24H",
            "1.5h",
            "+1h",
            " 24h",
            "24 h",
            "24hh",
            "1w",
            "-1h",
            "99999999999999999999s",
            "213503982334602d",
        ] {
            assert_eq!(
                exit_code(plus(&files.args(), flag, value)),
                ExitCode::from(2),
                "{flag} {value:?}"
            );
        }
    }
    for (value, secs) in [
        ("1s", 1),
        ("90m", 5400),
        ("24h", 86_400),
        ("7d", 7 * 86_400),
    ] {
        let flags = flags_of(plus(
            &plus(&files.args(), "--record-approve-for", value),
            "--record-approve-max",
            "213503982334601d",
        ));
        assert_eq!(flags.record_approve_for, secs, "{value}");
        assert_eq!(flags.record_approve_max, 213_503_982_334_601 * 86_400);
    }

    let defaults = flags_of(files.args());
    assert_eq!(
        (defaults.record_approve_for, defaults.record_approve_max),
        (DEFAULT_APPROVE_FOR_SECS, DEFAULT_APPROVE_MAX_SECS)
    );
    let help = Cli::try_parse_from(["hostkeeper-coordinator", "serve", "--help"])
        .expect_err("help")
        .to_string();
    assert!(
        help.contains("--record-approve-for <RECORD_APPROVE_FOR>")
            && help.contains("--record-approve-max <RECORD_APPROVE_MAX>"),
        "{help}"
    );
    assert!(
        help.contains("[default: 24h]") && help.contains("[default: 7d]"),
        "{help}"
    );
    assert!(!help.contains("[env:"), "{help}");
}

/// Proves: the start gives the coordinator the limits the flags say, with no enrollment flag
/// given; a default window longer than the most stops the start (`--record-approve-for` is
/// named, exit 1) and one equal to it does not.
#[test]
fn the_start_applies_the_limits_and_refuses_a_default_longer_than_the_most() {
    let files = Files::new("window-flags-start");
    let defaults = build_given(&flags_of(files.args())).expect("a start");
    assert_eq!(
        defaults.coordinator.record_window(),
        RecordWindow::default()
    );
    assert!(defaults.coordinator.enrollment().is_none());
    drop(defaults);

    let args = plus(
        &plus(&files.args(), "--record-approve-for", "2h"),
        "--record-approve-max",
        "3d",
    );
    let parts = build_given(&flags_of(args)).expect("a start");
    let window = parts.coordinator.record_window();
    assert_eq!((window.for_secs(), window.max_secs()), (7200, 3 * 86_400));
    drop(parts);

    let equal = plus(
        &plus(&files.args(), "--record-approve-for", "90m"),
        "--record-approve-max",
        "90m",
    );
    // The start holds the state files' lock for as long as its parts live.
    assert!(build_given(&flags_of(equal)).is_ok());

    let above = plus(
        &plus(&files.args(), "--record-approve-for", "8d"),
        "--record-approve-max",
        "7d",
    );
    let why = refusal(above.clone());
    assert!(matches!(why, StartError::RecordWindow(_)), "{why}");
    assert!(why.to_string().contains("--record-approve-for"), "{why}");
    assert_eq!(exit_code(above), ExitCode::from(1));
    let default_above_a_short_max = plus(&files.args(), "--record-approve-max", "1h");
    assert!(matches!(
        refusal(default_above_a_short_max),
        StartError::RecordWindow(_)
    ));
}
