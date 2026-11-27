//! The daemon's service observer as the heartbeat loop reads it ([`DaemonObserver`]): a service's
//! restarts, counted across observations on the daemon's monotonic clock, and the body of its
//! health URL's answer that was not 2xx. The service manager, the health URL and the clock are
//! the rig's fakes, scripted as systemd and js-agent answered on a host whose unit restarted in a
//! loop; and a Mac's, whose restarts are counted in the `runs` ring of its health state file.

mod support;

use std::fs;
use std::sync::Arc;

use hostkeeper_daemon::host_health::{BootFacts, HealthKeeper};
use hostkeeper_daemon::link_adapters::DaemonObserver;
use hostkeeper_daemon::shutdown_probe::{GoingDown, ShutdownProbe};
use hostkeeper_daemon::testing::manifest;
use hostkeeper_link::testing::{FakeClock, RecordingLog};
use hostkeeper_link::{Health, Liveness, ServiceObserver, UnixClock};
use hostkeeper_observe::{ActiveState, UnitRestarts, UnitState};
use hostkeeper_proto::hostkeeper::agent::v1::ServiceRestarts;
use support::{Rig, d, name};

const UNIT: &str = "js-agent-crun.service";
const URL: &str = "http://127.0.0.1:9301/healthz";

/// js-agent at 1.0.0, running [`UNIT`], with the health URL [`URL`], and `current` naming it.
fn installed(rig: &Rig) {
    let text = manifest("js-agent", "1.0.0")
        + &format!("\n[units]\nrun = [\"{UNIT}\"]\n\n[health]\nurl = \"{URL}\"\n");
    rig.store.add("js-agent", "1.0.0", &text);
    rig.store.seal("js-agent", "1.0.0", &d());
    rig.store.link("js-agent", "current", "1.0.0");
}

/// [`UNIT`] as systemd shows it: `active` with its main process, `count` restarts, `result`.
fn unit(active: ActiveState, count: u32, result: &str, exit_status: Option<i32>) -> UnitState {
    UnitState {
        active,
        sub: format!("{active:?}").to_lowercase(),
        main_pid: (active == ActiveState::Active).then_some(4242),
        restarts: Some(UnitRestarts {
            count,
            result: result.to_owned(),
            exit_status,
        }),
    }
}

