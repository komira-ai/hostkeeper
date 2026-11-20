//! The maintenance runs ([`super`]): the names of their enums, their JSON, where a run stands at a
//! time, cancelling, pruning and the ids.

use std::fmt::Debug;

use hostkeeper_proto::hostkeeper::agent::v1::MaintenanceKind;
use hostkeeper_proto::hostkeeper::operator::v1::{
    MaintenanceHostStep, MaintenanceSkip, MaintenanceState,
};
use hostkeeper_wire::wire::OrderId;
use serde::de::DeserializeOwned;
use serde_json::json;

use super::{
    Kind, MAX_OFFERED_AGAIN, MAX_RUNS, Run, RunHost, Skip, Step, new_id, order_id_of, prune,
};

const NOW: u64 = 1_900_000_000;

/// A few runs for the tests of the stores that keep runs (the ops file, the conformance suite, the
/// database's rows): they must round-trip exactly, so a store that reads back what it wrote
/// returns these runs equal, field for field and in the same order.
///
/// The first run names hosts and has a window, a batch, a drain deadline of 0 and takes the last
/// host; its hosts, sorted by id, are waiting, left out for each [`Skip`] (one without a group)
/// and not reached. The second names a host group, has no window, batch or drain deadline, was
/// cancelled, and has a done host that applied kinds and has baked 2 heartbeats, and a halted one
/// with a reason. The third names one host, has a batch of 1 and a drain deadline of 900, and a
/// host staging under its turn's second offer.
pub(crate) fn sample() -> Vec<Run> {
    let host = |id: &str, group: &str, step: Step, since: u64, kinds: &[Kind]| RunHost {
        host_id: id.to_owned(),
        host_group: group.to_owned(),
        step,
        since_unix: since,
        kinds: kinds.to_vec(),
        applied: Vec::new(),
        skipped_because: None,
        reason: String::new(),
        offered_again: 0,
        baked_beats: 0,
    };
    let skipped = |id: &str, group: &str, why: Skip, kinds: &[Kind]| RunHost {
        skipped_because: Some(why),
        ..host(id, group, Step::Skipped, 0, kinds)
    };
    let first = Run {
        id: "run-1900000000-1".to_owned(),
        kinds: vec![Kind::Macos, Kind::Linux],
        host_ids: ["host-a", "host-b", "host-c", "host-d", "host-e"]
            .map(str::to_owned)
            .to_vec(),
        host_group: String::new(),
        window_start_unix: 1_900_000_100,
        window_end_unix: 1_900_003_600,
        batch: Some(2),
        drain_deadline_secs: Some(0),
        take_last_host: true,
        reason: "kernel patch".to_owned(),
        by: "operator ana".to_owned(),
        scheduled_unix: 1_900_000_000,
        cancelled_unix: None,
        hosts: vec![
            host(
                "host-a",
                "web",
                Step::Waiting,
                0,
                &[Kind::Macos, Kind::Linux],
            ),
            skipped("host-b", "web", Skip::NoMaintenanceV1, &[Kind::Linux]),
            skipped("host-c", "db", Skip::Held, &[]),
            skipped("host-d", "", Skip::Excluded, &[]),
            host(
                "host-e",
                "web",
                Step::NotReached,
                1_900_000_050,
                &[Kind::Macos],
            ),
        ],
    };
    let second = Run {
        id: "run-1900000050-1".to_owned(),
        kinds: Vec::new(),
        host_ids: Vec::new(),
        host_group: "web".to_owned(),
        window_start_unix: 0,
        window_end_unix: 0,
        batch: None,
        drain_deadline_secs: None,
        take_last_host: false,
        reason: "put back".to_owned(),
        by: "token ci".to_owned(),
        scheduled_unix: 1_900_000_050,
        cancelled_unix: Some(1_900_000_200),
        hosts: vec![
            RunHost {
                applied: vec![Kind::Linux, Kind::Reboot],
                baked_beats: 2,
                ..host(
                    "host-f",
                    "web",
                    Step::Done,
                    1_900_000_150,
                    &[Kind::Linux, Kind::Reboot],
                )
            },
            RunHost {
                reason: "drain failed".to_owned(),
                ..host("host-g", "web", Step::Halted, 1_900_000_160, &[Kind::Linux])
            },
            host(
                "host-h",
                "web",
                Step::NotReached,
                1_900_000_200,
                &[Kind::Linux],
            ),
        ],
    };
    let third = Run {
        id: "run-1900000090-2".to_owned(),
        kinds: vec![Kind::Xcode],
        host_ids: vec!["host-i".to_owned()],
        host_group: String::new(),
        window_start_unix: 0,
        window_end_unix: 0,
        batch: Some(1),
        drain_deadline_secs: Some(900),
        take_last_host: false,
        reason: "xcode".to_owned(),
        by: "operator ana".to_owned(),
        scheduled_unix: 1_900_000_090,
        cancelled_unix: None,
        hosts: vec![RunHost {
            offered_again: 1,
            ..host(
                "host-i",
                "mac",
                Step::Staging,
                1_900_000_100,
                &[Kind::Xcode],
            )
        }],
    };
    vec![first, second, third]
}

