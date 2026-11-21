//! The maintenance runs in the file store: the ops file's `maintenance_runs` key. It is written
//! only when a run is held, so a coordinator that holds none writes what the release before reads;
//! once one is held, the strict reader of the release before refuses the file. No clock, no timing.

use std::fs;
use std::path::Path;

use super::conformance_tests::rich_ops;
use super::{Backend, BackendError, Commit, Files};
use crate::audit_file::DEFAULT_ROTATION;
use crate::maintenance_runs::tests::sample;
use crate::maintenance_runs::{Act, Kind, Run, RunHost, Skip, Step};
use crate::ops::OpsState;
use crate::testutil::{LEGACY, scratch};

/// The reader of the release before this one for the ops file, as strict as it is
/// (`deny_unknown_fields`), with the keys it knew; what it holds below them is read as any JSON.
#[allow(dead_code, reason = "the fields are read by serde only")]
mod previous {
    use serde::Deserialize;
    use serde_json::Value;

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct OpsFile {
        schema: u32,
        hosts: Option<Value>,
        excluded: Option<Value>,
        services: Option<Value>,
        audit: Option<Value>,
        holds: Option<Value>,
        assignments: Option<Value>,
        inventory: Option<Value>,
        host_groups: Option<Value>,
    }

    /// Whether the release before starts on the ops file `text`.
    pub(super) fn reads_ops(text: &[u8]) -> Result<(), String> {
        serde_json::from_slice::<OpsFile>(text)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

fn files_over(dir: &Path) -> Files {
    Files::default()
        .with_desired(&dir.join("desired.json"), Some(LEGACY))
        .with_ops(&dir.join("desired.json.ops"), DEFAULT_ROTATION)
        .expect("the ops files")
}

fn commit_ops(files: &Files, after: &OpsState) -> Result<(), BackendError> {
    files.commit(&Commit {
        ops: Some((&OpsState::default(), after)),
        ..Commit::default()
    })
}

fn ops_text(dir: &Path) -> Vec<u8> {
    fs::read(dir.join("desired.json.ops")).expect("the ops file")
}

/// A run with every key that is written only when set left unset, and one with them set.
fn hand_run() -> Run {
    let host = |host_id: &str, step: Step, skipped_because: Option<Skip>| RunHost {
        host_id: host_id.to_owned(),
        host_group: String::new(),
        step,
        since_unix: 0,
        kinds: if skipped_because.is_some() {
            Vec::new()
        } else {
            vec![Kind::Linux]
        },
        applied: Vec::new(),
        skipped_because,
        reason: String::new(),
        offered_again: 0,
        baked_beats: 0,
        queued: None,
        order_try: 0,
    };
    Run {
        id: "run-1900000000-1".to_owned(),
        kinds: vec![Kind::Linux, Kind::Reboot],
        host_ids: vec!["h1".to_owned(), "h2".to_owned()],
        host_group: String::new(),
        window_start_unix: 0,
        window_end_unix: 1_900_003_600,
        batch: Some(2),
        drain_deadline_secs: Some(0),
        take_last_host: true,
        reason: "patch".to_owned(),
        by: "alice".to_owned(),
        scheduled_unix: 1_900_000_000,
        cancelled_unix: None,
        hosts: vec![
            host("h1", Step::Waiting, None),
            host("h2", Step::Skipped, Some(Skip::Held)),
        ],
    }
}

/// Proves: an ops file of a state that holds no run has no `maintenance_runs` key and the release
/// before reads it; one that holds a run has the key with the run's JSON (a value left unset is
/// not written), the strict reader of the release before refuses it naming the key, and the
/// files read the run back equal.
///
/// Catches: a key written for nothing (which would refuse the release before on a farm that
/// schedules no run), a run written with a key it need not have, or a run lost at load.
#[test]
fn an_ops_file_with_a_run_is_refused_by_the_release_before_and_read_back() {
    let calm = scratch("files-maintenance-calm");
    let mut none = rich_ops();
    none.maintenance_runs.clear();
    commit_ops(&files_over(calm.path()), &none).expect("committed");
    let text = ops_text(calm.path());
    assert!(!String::from_utf8_lossy(&text).contains("maintenance_runs"));
    assert_eq!(previous::reads_ops(&text), Ok(()));
    assert_eq!(
        files_over(calm.path())
            .load_ops()
            .expect("loads")
            .maintenance_runs,
        Vec::<Run>::new()
    );

    let dir = scratch("files-maintenance-run");
    let mut after = rich_ops();
    after.maintenance_runs = vec![hand_run()];
    commit_ops(&files_over(dir.path()), &after).expect("committed");
    let text = ops_text(dir.path());
    let value: serde_json::Value = serde_json::from_slice(&text).expect("JSON");
    assert_eq!(
        value["maintenance_runs"],
        serde_json::json!([{
            "id": "run-1900000000-1",
            "kinds": ["linux", "reboot"],
            "host_ids": ["h1", "h2"],
            "window_end_unix": 1_900_003_600_u64,
            "batch": 2,
            "drain_deadline_secs": 0,
            "take_last_host": true,
            "reason": "patch",
            "by": "alice",
            "scheduled_unix": 1_900_000_000_u64,
            "hosts": [
                {"host_id": "h1", "step": "waiting", "since_unix": 0, "kinds": ["linux"]},
                {"host_id": "h2", "step": "skipped", "since_unix": 0,
                 "skipped_because": "held"}
            ]
        }])
    );
    let refused = previous::reads_ops(&text).expect_err("the release before is strict");
    assert!(
        refused.starts_with("unknown field `maintenance_runs`"),
        "{refused}"
    );
    let back = files_over(dir.path()).load_ops().expect("loads");
    assert_eq!(back.maintenance_runs, after.maintenance_runs);
}

/// Proves: the runs a coordinator committed (every shape the sample holds) are held by a
/// coordinator that starts again over the same directory, equal and in their order, and a later
/// commit that cancels one is held the same way.
///
/// Catches: a run, a host's turn or a cancel lost at a restart.
#[test]
fn runs_committed_are_loaded_equal_after_a_restart() {
    let dir = scratch("files-maintenance-restart");
    let mut first = rich_ops();
    first.maintenance_runs = sample();
    commit_ops(&files_over(dir.path()), &first).expect("committed");
    let again = files_over(dir.path());
    let loaded = again.load_ops().expect("loads");
    assert_eq!(loaded.maintenance_runs, sample());

    let mut second = loaded.clone();
    second.maintenance_runs[0].cancelled_unix = Some(1_900_000_500);
    again
        .commit(&Commit {
            ops: Some((&loaded, &second)),
            ..Commit::default()
        })
        .expect("committed");
    let last = files_over(dir.path()).load_ops().expect("loads");
    assert_eq!(last.maintenance_runs, second.maintenance_runs);
    assert_eq!(last.maintenance_runs[0].cancelled_unix, Some(1_900_000_500));
}

/// Proves: a halted host's queued act and its last order try are the keys `queued` and `order_try`
/// of its turn in the ops file, written only when set (a turn with neither writes neither, as a
/// file written before them has none), read back equal after a restart; a file without the keys
/// reads as no act and try 0; and a file naming an act other than `retry` or `put_back` is
/// refused at load naming it.
///
/// Catches: an act or a try lost at a restart (a queued retry never sent, or sent again under an
/// older try), a key written for a turn that has none, and an unknown act read as one.
#[test]
fn a_halted_hosts_queued_act_and_order_try_are_kept_in_the_ops_file() {
    let dir = scratch("files-maintenance-queued");
    let mut run = hand_run();
    run.hosts[0].step = Step::Halted;
    run.hosts[0].queued = Some(Act::Retry);
    run.hosts[0].order_try = 2;
    run.hosts[1].step = Step::Halted;
    run.hosts[1].queued = Some(Act::PutBack);
    run.hosts[1].order_try = 3;
    let mut after = rich_ops();
    after.maintenance_runs = vec![run.clone()];
    commit_ops(&files_over(dir.path()), &after).expect("committed");
    let text = ops_text(dir.path());
    let value: serde_json::Value = serde_json::from_slice(&text).expect("JSON");
    let hosts = &value["maintenance_runs"][0]["hosts"];
    assert_eq!(hosts[0]["queued"], "retry");
    assert_eq!(hosts[0]["order_try"], 2);
    assert_eq!(hosts[1]["queued"], "put_back");
    assert_eq!(hosts[1]["order_try"], 3);
    let back = files_over(dir.path()).load_ops().expect("loads");
    assert_eq!(back.maintenance_runs, vec![run]);

    let plain = scratch("files-maintenance-unqueued");
    let mut before = rich_ops();
    before.maintenance_runs = vec![hand_run()];
    commit_ops(&files_over(plain.path()), &before).expect("committed");
    let text = ops_text(plain.path());
    let value: serde_json::Value = serde_json::from_slice(&text).expect("JSON");
    for host in value["maintenance_runs"][0]["hosts"]
        .as_array()
        .expect("the hosts")
    {
        assert!(host.get("queued").is_none(), "{host}");
        assert!(host.get("order_try").is_none(), "{host}");
    }
    let loaded = files_over(plain.path()).load_ops().expect("loads");
    assert_eq!(
        loaded.maintenance_runs[0]
            .hosts
            .iter()
            .map(|host| (host.queued, host.order_try))
            .collect::<Vec<_>>(),
        vec![(None, 0), (None, 0)]
    );

    let mut odd = value;
    odd["maintenance_runs"][0]["hosts"][0]["queued"] = serde_json::json!("nap");
    fs::write(
        plain.path().join("desired.json.ops"),
        serde_json::to_vec(&odd).expect("JSON"),
    )
    .expect("the file is written");
    let refused = files_over(plain.path())
        .load_ops()
        .expect_err("an unknown act is refused");
    assert!(refused.to_string().contains("\"nap\" is not a Act"), "{refused}");
}
