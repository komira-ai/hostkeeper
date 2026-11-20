//! A host's turn of a maintenance run through `Daemon::order`: the steps it launches and in what
//! order, how each is reported while it runs, how the hook's steps and each service's `drain` and
//! `resume` are launched (unit, bound, params), and the run record the turn ends with, all on the
//! rig's scripted launcher.

mod support;
mod support_maintain;

use std::num::NonZeroU64;
use std::sync::{Arc, Mutex};

use hostkeeper_daemon::Reply;
use hostkeeper_daemon::maintenance_state::run_state;
use hostkeeper_hook::{Params, Verdict};
use hostkeeper_proto::hostkeeper::agent::v1::{MaintenanceKind, MaintenanceStep};
use hostkeeper_steps::Bound;
use hostkeeper_steps::maintenance::{
    Kind, KindProgress, MAINTENANCE_SCHEMA, MaintenanceLoaded, MaintenanceRecord, MaintenanceStore,
    RESUME_BOUND_SECS, RunStep,
};
use support::{Rig, name};
use support_maintain::{
    HOOK, ORDER, RUN, draining, launched, maintain, order_of, params_of, plain, record, terms,
};

/// The reply of a turn that ran to its end, done.
fn succeeded() -> Reply {
    Reply::Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
        resent: false,
    }
}

/// The wire names of the kinds the params file `text` (proto3 JSON) names, sorted.
fn kinds_of(text: &str) -> Vec<String> {
    let json: serde_json::Value = serde_json::from_str(text).expect("the params are JSON");
    let mut kinds: Vec<String> = json["kinds"]
        .as_array()
        .expect("the params name kinds")
        .iter()
        .map(|kind| kind.as_str().expect("a kind name").to_owned())
        .collect();
    kinds.sort();
    kinds
}

fn secs(n: u64) -> Bound {
    Bound::Secs(NonZeroU64::new(n).expect("a non-zero bound"))
}

/// Catches a turn that launches its steps in another order, puts services back in the order they
/// were drained, or ends with a record that is not done: the hook's `prepare`, each draining
/// service's `drain` by name, the `cutover` of the one kind, `verify`, then each `resume` in
/// reverse; the reply is a finished turn (not resent), the record is done with nothing drained,
/// the kind applied and no step running, and the heartbeat's run state of it says done with the
/// kind applied.
#[test]
fn a_turn_launches_its_steps_in_order_and_ends_done() {
    let rig = Rig::new("maintain-run-order");
    draining(&rig, "alpha", "1.0.0");
    draining(&rig, "beta", "1.0.0");
    plain(&rig, "gamma", "1.0.0");
    let daemon = rig.daemon();
    let reply = daemon.order(&maintain(&[MaintenanceKind::Linux]));
    assert_eq!(reply, succeeded());
    assert_eq!(
        launched(&rig),
        [
            "hostkeeper prepare",
            "alpha drain",
            "beta drain",
            "hostkeeper cutover",
            "hostkeeper verify",
            "beta resume",
            "alpha resume",
        ]
    );
    let expected = MaintenanceRecord {
        schema: MAINTENANCE_SCHEMA,
        run_id: RUN.to_owned(),
        order_id: ORDER.to_owned(),
        order_try: 1,
        kinds: vec![KindProgress {
            kind: Kind::Linux,
            applied: true,
            reboot_under_way: None,
        }],
        services: vec![name("alpha"), name("beta")],
        drained: Vec::new(),
        step: RunStep::Done,
        running: false,
        deferred: None,
        halted: None,
    };
    let held = record(&rig).expect("the run record");
    assert_eq!(held, expected);
    let state = run_state(&held);
    assert_eq!(state.run_id, RUN);
    assert_eq!(state.order_id, ORDER);
    assert_eq!(state.step, i32::from(MaintenanceStep::Done));
    assert_eq!(state.applied, [i32::from(MaintenanceKind::Linux)]);
    assert_eq!(state.halted_reason, "");
}