fn host(id: &str, group: &str, step: Step, since: u64) -> RunHost {
    RunHost {
        host_id: id.to_owned(),
        host_group: group.to_owned(),
        step,
        since_unix: since,
        kinds: Vec::new(),
        applied: Vec::new(),
        skipped_because: None,
        reason: String::new(),
        offered_again: 0,
        baked_beats: 0,
    }
}

fn run(id: &str, hosts: Vec<RunHost>) -> Run {
    Run {
        id: id.to_owned(),
        kinds: Vec::new(),
        host_ids: Vec::new(),
        host_group: String::new(),
        window_start_unix: 0,
        window_end_unix: 0,
        batch: None,
        drain_deadline_secs: None,
        take_last_host: false,
        reason: "why".to_owned(),
        by: "who".to_owned(),
        scheduled_unix: 500,
        cancelled_unix: None,
        hosts,
    }
}

/// A run with a waiting host, or, when `ended`, cancelled.
fn numbered(n: usize, ended: bool) -> Run {
    let mut run = run(
        &format!("run-{n}"),
        vec![host("host-a", "web", Step::Waiting, 0)],
    );
    if ended {
        run.cancelled_unix = Some(10);
    }
    run
}

fn refuses<T: DeserializeOwned + Debug>(ty: &str) {
    let err = serde_json::from_str::<T>("\"bogus\"").expect_err("an unknown name");
    assert!(
        err.to_string()
            .contains(&format!("\"bogus\" is not a {ty}")),
        "{err}"
    );
}

/// Proves: every kind has its name, round-trips through `parse` and serde, and stands for its
/// proto kind; a name that is no kind is refused by `parse` and by serde, saying so.
#[test]
fn kind_names_and_proto() {
    let expected = [
        (Kind::Macos, "macos", MaintenanceKind::Macos),
        (Kind::Xcode, "xcode", MaintenanceKind::Xcode),
        (Kind::Linux, "linux", MaintenanceKind::Linux),
        (Kind::Reboot, "reboot", MaintenanceKind::Reboot),
    ];
    assert_eq!(Kind::ALL.len(), expected.len());
    for (kind, name, proto) in expected {
        assert_eq!(kind.as_str(), name);
        assert_eq!(Kind::parse(name), Some(kind));
        assert_eq!(kind.proto(), proto);
        assert_eq!(serde_json::to_value(kind).expect("serializes"), json!(name));
        assert_eq!(
            serde_json::from_value::<Kind>(json!(name)).expect("parses"),
            kind
        );
    }
    assert_eq!(Kind::ALL, expected.map(|(kind, ..)| kind));
    assert_eq!(Kind::parse("bogus"), None);
    assert_eq!(Kind::parse("Macos"), None);
    refuses::<Kind>("Kind");
}

