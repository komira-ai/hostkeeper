//! `ctl` is not a verb of the `hostkeeper` binary: the coordinator's command line is `hkctl`
//! (docs/cli.md), generated from the protos, and nothing of the daemon's command line reaches
//! the coordinator's API.

use clap::Parser;
use clap::error::ErrorKind;
use hostkeeper_daemon::cli::Cli;

/// Proves: `ctl`, alone, with `--help` or with a command and flags it took before, is a usage
/// error: clap's unknown verb, exit status 2, naming `ctl`, before anything runs.
///
/// Catches: a `ctl` verb left in the command line, or arguments after it taken by another verb.
#[test]
fn ctl_is_a_usage_error_whatever_follows_it() {
    for line in [
        &["hostkeeper", "ctl"][..],
        &["hostkeeper", "ctl", "--help"],
        &["hostkeeper", "ctl", "-h"],
        &[
            "hostkeeper",
            "ctl",
            "hosts",
            "list",
            "--endpoint",
            "https://coordinator.example",
        ],
    ] {
        let refused = Cli::try_parse_from(line).expect_err("ctl is no verb");
        assert_eq!(refused.kind(), ErrorKind::InvalidSubcommand, "{line:?}");
        assert_eq!(refused.exit_code(), 2, "{line:?}");
        let text = refused.to_string();
        assert!(
            text.starts_with("error: unrecognized subcommand 'ctl'\n"),
            "{line:?}: {text}"
        );
    }
}

/// Proves: `hostkeeper --help` lists `daemon` and not `ctl`.
///
/// Catches: a `ctl` verb still offered in the program's help.
#[test]
fn the_program_help_lists_daemon_and_not_ctl() {
    let help = Cli::try_parse_from(["hostkeeper", "--help"]).expect_err("help is not a command");
    assert_eq!(help.kind(), ErrorKind::DisplayHelp);
    let text = help.to_string();
    assert!(text.contains("\n  daemon "), "{text}");
    assert!(!text.contains("\n  ctl "), "{text}");
}