/// Catches a turn whose record does not say, while a step runs, which step it is: the run state
/// read from the record on disk inside each launch is staging for `prepare`, draining for each
/// `drain`, applying for `cutover`, checking for `verify` and putting back for each `resume`.
#[test]
fn each_step_is_reported_while_it_runs() {
    let rig = Rig::new("maintain-run-reported");
    draining(&rig, "alpha", "1.0.0");
    draining(&rig, "beta", "1.0.0");
    let seen: Arc<Mutex<Vec<i32>>> = Arc::default();
    let sink = Arc::clone(&seen);
    let state = rig.state.clone();
    rig.launcher.during_additive(move |_| {
        let step = match MaintenanceStore::beside(&state).load() {
            Ok(MaintenanceLoaded::Current(record)) => run_state(&record).step,
            _ => -1,
        };
        sink.lock().expect("the sink").push(step);
    });
    let daemon = rig.daemon();
    assert_eq!(
        daemon.order(&maintain(&[MaintenanceKind::Linux])),
        succeeded()
    );
    let steps = seen.lock().expect("the sink").clone();
    assert_eq!(
        steps,
        [
            MaintenanceStep::Staging,
            MaintenanceStep::Draining,
            MaintenanceStep::Draining,
            MaintenanceStep::Applying,
            MaintenanceStep::Checking,
            MaintenanceStep::PuttingBack,
            MaintenanceStep::PuttingBack,
        ]
        .map(i32::from)
    );
}

/// Catches a package that names neither `drain` nor `resume` being drained or put back: `gamma`
/// is launched nothing, has no params file, and is not among the record's services.
#[test]
fn a_package_naming_no_drain_is_not_touched() {
    let rig = Rig::new("maintain-run-plain");
    draining(&rig, "alpha", "1.0.0");
    plain(&rig, "gamma", "1.0.0");
    let daemon = rig.daemon();
    assert_eq!(
        daemon.order(&maintain(&[MaintenanceKind::Linux])),
        succeeded()
    );
    assert_eq!(
        launched(&rig),
        [
            "hostkeeper prepare",
            "alpha drain",
            "hostkeeper cutover",
            "hostkeeper verify",
            "alpha resume",
        ]
    );
    assert!(
        rig.launcher
            .additive_runs()
            .iter()
            .all(|ran| !ran.unit.contains("gamma"))
    );
    let held = record(&rig).expect("the run record");
    assert_eq!(held.services, [name("alpha")]);
    let dir = MaintenanceStore::beside(&rig.state).dir().to_path_buf();
    assert!(dir.join("alpha.params").exists());
    assert!(!dir.join("gamma.params").exists());
}

/// Catches a run of macOS and Xcode that applies Xcode first or gives one `cutover` both kinds:
/// the order names `XCODE, MACOS`; the `cutover` launches' params name `MACOS` alone, then
/// `XCODE` alone, while `prepare`'s and `verify`'s name both.
#[test]
fn macos_is_applied_before_xcode_with_the_params_of_one_kind_each() {
    let rig = Rig::new("maintain-run-kinds");
    let daemon = rig.daemon();
    let order = maintain(&[MaintenanceKind::Xcode, MaintenanceKind::Macos]);
    assert_eq!(daemon.order(&order), succeeded());
    assert_eq!(
        launched(&rig),
        [
            "hostkeeper prepare",
            "hostkeeper cutover",
            "hostkeeper cutover",
            "hostkeeper verify",
        ]
    );
    let both = vec![
        MaintenanceKind::Macos.as_str_name().to_owned(),
        MaintenanceKind::Xcode.as_str_name().to_owned(),
    ];
    assert_eq!(kinds_of(&params_of(&rig, 0)), both);
    assert_eq!(
        kinds_of(&params_of(&rig, 1)),
        [MaintenanceKind::Macos.as_str_name()]
    );
    assert_eq!(
        kinds_of(&params_of(&rig, 2)),
        [MaintenanceKind::Xcode.as_str_name()]
    );
    assert_eq!(kinds_of(&params_of(&rig, 3)), both);
    let held = record(&rig).expect("the run record");
    let kinds: Vec<(Kind, bool)> = held.kinds.iter().map(|k| (k.kind, k.applied)).collect();
    assert_eq!(kinds, [(Kind::Macos, true), (Kind::Xcode, true)]);
}