/// Proves: every step has its name, round-trips through `parse` and serde, and stands for its
/// proto step; a name that is no step is refused by `parse` and by serde, saying so.
#[test]
fn step_names_and_proto() {
    let expected = [
        (Step::Waiting, "waiting", MaintenanceHostStep::Waiting),
        (Step::Staging, "staging", MaintenanceHostStep::Staging),
        (Step::Draining, "draining", MaintenanceHostStep::Draining),
        (Step::Applying, "applying", MaintenanceHostStep::Applying),
        (Step::Rebooting, "rebooting", MaintenanceHostStep::Rebooting),
        (Step::Checking, "checking", MaintenanceHostStep::Checking),
        (
            Step::PuttingBack,
            "putting_back",
            MaintenanceHostStep::PuttingBack,
        ),
        (Step::Done, "done", MaintenanceHostStep::Done),
        (Step::Deferred, "deferred", MaintenanceHostStep::Deferred),
        (Step::Halted, "halted", MaintenanceHostStep::Halted),
        (Step::NotBack, "not_back", MaintenanceHostStep::NotBack),
        (
            Step::NotReached,
            "not_reached",
            MaintenanceHostStep::NotReached,
        ),
        (Step::Skipped, "skipped", MaintenanceHostStep::Skipped),
    ];
    assert_eq!(Step::ALL.len(), expected.len());
    for (step, name, proto) in expected {
        assert_eq!(step.as_str(), name);
        assert_eq!(Step::parse(name), Some(step));
        assert_eq!(step.proto(), proto);
        assert_eq!(serde_json::to_value(step).expect("serializes"), json!(name));
        assert_eq!(
            serde_json::from_value::<Step>(json!(name)).expect("parses"),
            step
        );
    }
    assert_eq!(Step::ALL, expected.map(|(step, ..)| step));
    assert_eq!(Step::parse("bogus"), None);
    refuses::<Step>("Step");
}

/// Proves: every skip reason has its name, round-trips through `parse` and serde, and stands for
/// its proto value; a name that is no reason is refused by `parse` and by serde, saying so.
#[test]
fn skip_names_and_proto() {
    let expected = [
        (
            Skip::NoMaintenanceV1,
            "no_maintenance_v1",
            MaintenanceSkip::NoMaintenanceV1,
        ),
        (Skip::Held, "held", MaintenanceSkip::Held),
        (Skip::Excluded, "excluded", MaintenanceSkip::Excluded),
    ];
    assert_eq!(Skip::ALL.len(), expected.len());
    for (skip, name, proto) in expected {
        assert_eq!(skip.as_str(), name);
        assert_eq!(Skip::parse(name), Some(skip));
        assert_eq!(skip.proto(), proto);
        assert_eq!(serde_json::to_value(skip).expect("serializes"), json!(name));
        assert_eq!(
            serde_json::from_value::<Skip>(json!(name)).expect("parses"),
            skip
        );
    }
    assert_eq!(Skip::ALL, expected.map(|(skip, ..)| skip));
    assert_eq!(Skip::parse(""), None);
    refuses::<Skip>("Skip");
}

/// Proves: a step is in its turn from staging to putting back and while halted or not back, and
/// at no other step; only waiting and deferred may still start.
#[test]
fn step_in_turn_and_may_start() {
    for step in Step::ALL.iter().copied() {
        let in_turn = matches!(
            step,
            Step::Staging
                | Step::Draining
                | Step::Applying
                | Step::Rebooting
                | Step::Checking
                | Step::PuttingBack
                | Step::Halted
                | Step::NotBack
        );
        assert_eq!(step.in_turn(), in_turn, "{step:?}");
        let may_start = matches!(step, Step::Waiting | Step::Deferred);
        assert_eq!(step.may_start(), may_start, "{step:?}");
        assert!(!(step.in_turn() && step.may_start()), "{step:?}");
    }
    for step in [Step::Done, Step::NotReached, Step::Skipped] {
        assert!(!step.in_turn() && !step.may_start(), "{step:?}");
    }
}

/// Proves: a run with nothing optional is written with its required keys alone, and read back
/// from them alone.
#[test]
fn minimal_run_json() {
    let value = json!({
        "id": "run-1-1",
        "reason": "why",
        "by": "who",
        "scheduled_unix": 500,
        "hosts": [{"host_id": "host-a", "step": "waiting", "since_unix": 0}],
    });
    let minimal = run("run-1-1", vec![host("host-a", "", Step::Waiting, 0)]);
    assert_eq!(serde_json::to_value(&minimal).expect("serializes"), value);
    assert_eq!(
        serde_json::from_value::<Run>(value).expect("parses"),
        minimal
    );
    let bare = json!({"id": "run-1-1", "reason": "why", "by": "who", "scheduled_unix": 500});
    assert_eq!(
        serde_json::from_value::<Run>(bare).expect("parses"),
        run("run-1-1", Vec::new())
    );
}

