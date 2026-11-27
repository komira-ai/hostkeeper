//! A unit's restarts, from systemd's properties to the heartbeat's `ServiceRestarts`: the systemd
//! adapter over recorded `systemctl show` answers (a unit in a crash loop, one at
//! `start-limit-hit`, a healthy one, and the answers it cannot read), the observer carrying the
//! counts of each run unit even in a discarded round, and the window that counts the recent ones.
//! Nothing here sleeps, reads a clock or runs a program.

use std::time::Duration;

use hostkeeper_observe::fake::{
    FakeCommandRunner, FakeExpectedExe, FakeHealthClient, FakeProcessProbe, FakeServiceManager,
};
use hostkeeper_observe::restarts::{MAX_SAMPLES, RECENT_SECS};
use hostkeeper_observe::{
    ActiveState, CommandOutput, Current, Deadline, ObserveError, ObserveOptions, Observer, Ports,
    RestartWindow, RunUnitRestarts, ServiceInput, ServiceManager, ServiceState, SystemdManager,
    UnitRestarts, UnitState, UnobservedReason,
};
use hostkeeper_proto::health::MAX_LAST_RESULT;
use hostkeeper_proto::hostkeeper::agent::v1::ServiceRestarts;

const UNIT: &str = "js-agent-crun.service";
const A: &str = "tierstore@9301.service";
const B: &str = "tierstore@9302.service";
const DEADLINE: Deadline = Deadline {
    connect: Duration::from_secs(1),
    read: Duration::from_secs(1),
};

/// What the systemd adapter reads of `stdout`, printed by `systemctl show` for [`UNIT`].
fn shown(stdout: &str) -> Result<UnitState, ObserveError> {
    let runner = FakeCommandRunner::default();
    runner.then(Ok(CommandOutput {
        exit_code: Some(0),
        stdout: stdout.as_bytes().to_vec(),
        truncated: false,
    }));
    SystemdManager::new(runner, Duration::from_secs(5)).unit_state(UNIT)
}

fn restarts(count: u32, result: &str, exit_status: Option<i32>) -> UnitRestarts {
    UnitRestarts {
        count,
        result: result.to_owned(),
        exit_status,
    }
}

fn state(
    active: ActiveState,
    sub: &str,
    main_pid: Option<u32>,
    restarts: Option<UnitRestarts>,
) -> UnitState {
    UnitState {
        active,
        sub: sub.to_owned(),
        main_pid,
        restarts,
    }
}

/// Proves the adapter reads a unit's restarts from recorded `systemctl show` answers: a unit in a
/// crash loop, waiting to be started again, has its count, `exit-code` and the exit code 1; a unit
/// systemd stopped at its start limit says `start-limit-hit`; a healthy unit has a count of 0,
/// `success` and no exit code, since its main process has not exited.
///
/// Catches: `NRestarts`, `Result` or the exit code not read, read from the wrong property, or a
/// unit at its start limit reported as merely failed.
#[test]
fn systemd_reads_the_restarts_of_a_crash_loop_a_start_limit_and_a_healthy_unit() {
    let crash_loop = "Result=exit-code\nNRestarts=89\nExecMainCode=1\nExecMainStatus=1\n\
        MainPID=0\nLoadState=loaded\nActiveState=activating\nSubState=auto-restart\n";
    assert_eq!(
        shown(crash_loop),
        Ok(state(
            ActiveState::Activating,
            "auto-restart",
            None,
            Some(restarts(89, "exit-code", Some(1)))
        ))
    );
    let start_limit = "Result=start-limit-hit\nNRestarts=5\nExecMainCode=1\nExecMainStatus=1\n\
        MainPID=0\nLoadState=loaded\nActiveState=failed\nSubState=failed\n";
    assert_eq!(
        shown(start_limit),
        Ok(state(
            ActiveState::Failed,
            "failed",
            None,
            Some(restarts(5, "start-limit-hit", Some(1)))
        ))
    );
    let healthy = "Result=success\nNRestarts=0\nExecMainCode=0\nExecMainStatus=0\n\
        MainPID=4242\nLoadState=loaded\nActiveState=active\nSubState=running\n";
    assert_eq!(
        shown(healthy),
        Ok(state(
            ActiveState::Active,
            "running",
            Some(4242),
            Some(restarts(0, "success", None))
        ))
    );
}

