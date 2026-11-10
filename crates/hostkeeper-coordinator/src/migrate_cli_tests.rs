//! The verb `hostkeeper-coordinator host-groups migrate` as the binary parses and runs it.

use std::ffi::OsString;
use std::process::ExitCode;

use clap::Parser;
use clap::error::ErrorKind;

use crate::config::{Cli, Command, main};
use crate::migrate::HostGroupsCommand;
use crate::migrate_tests::{Disk, ROLLOUT};

fn args(words: &[&str]) -> Vec<OsString> {
    std::iter::once("hostkeeper-coordinator")
        .chain(words.iter().copied())
        .map(OsString::from)
        .collect()
}

/// Proves: `host-groups migrate` takes the rollout file, one of the file store's flags or the
/// database's, the legacy service, `--live-rollouts` (1 to 16, default 1), the Mac allowlist and
/// `--apply`, which is off unless given; the rollout file is required, and so is one store.
///
/// Catches: a flag renamed from the one `serve` names it by, or a verb that can run with no store
/// named.
#[test]
fn the_verb_takes_the_stores_flags_and_writes_only_with_apply() {
    let parse = |words: &[&str]| {
        let mut all = vec!["host-groups", "migrate"];
        all.extend(words);
        Cli::try_parse_from(args(&all))
    };
    let Command::HostGroups(HostGroupsCommand::Migrate(flags)) = parse(&[
        "--rollout-file",
        "r.toml",
        "--state-file",
        "s.json",
        "--enrollment-dir",
        "e",
    ])
    .expect("flags")
    .command
    else {
        panic!("the verb");
    };
    assert!(!flags.apply);
    assert_eq!(flags.live_rollouts, 1);
    assert_eq!(flags.legacy_service, None);
    let Command::HostGroups(HostGroupsCommand::Migrate(flags)) = parse(&[
        "--rollout-file",
        "r.toml",
        "--database-dsn-file",
        "d",
        "--live-rollouts",
        "16",
        "--legacy-service",
        "kbf",
        "--mac-serial-allowlist",
        "m.toml",
        "--apply",
    ])
    .expect("flags")
    .command
    else {
        panic!("the verb");
    };
    assert!(flags.apply);
    assert_eq!(flags.live_rollouts, 16);
    assert_eq!(flags.legacy_service.as_deref(), Some("kbf"));
    for refused in [
        vec!["--state-file", "s.json"],
        vec!["--rollout-file", "r.toml"],
        vec![
            "--rollout-file",
            "r.toml",
            "--state-file",
            "s",
            "--live-rollouts",
            "0",
        ],
        vec![
            "--rollout-file",
            "r.toml",
            "--state-file",
            "s",
            "--live-rollouts",
            "17",
        ],
        vec![
            "--rollout-file",
            "r.toml",
            "--state-file",
            "s",
            "--services-file",
            "x",
        ],
    ] {
        assert!(parse(&refused).is_err(), "{refused:?}");
    }
    assert_eq!(
        Cli::try_parse_from(args(&["host-groups", "migrate", "--help"]))
            .expect_err("help")
            .kind(),
        ErrorKind::DisplayHelp
    );
    assert!(Cli::try_parse_from(args(&["host-groups"])).is_err());
}

/// Proves: run through the binary's entry, a dry run of a store that holds no error is exit 0, one
/// whose plan holds an error is exit 1, and `--apply` over a good plan is exit 0.
///
/// Catches: a verb that exits 0 on a plan the operator must not apply.
#[test]
fn the_exit_is_0_for_a_plan_without_an_error_and_1_otherwise() {
    // `main` installs the process's log subscriber if none is: the one the store's tests read
    // their alerts from goes first.
    crate::store::postgres::tests::logging();
    let disk = Disk::new("migrate-cli");
    let words = |apply: bool| {
        let mut words = vec![
            "host-groups".to_owned(),
            "migrate".to_owned(),
            "--rollout-file".to_owned(),
            disk.rollout.display().to_string(),
            "--state-file".to_owned(),
            disk.state.display().to_string(),
            "--enrollment-dir".to_owned(),
            disk.enrollment.display().to_string(),
        ];
        if apply {
            words.push("--apply".to_owned());
        }
        words.into_iter().map(OsString::from).collect::<Vec<_>>()
    };
    let run = |apply: bool| {
        let mut all = vec![OsString::from("hostkeeper-coordinator")];
        all.extend(words(apply));
        main(all)
    };
    assert_eq!(run(false), ExitCode::SUCCESS);
    disk.put_rollout(&ROLLOUT.replace("\"private\"", "\"missing\""));
    assert_eq!(run(false), ExitCode::from(1));
    assert_eq!(run(true), ExitCode::from(1));
    disk.put_rollout(ROLLOUT);
    assert_eq!(run(true), ExitCode::SUCCESS);
    assert_eq!(run(true), ExitCode::from(1));
}