/// Proves: a run with every key set is written under exactly these keys and values, with a
/// zero deadline kept (it means no deadline) and the first of the sample read back equal; a host's
/// bake and its offers after a deferral are written only when not 0, and read back.
#[test]
fn full_run_json() {
    let value = json!({
        "id": "run-1900000000-1",
        "kinds": ["macos", "linux"],
        "host_ids": ["host-a", "host-b", "host-c", "host-d", "host-e"],
        "window_start_unix": 1_900_000_100u64,
        "window_end_unix": 1_900_003_600u64,
        "batch": 2,
        "drain_deadline_secs": 0,
        "take_last_host": true,
        "reason": "kernel patch",
        "by": "operator ana",
        "scheduled_unix": 1_900_000_000u64,
        "hosts": [
            {"host_id": "host-a", "host_group": "web", "step": "waiting", "since_unix": 0,
             "kinds": ["macos", "linux"]},
            {"host_id": "host-b", "host_group": "web", "step": "skipped", "since_unix": 0,
             "kinds": ["linux"], "skipped_because": "no_maintenance_v1"},
            {"host_id": "host-c", "host_group": "db", "step": "skipped", "since_unix": 0,
             "skipped_because": "held"},
            {"host_id": "host-d", "step": "skipped", "since_unix": 0,
             "skipped_because": "excluded"},
            {"host_id": "host-e", "host_group": "web", "step": "not_reached",
             "since_unix": 1_900_000_050u64, "kinds": ["macos"]},
        ],
    });
    let first = sample().remove(0);
    assert_eq!(serde_json::to_value(&first).expect("serializes"), value);
    assert_eq!(serde_json::from_value::<Run>(value).expect("parses"), first);

    let second = json!({
        "id": "run-1900000050-1",
        "host_group": "web",
        "reason": "put back",
        "by": "token ci",
        "scheduled_unix": 1_900_000_050u64,
        "cancelled_unix": 1_900_000_200u64,
        "hosts": [
            {"host_id": "host-f", "host_group": "web", "step": "done",
             "since_unix": 1_900_000_150u64, "kinds": ["linux", "reboot"],
             "applied": ["linux", "reboot"], "baked_beats": 2},
            {"host_id": "host-g", "host_group": "web", "step": "halted",
             "since_unix": 1_900_000_160u64, "kinds": ["linux"], "reason": "drain failed"},
            {"host_id": "host-h", "host_group": "web", "step": "not_reached",
             "since_unix": 1_900_000_200u64, "kinds": ["linux"]},
        ],
    });
    assert_eq!(
        serde_json::to_value(&sample()[1]).expect("serializes"),
        second
    );
    assert_eq!(
        serde_json::from_value::<Run>(second).expect("parses"),
        sample()[1]
    );

    let third = &serde_json::to_value(&sample()[2]).expect("serializes")["hosts"][0];
    assert_eq!(third["offered_again"], json!(1));
    assert_eq!(third.get("baked_beats"), None, "0 is not written");
}