/// Proves the boundary cases of the restart properties: a main process a signal ended (code 2)
/// or that dumped core (code 3) has no exit code; a unit systemd does not know, a systemd that
/// prints no `NRestarts`, and a unit type with none have no restarts; a unit with a count and no
/// `Result` has an empty result; and the exit code may be negative or 255.
///
/// Catches: a signal number reported as an exit code, a default count of 0 made up where systemd
/// gave none, or a not-found unit's `NRestarts=0` reported as restarts.
#[test]
fn systemd_restarts_boundaries() {
    let base = "LoadState=loaded\nActiveState=failed\nSubState=failed\nMainPID=0\n";
    let read = |extra: &str| shown(&format!("{base}{extra}")).map(|unit| unit.restarts);
    let cases = [
        (
            "NRestarts=3\nResult=signal\nExecMainCode=2\nExecMainStatus=9\n",
            Some(restarts(3, "signal", None)),
        ),
        (
            "NRestarts=3\nResult=core-dump\nExecMainCode=3\nExecMainStatus=11\n",
            Some(restarts(3, "core-dump", None)),
        ),
        (
            "NRestarts=4294967295\nResult=exit-code\nExecMainCode=1\nExecMainStatus=255\n",
            Some(restarts(u32::MAX, "exit-code", Some(255))),
        ),
        (
            "NRestarts=1\nResult=exit-code\nExecMainCode=1\nExecMainStatus=-2\n",
            Some(restarts(1, "exit-code", Some(-2))),
        ),
        ("NRestarts=2\n", Some(restarts(2, "", None))),
        ("Result=exit-code\nExecMainCode=1\nExecMainStatus=1\n", None),
        ("", None),
    ];
    for (extra, want) in cases {
        assert_eq!(read(extra), Ok(want), "{extra:?}");
    }
    let not_found = "LoadState=not-found\nActiveState=inactive\nSubState=dead\nMainPID=0\n\
        NRestarts=0\nResult=success\nExecMainCode=0\nExecMainStatus=0\n";
    assert_eq!(
        shown(not_found),
        Ok(state(ActiveState::NotFound, "dead", None, None))
    );
}

/// Proves a restart property systemd printed and this adapter cannot read is an error naming
/// it, never a count: an `NRestarts` that is not a number or is past `u32`, and, with
/// `ExecMainCode=1`, an `ExecMainStatus` that is missing or not a number.
///
/// Catches: an unreadable count read as 0 or as no restarts, which would hide a restart loop.
#[test]
fn systemd_restart_properties_it_cannot_read_are_errors() {
    let base = "LoadState=loaded\nActiveState=active\nSubState=running\nMainPID=7\n";
    let failed = |why: &str| -> Result<UnitState, ObserveError> {
        Err(ObserveError::Manager {
            unit: UNIT.to_owned(),
            why: why.to_owned(),
        })
    };
    let cases = [
        ("NRestarts=-1\n", "NRestarts \"-1\" is not a number"),
        (
            "NRestarts=4294967296\n",
            "NRestarts \"4294967296\" is not a number",
        ),
        (
            "NRestarts=1\nExecMainCode=1\n",
            "ExecMainStatus \"\" is not a number",
        ),
        (
            "NRestarts=1\nExecMainCode=1\nExecMainStatus=x\n",
            "ExecMainStatus \"x\" is not a number",
        ),
    ];
    for (extra, why) in cases {
        assert_eq!(shown(&format!("{base}{extra}")), failed(why), "{extra:?}");
    }
}

