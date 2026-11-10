//! `ci` is not a verb of the `hostkeeper` binary: CI sets a desired version with `hkctl`
//! (docs/deploy/ci-client.md), which `set-desired.yml` downloads and verifies. A caller pinned to
//! an older revision of that workflow runs the `hostkeeper ci` of the older release it downloads.

use clap::Parser;
use clap::error::ErrorKind;
use hostkeeper_daemon::cli::Cli;

/// Proves: `ci`, alone, with `--help`, with `set-desired --help`, or with the whole command line
/// an older `set-desired.yml` ran, is a usage error: clap's unknown verb, exit status 2, naming
/// `ci`, before anything runs (no token is asked for, no file is read).
///
/// Catches: a `ci` verb left in the command line, or its arguments taken by another verb.
#[test]
fn ci_is_a_usage_error_whatever_follows_it() {
    let digest = "0".repeat(64);
    let pinned = [
        "hostkeeper",
        "ci",
        "set-desired",
        "--service",
        "web",
        "--version",
        "1.0.0",
        "--package-digest",
        &digest,
        "--endpoint",
        "https://coordinator.example",
        "--ca-file",
        "ca.pem",
        "--audience",
        "web-release",
        "--work-dir",
        "work",
        "--web-roots",
        "roots.pem",
    ];
    for line in [
        &["hostkeeper", "ci"][..],
        &["hostkeeper", "ci", "--help"],
        &["hostkeeper", "ci", "-h"],
        &["hostkeeper", "ci", "set-desired", "--help"],
        &pinned,
    ] {
        let refused = Cli::try_parse_from(line).expect_err("ci is no verb");
        assert_eq!(refused.kind(), ErrorKind::InvalidSubcommand, "{line:?}");
        assert_eq!(refused.exit_code(), 2, "{line:?}");
        let text = refused.to_string();
        assert!(
            text.starts_with("error: unrecognized subcommand 'ci'\n"),
            "{line:?}: {text}"
        );
    }
}

/// Proves: `hostkeeper --help` lists `daemon` and not `ci`.
///
/// Catches: a `ci` verb still offered in the program's help.
#[test]
fn the_program_help_lists_daemon_and_not_ci() {
    let help = Cli::try_parse_from(["hostkeeper", "--help"]).expect_err("help is not a command");
    assert_eq!(help.kind(), ErrorKind::DisplayHelp);
    let text = help.to_string();
    assert!(text.contains("\n  daemon "), "{text}");
    assert!(!text.contains("\n  ci "), "{text}");
}
