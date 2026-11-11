//! The verbs and the flag of the update before the hand-over are gone from the `hostkeeper`
//! command line (docs/design/two-programs.md, section 4, stage 3): `hostkeeper daemon` is the
//! verb that runs the daemon, and what the units, the pkg and the releases before it used to call
//! is a usage error.

use clap::Parser;
use clap::error::ErrorKind;
use hostkeeper_daemon::cli::Cli;

/// The command lines the removed verbs were called with: the plumbing verbs of the fallback and
/// the seed, the Mac config, the hook of hostkeeper's own package and the step words a launcher
/// ran it with, and the check of a repositories directory.
const REMOVED: [&[&str]; 11] = [
    &["self-update-protocol"],
    &["self-update-fallback", "--dir", "/x"],
    &["self-update-seed", "--state-dir", "/x", "--binary", "/y"],
    &["mac-config"],
    &["update-hook", "prepare"],
    &["prepare"],
    &["cutover"],
    &["verify", "extra"],
    &["rollback"],
    &["cleanup"],
    &["check-repositories", "--dir", "/x"],
];

/// Proves: each removed verb, with the arguments it was called with, is clap's unknown
/// subcommand, exit status 2, naming the verb, before anything runs.
///
/// Catches: a removed verb left in the command line, or its arguments taken by another verb.
#[test]
fn a_removed_verb_is_a_usage_error() {
    for words in REMOVED {
        let line: Vec<&str> = ["hostkeeper"]
            .into_iter()
            .chain(words.iter().copied())
            .collect();
        let refused = Cli::try_parse_from(&line).expect_err("the verb is gone");
        assert_eq!(refused.kind(), ErrorKind::InvalidSubcommand, "{line:?}");
        assert_eq!(refused.exit_code(), 2, "{line:?}");
        let text = refused.to_string();
        assert!(
            text.starts_with(&format!("error: unrecognized subcommand '{}'\n", words[0])),
            "{line:?}: {text}"
        );
    }
}

/// Proves: `daemon --admin-socket <path>`, the flag of the units before stage 2, is clap's unknown
/// argument, exit status 2, naming the flag; with the rest of such a unit's flags too.
///
/// Catches: the flag accepted and ignored again.
#[test]
fn the_admin_socket_flag_is_a_usage_error() {
    for line in [
        &["hostkeeper", "daemon", "--admin-socket", "/x"][..],
        &[
            "hostkeeper",
            "daemon",
            "--config",
            "/etc/hostkeeper/hostkeeper.toml",
            "--admin-socket=/run/hostkeeper/admin.sock",
            "--notify-systemd",
        ],
    ] {
        let refused = Cli::try_parse_from(line).expect_err("the flag is gone");
        assert_eq!(refused.kind(), ErrorKind::UnknownArgument, "{line:?}");
        assert_eq!(refused.exit_code(), 2, "{line:?}");
        let text = refused.to_string();
        assert!(
            text.starts_with("error: unexpected argument '--admin-socket"),
            "{line:?}: {text}"
        );
    }
}

/// Proves: `hostkeeper --help` lists `daemon` and none of the removed verbs, and `daemon --help`
/// does not list the flag.
///
/// Catches: a removed verb or flag still offered in the help.
#[test]
fn the_help_lists_only_the_daemon() {
    let help = Cli::try_parse_from(["hostkeeper", "--help"]).expect_err("help is not a command");
    assert_eq!(help.kind(), ErrorKind::DisplayHelp);
    let text = help.to_string();
    assert!(text.contains("\n  daemon "), "{text}");
    for words in REMOVED {
        assert!(!text.contains(&format!("\n  {} ", words[0])), "{text}");
    }
    let daemon = Cli::try_parse_from(["hostkeeper", "daemon", "--help"]).expect_err("help");
    assert!(!daemon.to_string().contains("admin-socket"), "{daemon}");
}