/// An observer over a scripted service manager, with no process, health URL or seal.
fn observer() -> (FakeServiceManager, Observer) {
    let manager = FakeServiceManager::default();
    let ports = Ports {
        manager: Box::new(manager.clone()),
        process: Box::new(FakeProcessProbe::default()),
        health: Box::new(FakeHealthClient::default()),
        expected: Box::new(FakeExpectedExe::default()),
    };
    (manager, Observer::new(ports, ObserveOptions::default()))
}

fn tierstore(run: &[&str]) -> ServiceInput {
    ServiceInput {
        service: "tierstore".to_owned(),
        current: Some(Current {
            run: run.iter().map(|unit| (*unit).to_owned()).collect(),
            health_url: None,
        }),
        recorded_version: None,
    }
}

fn of(unit: &str, restarts: UnitRestarts) -> RunUnitRestarts {
    RunUnitRestarts {
        unit: unit.to_owned(),
        restarts,
    }
}

/// Proves an observation carries the restarts of every run unit the manager counts, in run
/// order, and none of a unit it counts none of; and a stopped service (every unit failed at its
/// start limit) carries them as a live one does.
///
/// Catches: the restarts of the second instance lost, units reordered, or the counts dropped
/// when the service is not live.
#[test]
fn an_observation_carries_each_run_units_restarts_in_run_order() {
    let (manager, observer) = observer();
    let counted = |n| Some(restarts(n, "success", None));
    manager.set_state(
        B,
        state(ActiveState::Active, "running", Some(9), counted(2)),
    );
    manager.set_state(
        A,
        state(ActiveState::Active, "running", Some(8), counted(7)),
    );
    manager.set("tierstore-sidecar.service", ActiveState::Active, Some(10));
    let seen = observer.observe(&tierstore(&[B, "tierstore-sidecar.service", A]), DEADLINE);
    assert_eq!(seen.state, ServiceState::Live);
    assert_eq!(
        seen.restarts,
        [
            of(B, restarts(2, "success", None)),
            of(A, restarts(7, "success", None))
        ]
    );

    let limit = Some(restarts(5, "start-limit-hit", Some(1)));
    manager.set_state(A, state(ActiveState::Failed, "failed", None, limit.clone()));
    manager.set_state(B, state(ActiveState::Failed, "failed", None, limit));
    let stopped = observer.observe(&tierstore(&[A, B]), DEADLINE);
    assert_eq!(stopped.state, ServiceState::Stopped);
    assert_eq!(stopped.reason_token(), Some("stopped"));
    assert_eq!(
        stopped.restarts,
        [
            of(A, restarts(5, "start-limit-hit", Some(1))),
            of(B, restarts(5, "start-limit-hit", Some(1)))
        ]
    );
}

/// Proves a round discarded after a read of the unit set keeps that read's restarts: a unit
/// that restarts while it is observed (active as pid 8, then waiting to start again, one restart
/// more) gives an `unobserved` round with the later count; a manager that fails on the second
/// read keeps the first read's; one that fails on the first read gives none.
///
/// Catches: the counts of exactly the unit in a restart loop dropped, because its round is the
/// one that keeps being discarded.
#[test]
fn a_discarded_round_keeps_the_restarts_of_its_last_read() {
    let (manager, observer) = observer();
    let unit_at =
        |active, pid, n| state(active, "sub", pid, Some(restarts(n, "exit-code", Some(1))));
    manager.then(A, Ok(unit_at(ActiveState::Active, Some(8), 88)));
    manager.set_state(A, unit_at(ActiveState::Activating, None, 89));
    let seen = observer.observe(&tierstore(&[A]), DEADLINE);
    assert!(
        matches!(
            seen.state,
            ServiceState::Unobserved {
                reason: UnobservedReason::ChangedDuringRound,
                ..
            }
        ),
        "{seen:?}"
    );
    assert_eq!(seen.restarts, [of(A, restarts(89, "exit-code", Some(1)))]);

    let failure = ObserveError::Manager {
        unit: A.to_owned(),
        why: "systemctl show exited with code 1".to_owned(),
    };
    manager.then(A, Ok(unit_at(ActiveState::Active, Some(8), 90)));
    manager.then(A, Err(failure.clone()));
    let seen = observer.observe(&tierstore(&[A]), DEADLINE);
    assert!(
        matches!(
            seen.state,
            ServiceState::Unobserved {
                reason: UnobservedReason::ManagerError,
                ..
            }
        ),
        "{seen:?}"
    );
    assert_eq!(seen.restarts, [of(A, restarts(90, "exit-code", Some(1)))]);

    manager.then(A, Err(failure));
    let seen = observer.observe(&tierstore(&[A]), DEADLINE);
    assert!(seen.restarts.is_empty(), "{seen:?}");
}