fn restarts(
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

/// Proves a unit in a restart loop reaches the heartbeat's observation with its count and the
/// restarts of the last ten minutes of the daemon's clock: none at the first observation, the
/// rise since then while it loops, the restarts more than ten minutes old left out; a deploy's
/// restart (systemd's count back at 0) is not counted; and a unit systemd stopped at its start
/// limit is stopped and unhealthy, with `start-limit-hit` and the restarts that led to it.
///
/// Catches: a restart loop the coordinator is never told of (no restarts in the heartbeat, as
/// when js-agent-crun.service restarted 89 times and nothing was raised), the whole counter sent
/// as recent, or a deploy's restart counted as a crash.
#[test]
fn a_restart_loop_reaches_the_heartbeats_observation() {
    let rig = Rig::new("observer-restarts");
    installed(&rig);
    let observer = DaemonObserver::new(Arc::new(rig.daemon()));
    let js_agent = name("js-agent");
    let observe = |active: ActiveState, count: u32, result: &str, exit_status: Option<i32>| {
        rig.manager
            .set_state(UNIT, unit(active, count, result, exit_status));
        observer.observe(&js_agent)
    };

    let first = observe(ActiveState::Activating, 80, "exit-code", Some(1));
    assert_eq!(
        (first.liveness, first.health),
        (Liveness::Transitioning, Health::Unhealthy)
    );
    assert_eq!(first.restarts, Some(restarts(80, 0, "exit-code", Some(1))));

    rig.clock.advance(60);
    let looping = observe(ActiveState::Activating, 85, "exit-code", Some(1));
    assert_eq!(
        looping.restarts,
        Some(restarts(85, 5, "exit-code", Some(1)))
    );

    rig.clock.advance(600);
    let later = observe(ActiveState::Activating, 89, "exit-code", Some(1));
    assert_eq!(later.restarts, Some(restarts(89, 4, "exit-code", Some(1))));

    rig.clock.advance(30);
    rig.health.answer(URL, 200);
    let deployed = observe(ActiveState::Active, 0, "success", None);
    assert_eq!(
        (deployed.liveness, deployed.health),
        (Liveness::Running, Health::Healthy)
    );
    assert_eq!(deployed.restarts, Some(restarts(0, 0, "success", None)));

    rig.clock.advance(30);
    let limit = observe(ActiveState::Failed, 5, "start-limit-hit", Some(1));
    assert_eq!(
        (limit.liveness, limit.health, limit.health_detail.as_deref()),
        (Liveness::Stopped, Health::Unhealthy, Some("stopped"))
    );
    assert_eq!(
        limit.restarts,
        Some(restarts(5, 5, "start-limit-hit", Some(1)))
    );
}

/// Proves a running service whose health URL answers 503 is reported unhealthy with the body of
/// that answer, and healthy with none once it answers 200; and a service the service manager
/// counts no restarts of (a manager with no counter) has none.
///
/// Catches: js-agent's `503 not placeable: <reason>` lost between the observer and the
/// heartbeat, or restarts made up for a unit with no counter.
#[test]
fn a_503_body_reaches_the_heartbeats_observation() {
    let rig = Rig::new("observer-body");
    installed(&rig);
    let observer = DaemonObserver::new(Arc::new(rig.daemon()));
    rig.manager.set(UNIT, ActiveState::Active, Some(4242));
    rig.health
        .answer_body(URL, 503, b"503 not placeable: memory 16 GiB short\n");
    let seen = observer.observe(&name("js-agent"));
    assert_eq!(
        (
            seen.liveness,
            seen.health,
            seen.health_detail.as_deref(),
            seen.health_body.as_deref()
        ),
        (
            Liveness::Running,
            Health::Unhealthy,
            Some("status_non_2xx"),
            Some("503 not placeable: memory 16 GiB short")
        )
    );
    assert_eq!(seen.restarts, None);

    rig.health.answer(URL, 200);
    let healthy = observer.observe(&name("js-agent"));
    assert_eq!(
        (healthy.health, healthy.health_body, healthy.restarts),
        (Health::Healthy, None, None)
    );
}

/// Proves a service the daemon does not observe (no `current`) has no restarts and no body, and
/// that the window forgets the service then: observed again, its count starts from scratch.
///
/// Catches: the restarts of a service observed before reported after it could no longer be
/// observed, or counted against a count from before the gap.
#[test]
fn a_service_not_observed_has_no_restarts_and_is_forgotten() {
    let rig = Rig::new("observer-unobserved");
    installed(&rig);
    let observer = DaemonObserver::new(Arc::new(rig.daemon()));
    rig.manager.set_state(
        UNIT,
        unit(ActiveState::Activating, 10, "exit-code", Some(1)),
    );
    let js_agent = name("js-agent");
    assert_eq!(
        observer.observe(&js_agent).restarts,
        Some(restarts(10, 0, "exit-code", Some(1)))
    );
    fs::remove_file(rig.packages.join("js-agent").join("current")).expect("current removed");
    let gone = observer.observe(&js_agent);
    assert_eq!(
        (gone.liveness, gone.health_body, gone.restarts),
        (Liveness::Unobserved, None, None)
    );
    rig.store.link("js-agent", "current", "1.0.0");
    rig.manager.set_state(
        UNIT,
        unit(ActiveState::Activating, 14, "exit-code", Some(1)),
    );
    assert_eq!(
        observer.observe(&js_agent).restarts,
        Some(restarts(14, 0, "exit-code", Some(1)))
    );
}

struct Never;

impl ShutdownProbe for Never {
    fn going_down(&self) -> GoingDown {
        GoingDown::No("a test".to_owned())
    }
}

/// Proves a Mac's observer counts a service's restarts in the keeper's `runs` ring at the Unix
/// clock: the first observation counts none, a rise counts, and a new observer and keeper over
/// the same state directory (the daemon restarted) go on counting from the count the one before
/// saw; with no keeper the restarts are counted in memory, starting from none.
///
/// Catches: a Mac's job in a crash loop whose count is lost at every restart of the daemon, or a
/// keeper given and not used.
#[test]
fn a_macs_restarts_are_counted_in_the_runs_ring_across_a_restart() {
    let rig = Rig::new("observer-runs");
    installed(&rig);
    let health = rig.scratch.path().join("health");
    fs::create_dir(&health).expect("the health state directory");
    let clock = Arc::new(FakeClock::new(1_795_662_000, 0));
    let observer = |keeper: Option<Arc<HealthKeeper>>| {
        DaemonObserver::with_runs(
            Arc::new(rig.daemon()),
            keeper,
            Arc::clone(&clock) as Arc<dyn UnixClock>,
        )
    };
    let keeper = || {
        Some(Arc::new(HealthKeeper::open(
            &health,
            BootFacts {
                boot_id: Some("boot-1".to_owned()),
                boot_unix: 1_795_660_000,
                machine_id: String::new(),
            },
            Box::new(Never),
            Arc::new(RecordingLog::default()),
        )))
    };
    let js_agent = name("js-agent");
    let count = |n: u32| {
        rig.manager
            .set_state(UNIT, unit(ActiveState::Activating, n, "exit-code", Some(1)));
    };

    let first = observer(keeper());
    count(2);
    assert_eq!(
        first.observe(&js_agent).restarts,
        Some(restarts(2, 0, "exit-code", Some(1)))
    );
    clock.advance(60);
    count(5);
    assert_eq!(
        first.observe(&js_agent).restarts,
        Some(restarts(5, 3, "exit-code", Some(1)))
    );
    drop(first);

    let restarted = observer(keeper());
    clock.advance(60);
    count(7);
    assert_eq!(
        restarted.observe(&js_agent).restarts,
        Some(restarts(7, 5, "exit-code", Some(1)))
    );

    let in_memory = observer(None);
    assert_eq!(
        in_memory.observe(&js_agent).restarts,
        Some(restarts(7, 0, "exit-code", Some(1)))
    );
}