/// Proves: the sample round-trips through JSON exactly, and covers what the stores' tests rely
/// on: hosts sorted by id, every skip reason, an empty group, a reason, applied kinds, a batch,
/// a zero and an absent drain deadline, a cancel and a taken last host.
#[test]
fn sample_round_trips_and_covers() {
    let runs = sample();
    assert!(runs.len() >= 2);
    let text = serde_json::to_string(&runs).expect("serializes");
    assert_eq!(
        serde_json::from_str::<Vec<Run>>(&text).expect("parses"),
        runs
    );
    for run in &runs {
        let ids: Vec<&str> = run.hosts.iter().map(|host| host.host_id.as_str()).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted, "{}", run.id);
    }
    let hosts: Vec<&RunHost> = runs.iter().flat_map(|run| run.hosts.iter()).collect();
    for skip in Skip::ALL {
        assert!(
            hosts.iter().any(|host| host.skipped_because == Some(*skip)),
            "{skip:?}"
        );
    }
    assert!(hosts.iter().any(|host| host.step == Step::Waiting));
    assert!(hosts.iter().any(|host| host.step == Step::NotReached));
    assert!(hosts.iter().any(|host| host.host_group.is_empty()));
    assert!(hosts.iter().any(|host| !host.reason.is_empty()));
    assert!(hosts.iter().any(|host| !host.applied.is_empty()));
    assert!(hosts.iter().any(|host| !host.kinds.is_empty()));
    assert!(
        runs.iter()
            .any(|run| !run.host_ids.is_empty() && run.host_group.is_empty())
    );
    assert!(
        runs.iter()
            .any(|run| run.host_ids.is_empty() && !run.host_group.is_empty())
    );
    assert!(runs.iter().any(|run| run.window_end_unix != 0));
    assert!(runs.iter().any(|run| run.window_end_unix == 0));
    assert!(runs.iter().any(|run| run.batch.is_some()));
    assert!(runs.iter().any(|run| run.batch.is_none()));
    assert!(runs.iter().any(|run| run.drain_deadline_secs == Some(0)));
    assert!(runs.iter().any(|run| run.drain_deadline_secs.is_none()));
    assert!(runs.iter().any(|run| run.cancelled_unix.is_some()));
    assert!(runs.iter().any(|run| run.take_last_host));
    assert!(runs.iter().any(|run| !run.take_last_host));
    assert!(runs.iter().any(|run| run.kinds.is_empty()));
}

/// Proves: a run or a host carrying a key it does not know is refused, not read past.
#[test]
fn unknown_keys_are_refused() {
    let mut value = serde_json::to_value(&sample()[0]).expect("serializes");
    value["surprise"] = json!(1);
    let err = serde_json::from_value::<Run>(value).expect_err("an unknown key");
    assert!(
        err.to_string().contains("unknown field `surprise`"),
        "{err}"
    );

    let mut value = serde_json::to_value(&sample()[0].hosts[0]).expect("serializes");
    value["surprise"] = json!(1);
    let err = serde_json::from_value::<RunHost>(value).expect_err("an unknown key");
    assert!(
        err.to_string().contains("unknown field `surprise`"),
        "{err}"
    );
}

/// Proves: an operator's cancel is the state and ends the run at the cancel's time, whatever the
/// hosts' steps and the window.
#[test]
fn state_cancelled() {
    let mut cancelled = run("run-1-1", vec![host("host-a", "web", Step::Applying, 5)]);
    cancelled.cancelled_unix = Some(77);
    assert_eq!(cancelled.state_at(NOW), (MaintenanceState::Cancelled, 77));
    assert!(cancelled.ended_at(NOW));
}

/// Proves: a run is done when no host can start or is in its turn, at the latest time a host
/// entered its step, and at its scheduling time when it has no host.
#[test]
fn state_done() {
    let done = run(
        "run-1-1",
        vec![
            host("host-a", "web", Step::Done, 10),
            host("host-b", "web", Step::NotReached, 30),
            host("host-c", "web", Step::Skipped, 0),
        ],
    );
    assert_eq!(done.state_at(NOW), (MaintenanceState::Done, 30));
    assert!(done.ended_at(NOW));

    let empty = run("run-1-2", Vec::new());
    assert_eq!(empty.state_at(NOW), (MaintenanceState::Done, 500));
    assert!(empty.ended_at(0));
}

/// Proves: past its window with no host in its turn a run is window-ended, at the window's end;
/// at the second before it is running; with no end to the window it never ends so.
#[test]
fn state_window_ended() {
    let mut windowed = run(
        "run-1-1",
        vec![
            host("host-a", "web", Step::Waiting, 0),
            host("host-b", "web", Step::Deferred, 3),
        ],
    );
    windowed.window_end_unix = 100;
    assert_eq!(windowed.state_at(100), (MaintenanceState::WindowEnded, 100));
    assert_eq!(windowed.state_at(NOW), (MaintenanceState::WindowEnded, 100));
    assert!(windowed.ended_at(100));
    assert_eq!(windowed.state_at(99), (MaintenanceState::Running, 0));
    assert!(!windowed.ended_at(99));

    windowed.window_end_unix = 0;
    assert_eq!(windowed.state_at(NOW), (MaintenanceState::Running, 0));
}