/// Proves a service with no unit to read has no restarts: one with no `current`, and one whose
/// manifest runs no unit.
///
/// Catches: restarts made up for a service whose units the manager was never asked about.
#[test]
fn a_service_with_no_unit_read_has_no_restarts() {
    let (manager, observer) = observer();
    let absent = ServiceInput {
        current: None,
        ..tierstore(&[A])
    };
    assert!(observer.observe(&absent, DEADLINE).restarts.is_empty());
    assert!(
        observer
            .observe(&tierstore(&[]), DEADLINE)
            .restarts
            .is_empty()
    );
    assert_eq!(manager.calls(), Vec::<String>::new());
}

fn restarted(
    count: u32,
    recent: u32,
    last_result: &str,
    exit_status: Option<i32>,
) -> ServiceRestarts {
    ServiceRestarts {
        count,
        recent,
        last_result: last_result.to_owned(),
        exit_status,
    }
}

/// `unit`'s count `count`, with a result of `exit-code`.
fn looping(unit: &str, count: u32) -> Vec<RunUnitRestarts> {
    vec![of(unit, restarts(count, "exit-code", Some(1)))]
}

/// Proves `recent` counts the restarts of the last ten minutes: none at a unit's first
/// observation (its count's restarts may be older), the rise since its oldest observation in the
/// window after that, an observation exactly 600 seconds old still in the window and one 601
/// seconds old out of it.
///
/// Catches: the whole counter sent as recent (the 89 restarts of a loop that ended an hour ago
/// raising a crash-loop alert), or restarts older than ten minutes counted.
#[test]
fn recent_counts_the_rise_within_ten_minutes() {
    assert_eq!(RECENT_SECS, 600);
    let mut window = RestartWindow::new();
    let mut at = |now: u64, count: u32| {
        window
            .record("js-agent", &looping(UNIT, count), now)
            .map(|seen| (seen.count, seen.recent))
    };
    assert_eq!(at(1000, 80), Some((80, 0)));
    assert_eq!(at(1060, 85), Some((85, 5)));
    assert_eq!(at(1600, 88), Some((88, 8)));
    assert_eq!(at(1601, 89), Some((89, 4)));
    assert_eq!(at(1660, 89), Some((89, 4)));
    assert_eq!(at(1661, 89), Some((89, 1)));
    assert_eq!(at(3000, 89), Some((89, 0)));
}

/// Proves a count that fell, as systemd's does when a deploy restarts the unit by hand, starts
/// the unit's window again, so the deploy's restart and the loop before it are not counted, and
/// restarts after it are; and an observation at a time before the last starts it again too.
///
/// Catches: a deploy's restart counted as a crash, or a fallen count giving a recent count that
/// wraps around.
#[test]
fn a_fallen_count_or_an_earlier_time_starts_the_window_again() {
    let mut window = RestartWindow::new();
    let mut at = |now: u64, count: u32| {
        window
            .record("js-agent", &looping(UNIT, count), now)
            .map(|seen| seen.recent)
    };
    assert_eq!(at(1000, 10), Some(0));
    assert_eq!(at(1010, 12), Some(2));
    assert_eq!(at(1020, 0), Some(0));
    assert_eq!(at(1030, 3), Some(3));
    assert_eq!(at(900, 4), Some(0));
    assert_eq!(at(910, 6), Some(2));
}

