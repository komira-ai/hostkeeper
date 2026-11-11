//! `hostkeeper-coordinator serve` reads no rollout file: `--rollout-file` is refused before any
//! file is opened, the old name `--services-file` is no argument, and the coordinator `serve`
//! builds places hosts by host group from the start.

mod support;

use std::ffi::OsString;
use std::process::ExitCode;

use clap::Parser;
use clap::error::ErrorKind;
use hostkeeper_coordinator::config::{Cli, StartError};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use support::serve::{Files, build_given, exit_code, flags_of, refusal};
use support::{OPERATOR, recorded};

/// The arguments of `files`' `serve` with `--rollout-file <path>` added.
fn with_rollout(files: &Files, path: &str) -> Vec<OsString> {
    let mut args = files.args();
    args.extend(["--rollout-file".into(), path.into()]);
    args
}

/// Proves: `serve` given `--rollout-file`, whether the file exists or not, is refused with the
/// exact text naming the operator API's calls that place the hosts instead, as exit code 1, and opens nothing: no state file is
/// made and no lock file is taken.
///
/// Catches: a stale unit file whose rollout file is read again, or one whose refusal comes after
/// the state files are opened.
#[test]
fn serve_given_a_rollout_file_is_refused_before_any_file_is_opened() {
    let files = Files::new("retired-refused");
    files.put("rollout.toml", &recorded("services.toml"), 0o600);
    let present = files.path("rollout.toml").display().to_string();
    let absent = files.path("absent.toml").display().to_string();
    for path in [present, absent] {
        let args = with_rollout(&files, &path);

        let why = refusal(args.clone());

        assert!(matches!(why, StartError::RolloutFileRetired), "{why}");
        assert_eq!(
            why.to_string(),
            "--rollout-file is not read by serve: start serve without it and place hosts with \
             the operator API: HostGroups.Put each group, HostGroups.Move each host, \
             Services.SetRepository and Services.SetMinVersion where needed, Releases.SetDesired \
             per group (docs/operations/hostgroups.md)"
        );
        assert_eq!(exit_code(args), ExitCode::from(1));
        assert!(!files.path("desired.json").exists());
        assert!(!files.path("desired.json.lock").exists());
    }
}

/// Proves: `--services-file`, the name `--rollout-file` once had, is an unknown argument (exit
/// 2), and `--rollout-file` is hidden from the usage of `serve`.
///
/// Catches: the hidden alias left in place, which would let a stale unit file give the retired
/// flag under its old name and get another message.
#[test]
fn the_old_name_of_the_flag_is_no_argument_and_the_flag_is_not_in_the_help() {
    let files = Files::new("retired-alias");
    let mut args = files.args();
    args.extend(["--services-file".into(), "rollout.toml".into()]);

    let unknown = Cli::try_parse_from(args.clone()).expect_err("an unknown argument");

    assert_eq!(unknown.kind(), ErrorKind::UnknownArgument);
    assert_eq!(exit_code(args), ExitCode::from(2));
    let help = Cli::try_parse_from(["hostkeeper-coordinator", "serve", "--help"])
        .expect_err("help")
        .to_string();
    assert!(!help.contains("--rollout-file"), "{help}");
}

/// Proves: the coordinator `serve` builds is in group mode while it holds no group (`Assign` is
/// refused `placed_by_host_group`) and manages no service of its own, and it starts without any
/// rollout file.
///
/// Catches: a start that falls back to role placement until the first group is put.
#[tokio::test]
async fn the_coordinator_serve_builds_places_by_group_from_the_start() {
    let files = Files::new("retired-group-mode");

    let parts = build_given(&flags_of(files.args())).expect("a start");

    let operator = parts.coordinator.caller(Some(OPERATOR)).await;
    let refused = parts.coordinator.assign(
        &operator,
        proto::AssignRequest {
            node_id: "w1".to_owned(),
            service: "kbf".to_owned(),
            role: String::new(),
        },
    );
    assert_eq!(
        refused.err(),
        Some(proto::ServicesRefusal::PlacedByHostGroup)
    );
    let status = parts.coordinator.status(Some(OPERATOR)).expect("status");
    let shown = serde_json::to_value(&status).expect("json");
    assert_eq!(shown["services"], serde_json::json!({}));
}
