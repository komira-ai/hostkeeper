//! The entry point and its flags.

use std::process::ExitCode;

use clap::Parser;

use crate::testing::{flags, scratch};
use crate::{CiCommand, run, status};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: CiCommand,
}

fn parse(extra: &[&str]) -> Result<crate::SetDesiredFlags, clap::Error> {
    let mut args = vec![
        "hostkeeper-ci",
        "set-desired",
        "--service",
        "svc",
        "--version",
        "1.2.3",
        "--package-digest",
        "0000000000000000000000000000000000000000000000000000000000000000",
        "--endpoint",
        "https://coordinator.example",
        "--ca-file",
        "ca.pem",
        "--audience",
        "hostkeeper-ci",
        "--work-dir",
        "work",
        "--web-roots",
        "roots.pem",
    ];
    args.extend_from_slice(extra);
    Cli::try_parse_from(args).map(|cli| match cli.command {
        CiCommand::SetDesired(flags) => flags,
    })
}

/// Catches a flag spelled otherwise, a default that moved, and `--version` taken by clap's own.
#[test]
fn the_flags_parse_with_their_defaults() {
    let flags = parse(&[]).expect("flags");
    assert_eq!(flags.service, "svc");
    assert_eq!(flags.service_version, "1.2.3");
    assert!(flags.wait);
    assert_eq!(flags.max_polls, 180);
    assert_eq!(flags.pause_secs, 10);
    assert_eq!(flags.github_output, None);
    assert_eq!(flags.package_digest.as_deref().map(str::len), Some(64));
    assert_eq!(flags.release_index_file, None);
    assert_eq!(flags.release_index_digest, None);
    let given = parse(&[
        "--wait",
        "false",
        "--max-polls",
        "7",
        "--pause-secs",
        "0",
        "--github-output",
        "out",
    ])
    .expect("flags");
    assert!(!given.wait);
    assert_eq!(given.max_polls, 7);
    assert_eq!(given.pause_secs, 0);
    assert_eq!(
        given.github_output.as_deref(),
        Some(std::path::Path::new("out"))
    );
}

/// Catches a bad flag that gets as far as a token request, and `run` mapping a status to the
/// wrong exit code. The tests' environment has no OIDC request variables, so a valid command
/// ends at the token with exit 1 (a usage error is 2, a done job 0).
#[test]
fn run_without_the_platform_variables_fails_at_the_token() {
    let dir = scratch("run");
    let valid = flags(dir.path());
    assert_eq!(status(CiCommand::SetDesired(valid.clone())), 1);
    assert!(!dir.path().join("hostkeeper-oidc-token").exists());
    assert!(!dir.path().join("set-desired.json").exists());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("github-output")).expect("outputs"),
        "desired_version=\nconverged=false\nreason=no_answer\n"
    );

    let mut invalid = valid;
    invalid.audience = String::new();
    assert_eq!(status(CiCommand::SetDesired(invalid.clone())), 2);
    assert_eq!(
        format!("{:?}", run(CiCommand::SetDesired(invalid))),
        format!("{:?}", ExitCode::from(2))
    );
}

/// Catches the index flags spelled otherwise, the package digest still being required, and an
/// empty value (what the workflow passes for an input left out) being refused by the parser
/// instead of read as absent.
#[test]
fn the_release_flags_are_optional_and_may_be_empty() {
    let mut args = vec![
        "hostkeeper-ci",
        "set-desired",
        "--service",
        "svc",
        "--version",
        "1.2.3",
        "--release-index-file",
        "index.json",
        "--release-index-digest",
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "--endpoint",
        "https://coordinator.example",
        "--ca-file",
        "ca.pem",
        "--audience",
        "hostkeeper-ci",
        "--work-dir",
        "work",
        "--web-roots",
        "roots.pem",
    ];
    let CiCommand::SetDesired(index) = Cli::try_parse_from(args.clone()).expect("flags").command;
    assert_eq!(index.package_digest, None);
    assert_eq!(
        index.release_index_file.as_deref(),
        Some(std::path::Path::new("index.json"))
    );
    assert_eq!(
        index.release_index_digest.as_deref().map(str::len),
        Some(64)
    );
    args.extend(["--package-digest", ""]);
    let CiCommand::SetDesired(empty) = Cli::try_parse_from(args).expect("flags").command;
    assert_eq!(empty.package_digest.as_deref(), Some(""));
}