/// Proves: a window that ended does not end a run while a host is in its turn: it is running, or
/// halted when a host halted.
#[test]
fn state_window_ended_with_a_host_in_turn() {
    let mut windowed = run(
        "run-1-1",
        vec![
            host("host-a", "web", Step::Waiting, 0),
            host("host-b", "web", Step::Applying, 3),
        ],
    );
    windowed.window_end_unix = 100;
    assert_eq!(windowed.state_at(NOW), (MaintenanceState::Running, 0));
    assert!(!windowed.ended_at(NOW));
    windowed.hosts[1].step = Step::Halted;
    assert_eq!(windowed.state_at(NOW), (MaintenanceState::Halted, 0));
    assert!(!windowed.ended_at(NOW));
}

/// Proves: a host halted or not back makes the run halted, and `halted_groups` names those
/// groups sorted and each once, leaving out a group whose host is merely in its turn.
#[test]
fn state_halted_and_groups() {
    let halted = run(
        "run-1-1",
        vec![
            host("host-a", "web", Step::Halted, 1),
            host("host-b", "db", Step::NotBack, 2),
            host("host-c", "web", Step::NotBack, 3),
            host("host-d", "zzz", Step::Applying, 4),
            host("host-e", "zzz", Step::Waiting, 0),
        ],
    );
    assert_eq!(halted.halted_groups(), ["db", "web"]);
    assert_eq!(halted.state_at(NOW), (MaintenanceState::Halted, 0));
    assert!(!halted.ended_at(NOW));

    let running = run("run-1-2", vec![host("host-a", "web", Step::Applying, 1)]);
    assert!(running.halted_groups().is_empty());
}

/// Proves: before its window opens a run is scheduled, from the start on it is running, and a
/// halted host outranks the window's start.
#[test]
fn state_scheduled_and_running() {
    let mut waiting = run("run-1-1", vec![host("host-a", "web", Step::Waiting, 0)]);
    waiting.window_start_unix = 200;
    assert_eq!(waiting.state_at(199), (MaintenanceState::Scheduled, 0));
    assert!(!waiting.ended_at(199));
    assert_eq!(waiting.state_at(200), (MaintenanceState::Running, 0));
    waiting.hosts.push(host("host-b", "web", Step::Halted, 1));
    assert_eq!(waiting.state_at(199), (MaintenanceState::Halted, 0));
}

/// Proves: a host holds a turn while it is in one, even in a run that ended, and while it waits
/// or is deferred in a run that goes on; not once the run ended, not for a host that is not in
/// it, and not at a step that ended or left it out.
#[test]
fn holds_turn_cases() {
    let mut mixed = run(
        "run-1-1",
        vec![
            host("host-a", "web", Step::Applying, 1),
            host("host-b", "web", Step::Waiting, 0),
            host("host-c", "web", Step::Deferred, 2),
            host("host-d", "web", Step::Done, 3),
            host("host-e", "web", Step::Skipped, 0),
            host("host-f", "web", Step::NotReached, 4),
        ],
    );
    assert!(mixed.holds_turn("host-a", NOW));
    assert!(mixed.holds_turn("host-b", NOW));
    assert!(mixed.holds_turn("host-c", NOW));
    for id in ["host-d", "host-e", "host-f", "host-z"] {
        assert!(!mixed.holds_turn(id, NOW), "{id}");
    }
    mixed.cancelled_unix = Some(9);
    assert!(mixed.holds_turn("host-a", NOW));
    assert!(!mixed.holds_turn("host-b", NOW));
    assert!(!mixed.holds_turn("host-c", NOW));

    let mut windowed = run("run-1-2", vec![host("host-a", "web", Step::Waiting, 0)]);
    windowed.window_end_unix = 100;
    assert!(windowed.holds_turn("host-a", 99));
    assert!(!windowed.holds_turn("host-a", 100));
}