/// Catches a hook step launched as the wrong program, unit or bound: the hook's steps are this
/// binary with `maintenance-hook` before the step, units `updater-hostkeeper-<step>-1`, no bound;
/// a `drain` without a deadline is unbounded and a `resume` has the resume bound; their units are
/// `updater-alpha-<step>-1`, and the drain's params file names the service and the run and no
/// deadline.
#[test]
fn the_steps_are_launched_as_the_hook_and_the_services_hooks() {
    let rig = Rig::new("maintain-run-launch");
    draining(&rig, "alpha", "1.0.0");
    let daemon = rig.daemon();
    assert_eq!(
        daemon.order(&maintain(&[MaintenanceKind::Linux])),
        succeeded()
    );
    let ran = rig.launcher.additive_runs();
    assert_eq!(ran.len(), 5);
    let exe = std::env::current_exe().expect("this binary's path");
    for (at, step) in [(0, "prepare"), (2, "cutover"), (3, "verify")] {
        assert_eq!(ran[at].service, HOOK);
        assert_eq!(ran[at].step, step);
        assert_eq!(ran[at].unit, format!("updater-hostkeeper-{step}-1"));
        assert_eq!(ran[at].executable, exe);
        assert_eq!(ran[at].hook_args, ["maintenance-hook"]);
        assert_eq!(ran[at].bound, Bound::Unbounded);
    }
    assert_eq!(ran[1].service, "alpha");
    assert_eq!(ran[1].step, "drain");
    assert_eq!(ran[1].unit, "updater-alpha-drain-1");
    assert_eq!(ran[1].bound, Bound::Unbounded);
    assert_eq!(ran[4].service, "alpha");
    assert_eq!(ran[4].step, "resume");
    assert_eq!(ran[4].unit, "updater-alpha-resume-1");
    assert_eq!(ran[4].bound, Bound::Secs(RESUME_BOUND_SECS));
    let drain = Params::parse(&ran[1].params).expect("the drain's params");
    assert_eq!(drain.service(), Some("alpha"));
    assert_eq!(drain.run_id(), Some(RUN));
    assert_eq!(drain.drain_deadline_unix(), None);
    assert!(!ran[1].params.contains("drain_deadline_unix"));
}

/// Catches a drain deadline that is not the drain's bound or not told to the hook in unix
/// seconds: with a deadline of 300 and the unix clock at 1000000 the drain is bounded by 300
/// seconds and its params hold the deadline 1000300; the `resume` keeps its own bound.
#[test]
fn a_drain_deadline_bounds_the_drain_and_is_told_in_unix_seconds() {
    let rig = Rig::new("maintain-run-deadline");
    draining(&rig, "alpha", "1.0.0");
    let daemon = rig
        .daemon()
        .with_unix_clock(Arc::new(hostkeeper_link::testing::FakeClock::new(
            1_000_000, 5,
        )));
    let order = order_of(ORDER, 1, terms(&[MaintenanceKind::Linux], Some(300)));
    assert_eq!(daemon.order(&order), succeeded());
    let ran = rig.launcher.additive_runs();
    assert_eq!(ran[1].step, "drain");
    assert_eq!(ran[1].bound, secs(300));
    let drain = Params::parse(&ran[1].params).expect("the drain's params");
    assert_eq!(drain.drain_deadline_unix(), Some(1_000_300));
    assert_eq!(ran[4].step, "resume");
    assert_eq!(ran[4].bound, Bound::Secs(RESUME_BOUND_SECS));
}

/// Catches a deadline of 0 that gives the drain no bound or a zero bound: the drain is bounded by
/// one second and its params hold the deadline at the clock's second.
#[test]
fn a_drain_deadline_of_zero_bounds_the_drain_by_one_second() {
    let rig = Rig::new("maintain-run-deadline-zero");
    draining(&rig, "alpha", "1.0.0");
    let daemon = rig
        .daemon()
        .with_unix_clock(Arc::new(hostkeeper_link::testing::FakeClock::new(
            1_000_000, 5,
        )));
    let order = order_of(ORDER, 1, terms(&[MaintenanceKind::Linux], Some(0)));
    assert_eq!(daemon.order(&order), succeeded());
    let ran = rig.launcher.additive_runs();
    assert_eq!(ran[1].step, "drain");
    assert_eq!(ran[1].bound, secs(1));
    let drain = Params::parse(&ran[1].params).expect("the drain's params");
    assert_eq!(drain.drain_deadline_unix(), Some(1_000_000));
}

/// Catches units that ignore the order's try: an order of try 3 names every unit, the hook's and
/// the services', with `-3`, and the record holds try 3.
#[test]
fn the_units_of_a_turn_carry_the_orders_try() {
    let rig = Rig::new("maintain-run-try");
    draining(&rig, "alpha", "1.0.0");
    let daemon = rig.daemon();
    let order = order_of(ORDER, 3, terms(&[MaintenanceKind::Linux], None));
    assert_eq!(daemon.order(&order), succeeded());
    let units: Vec<String> = rig
        .launcher
        .additive_runs()
        .iter()
        .map(|ran| ran.unit.clone())
        .collect();
    assert_eq!(
        units,
        [
            "updater-hostkeeper-prepare-3",
            "updater-alpha-drain-3",
            "updater-hostkeeper-cutover-3",
            "updater-hostkeeper-verify-3",
            "updater-alpha-resume-3",
        ]
    );
    assert_eq!(record(&rig).expect("the run record").order_try, 3);
}
