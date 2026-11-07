//! The verbs of `hostkeeper-coordinator`: `serve`, `backup`, `restore`, `import` and `export`, as
//! the binary parses them and the exit codes of `backup` and `restore`, driven in-process through
//! `config::main`. What a backup and a restore copy, check and refuse is in the crate's
//! `backup_tests` and `backup_refusal_tests`; what an import and an export move and refuse, in
//! its `transfer_tests` and the integration test `transfer.rs`.

mod support;

use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;

use clap::Parser;
use clap::error::ErrorKind;
use hostkeeper_coordinator::config::{Cli, Command, main};
use support::scratch;

fn args(words: &[&str]) -> Vec<OsString> {
    std::iter::once("hostkeeper-coordinator")
        .chain(words.iter().copied())
        .map(OsString::from)
        .collect()
}

fn text(path: &Path) -> &str {
    path.to_str().expect("a UTF-8 path")
}

/// The flags of a `serve`; parsing reads none of the files.
const SERVE: [&str; 17] = [
    "serve",
    "--heartbeat-listen",
    "192.0.2.1:1",
    "--private-listen",
    "192.0.2.1:2",
    "--tls-cert",
    "tls.pem",
    "--tls-key",
    "tls.key",
    "--client-ca",
    "ca.pem",
    "--state-file",
    "desired.json",
    "--oidc-config",
    "oidc.toml",
    "--rollout-file",
    "rollout.toml",
];

/// Catches: a verb missing or one too many, or a `serve` that can start without the services
/// it manages: the verbs are `serve`, `backup`, `restore`, `import` and `export`, any other is
/// refused, and `serve`'s `--rollout-file` is required. An import writes only with `--apply`.
#[test]
fn the_verbs_are_serve_backup_restore_import_and_export() {
    assert!(matches!(
        Cli::try_parse_from(args(&SERVE)).expect("flags").command,
        Command::Serve(_)
    ));
    let backup = args(&["backup", "--state-file", "s.json", "--out", "snap"]);
    let Command::Backup(flags) = Cli::try_parse_from(backup).expect("flags").command else {
        panic!("backup")
    };
    assert_eq!(flags.paths.ops_file(), Path::new("s.json.ops"));
    assert_eq!(flags.paths.enrollment_dir, None);
    let restore = args(&[
        "restore",
        "--from",
        "snap",
        "--state-file",
        "s.json",
        "--ops-file",
        "o.json",
        "--enrollment-dir",
        "enr",
        "--into-empty",
    ]);
    let Command::Restore(flags) = Cli::try_parse_from(restore).expect("flags").command else {
        panic!("restore")
    };
    assert_eq!(flags.paths.ops_file(), Path::new("o.json"));
    assert_eq!(
        flags.paths.enrollment_dir.as_deref(),
        Some(Path::new("enr"))
    );
    assert!(flags.into_empty);
    let import = args(&[
        "import",
        "--state-file",
        "s.json",
        "--database-dsn-file",
        "db.dsn",
        "--legacy-service",
        "kbf",
    ]);
    let Command::Import(flags) = Cli::try_parse_from(import).expect("flags").command else {
        panic!("import")
    };
    assert_eq!(flags.paths.ops_file(), Path::new("s.json.ops"));
    assert_eq!(flags.database_dsn_file, Path::new("db.dsn"));
    assert_eq!(flags.legacy_service.as_deref(), Some("kbf"));
    assert!(!flags.apply, "an import without --apply writes nothing");
    let export = args(&[
        "export",
        "--database-dsn-file",
        "db.dsn",
        "--state-file",
        "s.json",
        "--enrollment-dir",
        "enr",
        "--into-empty",
    ]);
    let Command::Export(flags) = Cli::try_parse_from(export).expect("flags").command else {
        panic!("export")
    };
    assert_eq!(
        flags.paths.enrollment_dir.as_deref(),
        Some(Path::new("enr"))
    );
    assert!(flags.into_empty);
    assert_eq!(
        Cli::try_parse_from(args(&["run"]))
            .expect_err("no such verb")
            .kind(),
        ErrorKind::InvalidSubcommand
    );
    let missing = Cli::try_parse_from(args(&SERVE[..SERVE.len() - 2]))
        .expect_err("--rollout-file is required");
    assert_eq!(missing.kind(), ErrorKind::MissingRequiredArgument);
    assert!(missing.to_string().contains("--rollout-file"), "{missing}");
}

/// Catches: a restore or an export that runs without saying it writes only into empty targets,
/// or a backup without its place: `restore` and `export` without `--into-empty`, `backup`
/// without `--out` and `import` without `--database-dsn-file` are usage errors, exit 2, and the
/// help of each names the flags `serve` names the state by.
#[test]
fn restore_requires_into_empty_and_backup_requires_out() {
    let refused = Cli::try_parse_from(args(&["restore", "--from", "s", "--state-file", "x"]))
        .expect_err("--into-empty is required");
    assert_eq!(refused.kind(), ErrorKind::MissingRequiredArgument);
    assert!(refused.to_string().contains("--into-empty"), "{refused}");
    assert_eq!(
        main(args(&["restore", "--from", "s", "--state-file", "x"])),
        ExitCode::from(2)
    );
    assert_eq!(
        main(args(&["backup", "--state-file", "x"])),
        ExitCode::from(2)
    );
    let refused = Cli::try_parse_from(args(&[
        "export",
        "--database-dsn-file",
        "d",
        "--state-file",
        "x",
    ]))
    .expect_err("--into-empty is required");
    assert_eq!(refused.kind(), ErrorKind::MissingRequiredArgument);
    assert!(refused.to_string().contains("--into-empty"), "{refused}");
    assert_eq!(
        main(args(&["import", "--state-file", "x"])),
        ExitCode::from(2)
    );
    for verb in ["backup", "restore", "import", "export"] {
        let help = Cli::try_parse_from(args(&[verb, "--help"])).expect_err("help");
        assert_eq!(help.kind(), ErrorKind::DisplayHelp);
        for flag in ["--state-file", "--ops-file", "--enrollment-dir"] {
            assert!(help.to_string().contains(flag), "{verb}: {help}");
        }
    }
}

/// Catches: a backup or a restore whose outcome is not its exit code: a backup made and a
/// restore done exit 0; a restore into targets it has just filled and a backup over a
/// snapshot are refused, exit 1.
#[test]
fn a_backup_and_a_restore_exit_0_and_a_refused_restore_exit_1() {
    let dir = scratch("verbs-exit");
    let state = dir.path().join("desired.json");
    std::fs::write(&state, b"{}\n").expect("a state file");
    let snapshot = dir.path().join("snapshot");
    assert_eq!(
        main(args(&[
            "backup",
            "--state-file",
            text(&state),
            "--out",
            text(&snapshot)
        ])),
        ExitCode::SUCCESS
    );
    let target = dir.path().join("restored").join("desired.json");
    let restore = args(&[
        "restore",
        "--from",
        text(&snapshot),
        "--state-file",
        text(&target),
        "--into-empty",
    ]);
    assert_eq!(main(restore.clone()), ExitCode::SUCCESS);
    assert_eq!(std::fs::read(&target).expect("restored"), b"{}\n");
    assert_eq!(main(restore), ExitCode::from(1));
    let again = args(&[
        "backup",
        "--state-file",
        text(&state),
        "--out",
        text(&snapshot),
    ]);
    assert_eq!(main(again), ExitCode::from(1), "a backup over a snapshot");
}