/// Proves: cancelling stamps the run and ends each host that had not started (waiting or
/// deferred) as not reached since now, and leaves every other host exactly as it was.
#[test]
fn cancel_ends_unstarted_hosts() {
    let mut cancelled = run(
        "run-1-1",
        vec![
            host("host-a", "web", Step::Waiting, 0),
            host("host-b", "web", Step::Deferred, 3),
            host("host-c", "web", Step::Applying, 4),
            host("host-d", "web", Step::Done, 5),
            host("host-e", "web", Step::Skipped, 0),
        ],
    );
    let before = cancelled.clone();
    cancelled.cancel(1234);
    assert_eq!(cancelled.cancelled_unix, Some(1234));
    assert_eq!(
        cancelled.hosts[0],
        host("host-a", "web", Step::NotReached, 1234)
    );
    assert_eq!(
        cancelled.hosts[1],
        host("host-b", "web", Step::NotReached, 1234)
    );
    assert_eq!(cancelled.hosts[2..], before.hosts[2..]);
    assert_eq!(cancelled.state_at(NOW), (MaintenanceState::Cancelled, 1234));
}

/// Proves: at or under the cap no run is dropped, ended or not.
#[test]
fn prune_under_the_cap() {
    let mut runs: Vec<Run> = (0..MAX_RUNS).map(|n| numbered(n, true)).collect();
    let before = runs.clone();
    prune(&mut runs, NOW);
    assert_eq!(runs, before);
    let mut few = vec![numbered(1, true)];
    prune(&mut few, NOW);
    assert_eq!(few.len(), 1);
}

/// Proves: over the cap the oldest ended runs are dropped until the cap is met, in order, and a
/// run that has not ended is kept however old.
#[test]
fn prune_drops_the_oldest_ended() {
    let mut runs: Vec<Run> = (0..MAX_RUNS + 2).map(|n| numbered(n, n != 0)).collect();
    prune(&mut runs, NOW);
    assert_eq!(runs.len(), MAX_RUNS);
    assert_eq!(runs[0].id, "run-0");
    assert_eq!(runs[1].id, "run-3");
    assert_eq!(
        runs.last().expect("a run").id,
        format!("run-{}", MAX_RUNS + 1)
    );
}

/// Proves: when no run over the cap has ended, none is dropped.
#[test]
fn prune_never_drops_an_unended_run() {
    let mut runs: Vec<Run> = (0..MAX_RUNS + 3).map(|n| numbered(n, false)).collect();
    prune(&mut runs, NOW);
    assert_eq!(runs.len(), MAX_RUNS + 3);
    let mut mixed: Vec<Run> = (0..MAX_RUNS + 3).map(|n| numbered(n, n == 5)).collect();
    prune(&mut mixed, NOW);
    assert_eq!(mixed.len(), MAX_RUNS + 2);
    assert!(mixed.iter().all(|run| run.id != "run-5"));
}

/// Proves: over the cap a cancelled run that holds a halted host is kept, though it has ended
/// and is the oldest, and the next oldest ended run is dropped in its place; the halted host is
/// still in its turn after the prune.
#[test]
fn prune_keeps_an_ended_run_that_holds_a_halted_host() {
    let mut runs: Vec<Run> = (0..MAX_RUNS + 1).map(|n| numbered(n, true)).collect();
    runs[0].hosts = vec![host("host-h", "web", Step::Halted, 5)];
    assert!(runs[0].ended_at(NOW));

    prune(&mut runs, NOW);

    assert_eq!(runs.len(), MAX_RUNS);
    assert_eq!(runs[0].id, "run-0");
    assert_eq!(runs[0].hosts[0].step, Step::Halted);
    assert_eq!(runs[1].id, "run-2");
    assert!(runs.iter().all(|run| run.id != "run-1"));
}