/// Proves the window keeps at most [`MAX_SAMPLES`] observations of a unit, dropping the oldest:
/// 257 observations a second apart, the count rising by one each, give a recent count of 255.
///
/// Catches: a window that grows without bound while a daemon observes a unit often.
#[test]
fn the_window_keeps_at_most_256_observations_of_a_unit() {
    assert_eq!(MAX_SAMPLES, 256);
    let mut window = RestartWindow::new();
    let mut last = None;
    for n in 0..=256u32 {
        last = window.record("js-agent", &looping(UNIT, n), 1000 + u64::from(n));
    }
    assert_eq!(last.map(|seen| (seen.count, seen.recent)), Some((256, 255)));
}

/// Proves the service's restarts sum its units' counts and recent counts, and take the result
/// and exit code of the first unit, in run order, whose result is not `success` or empty, else
/// of the first unit; a result past 32 bytes is cut; and the sums saturate.
///
/// Catches: the restarts of one instance standing for the service's, a healthy instance's
/// `success` hiding another's `start-limit-hit`, or a sum that wraps.
#[test]
fn a_services_restarts_sum_its_units() {
    let mut window = RestartWindow::new();
    let both = |a: UnitRestarts, b: UnitRestarts| vec![of(A, a), of(B, b)];
    assert_eq!(
        window.record(
            "tierstore",
            &both(restarts(3, "success", None), restarts(4, "", None)),
            1000
        ),
        Some(restarted(7, 0, "success", None))
    );
    assert_eq!(
        window.record(
            "tierstore",
            &both(
                restarts(5, "success", None),
                restarts(9, "start-limit-hit", Some(2))
            ),
            1010
        ),
        Some(restarted(14, 7, "start-limit-hit", Some(2)))
    );
    assert_eq!(
        window.record(
            "tierstore",
            &both(restarts(5, "", None), restarts(9, "", None)),
            1020
        ),
        Some(restarted(14, 7, "", None))
    );

    let long = "r".repeat(MAX_LAST_RESULT + 8);
    assert_eq!(
        window.record("odd", &[of(A, restarts(1, &long, None))], 1000),
        Some(restarted(1, 0, &"r".repeat(MAX_LAST_RESULT), None))
    );

    let mut huge = RestartWindow::new();
    huge.record(
        "big",
        &both(restarts(0, "", None), restarts(0, "", None)),
        1000,
    );
    assert_eq!(
        huge.record(
            "big",
            &both(restarts(u32::MAX, "", None), restarts(u32::MAX, "", None)),
            1010
        ),
        Some(restarted(u32::MAX, u32::MAX, "", None))
    );
}

/// Proves the window forgets a service observed with no counts (its restarts are none) and a
/// unit that left the run set, so either counts from scratch when it comes back; and that two
/// services with a unit of the same name are counted apart.
///
/// Catches: a unit's old count read against a new one after it was gone, or one service's
/// restarts counted against another's.
#[test]
fn the_window_forgets_what_it_no_longer_sees() {
    let mut window = RestartWindow::new();
    let recent = |seen: Option<ServiceRestarts>| seen.map(|seen| seen.recent);
    assert_eq!(recent(window.record("web", &looping(A, 5), 1000)), Some(0));
    assert_eq!(recent(window.record("db", &looping(A, 50), 1000)), Some(0));
    assert_eq!(window.record("web", &[], 1010), None);
    assert_eq!(recent(window.record("web", &looping(A, 9), 1020)), Some(0));
    assert_eq!(recent(window.record("db", &looping(A, 52), 1020)), Some(2));

    assert_eq!(recent(window.record("db", &looping(B, 1), 1030)), Some(0));
    assert_eq!(recent(window.record("db", &looping(A, 60), 1040)), Some(0));
}