/// Proves: the first id of a time is `run-<unix>-1`, a number a run holds is skipped to the
/// smallest free one, and the id is a valid order id.
#[test]
fn new_id_counts_from_one_and_skips_taken() {
    assert_eq!(new_id(&[], NOW), "run-1900000000-1");
    let taken = [
        run("run-1900000000-1", Vec::new()),
        run("run-1900000000-2", Vec::new()),
        run("run-1900000000-4", Vec::new()),
        run("run-1900000001-3", Vec::new()),
    ];
    let id = new_id(&taken, NOW);
    assert_eq!(id, "run-1900000000-3");
    assert!(OrderId::parse(&id).is_some());
    let longest = new_id(&[], 9_999_999_999_999);
    assert!(OrderId::parse(&longest).is_some(), "{longest}");
}

/// Proves: a host's order id is `maintain-<run id>` for its first offer and `maintain-<run id>-<n>`
/// for the n-th; the next one is the following offer's only for a deferred host; and at
/// [`MAX_OFFERED_AGAIN`], with the longest run id [`new_id`] makes, it is still a valid order id
/// (at most 64 bytes).
///
/// Catches: a host offered again under the id of its earlier offer, and an id over the wire's
/// bound that its daemon would refuse.
#[test]
fn a_hosts_order_id_names_its_offer_and_stays_an_order_id() {
    let mut turn = host("h1", "web", Step::Staging, 0);
    assert_eq!(turn.order_id("run-1-1"), "maintain-run-1-1");
    assert_eq!(turn.next_order_id("run-1-1"), "maintain-run-1-1");
    turn.step = Step::Deferred;
    assert_eq!(turn.next_order_id("run-1-1"), "maintain-run-1-1-2");
    turn.offered_again = 1;
    assert_eq!(turn.order_id("run-1-1"), "maintain-run-1-1-2");
    assert_eq!(turn.next_order_id("run-1-1"), "maintain-run-1-1-3");
    turn.step = Step::Waiting;
    assert_eq!(turn.next_order_id("run-1-1"), "maintain-run-1-1-2");

    let longest = format!("run-{}-{}", u64::MAX, u64::MAX);
    assert_eq!(longest.len(), 45);
    let id = order_id_of(&longest, MAX_OFFERED_AGAIN);
    assert_eq!(id.len(), 64);
    assert!(OrderId::parse(&id).is_some(), "{id}");
}

/// Proves: a done host bakes while its count is under its group's `bake_beats` and not once it is
/// there, and a host at any other step never bakes.
///
/// Catches: a bake that holds a place for a host that is not done, or past its count.
#[test]
fn only_a_done_host_under_its_count_bakes() {
    let mut turn = host("h1", "web", Step::Done, 0);
    turn.baked_beats = 1;
    assert!(turn.bakes(2));
    assert!(!turn.bakes(1));
    assert!(!turn.bakes(0));
    for step in Step::ALL.iter().copied().filter(|step| *step != Step::Done) {
        turn.step = step;
        assert!(!turn.bakes(2), "{step:?}");
    }
}

/// Proves: once a run's window has ended, its hosts that had not started (waiting, deferred) are
/// shown not reached since the window's end with their reason kept, and every other host as
/// held; before the end, and in a run with no end, every host is shown as held.
///
/// Catches: a window that ends with its hosts shown still waiting, and one that rewrites a host
/// that started.
#[test]
fn a_run_past_its_window_shows_its_unstarted_hosts_not_reached() {
    let mut run = run("run-1-1", Vec::new());
    for step in Step::ALL {
        let mut turn = host(step.as_str(), "web", *step, 5);
        turn.reason = "why".to_owned();
        run.hosts.push(turn);
    }
    run.window_end_unix = 100;
    for turn in &run.hosts {
        assert_eq!(
            &run.shown(turn, 99),
            turn,
            "{:?}: before the end",
            turn.step
        );
        let shown = run.shown(turn, 100);
        if turn.step.may_start() {
            assert_eq!(
                (shown.step, shown.since_unix, shown.reason.as_str()),
                (Step::NotReached, 100, "why")
            );
        } else {
            assert_eq!(&shown, turn, "{:?}", turn.step);
        }
    }
    run.window_end_unix = 0;
    for turn in &run.hosts {
        assert_eq!(&run.shown(turn, u64::MAX), turn, "no end");
    }
}
