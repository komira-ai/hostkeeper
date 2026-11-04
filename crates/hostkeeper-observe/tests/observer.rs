//! The observer against the fakes: the states, the failure rule, the aggregation of a run set,
//! the version check and its cache, and the health probes. Nothing here sleeps or opens a socket.

use std::path::Path;
use std::time::Duration;

use hostkeeper_config::manifest::{Expected, HostkeeperVersion, Manifest};
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_observe::fake::{
    FakeExpectedExe, FakeHealthClient, FakeProcessProbe, FakeServiceManager,
};
use hostkeeper_observe::{
    ActiveState, Current, Deadline, ExeStat, HealthReply, HealthView, Observation, ObserveError,
    ObserveOptions, Observer, Ports, ProcessIdentity, ServiceInput, ServiceState, UnhealthyReason,
    UnitState, UnobservedReason, VersionView,
};
use hostkeeper_testkit::Scratch;

const DEADLINE: Deadline = Deadline {
    connect: Duration::from_secs(2),
    read: Duration::from_secs(3),
};
const A: &str = "tierstore@9301.service";
const B: &str = "tierstore@9302.service";
const URL_A: &str = "http://127.0.0.1:9301/healthz";
const URL_B: &str = "http://127.0.0.1:9302/healthz";
const PID: u32 = 101;
const EXE: [u8; 32] = [7; 32];
const WRAPPER: [u8; 32] = [9; 32];
const STAT: ExeStat = ExeStat {
    size: 50_000_000,
    mtime_secs: 1000,
    mtime_nanos: 5,
};

/// An observer over fakes, and a clone of each fake to script and read.
struct Rig {
    manager: FakeServiceManager,
    process: FakeProcessProbe,
    health: FakeHealthClient,
    expected: FakeExpectedExe,
    observer: Observer,
}

fn rig_with(options: ObserveOptions) -> Rig {
    let (manager, process) = (FakeServiceManager::default(), FakeProcessProbe::default());
    let (health, expected) = (FakeHealthClient::default(), FakeExpectedExe::default());
    let ports = Ports {
        manager: Box::new(manager.clone()),
        process: Box::new(process.clone()),
        health: Box::new(health.clone()),
        expected: Box::new(expected.clone()),
    };
    let observer = Observer::new(ports, options);
    Rig {
        manager,
        process,
        health,
        expected,
        observer,
    }
}

fn rig() -> Rig {
    rig_with(ObserveOptions::default())
}

impl Rig {
    /// tierstore 1.5.0 as on a host mid-life: `@9301` serving as pid 101 with the sealed
    /// executable and a healthy URL, `@9302` a standby that is inactive.
    fn serving(self) -> Rig {
        self.manager.set(A, ActiveState::Active, Some(PID));
        self.manager.set(B, ActiveState::Inactive, None);
        self.process.set(PID, 5000, STAT, EXE);
        self.expected.seal("tierstore", "1.5.0", EXE);
        self.health.answer(URL_A, 200);
        self
    }

    fn observe(&self, svc: &ServiceInput) -> Observation {
        self.observer.observe(svc, DEADLINE)
    }
}

/// tierstore with the run set and health URL of the design's example, at `recorded`.
fn tierstore(recorded: Option<&str>) -> ServiceInput {
    with_current(&[A, B], Some("http://127.0.0.1:{port}/healthz"), recorded)
}

fn with_current(run: &[&str], url: Option<&str>, recorded: Option<&str>) -> ServiceInput {
    ServiceInput {
        service: "tierstore".to_owned(),
        current: Some(Current {
            run: run.iter().map(|unit| (*unit).to_owned()).collect(),
            health_url: url.map(str::to_owned),
        }),
        recorded_version: recorded.map(str::to_owned),
    }
}

fn unit(active: ActiveState, main_pid: Option<u32>) -> UnitState {
    UnitState {
        active,
        sub: "running".to_owned(),
        main_pid,
    }
}

fn confirmed(version: &str) -> VersionView {
    VersionView::Recorded {
        version: version.to_owned(),
        confirmed: true,
    }
}

fn unconfirmed(version: &str) -> VersionView {
    VersionView::Recorded {
        version: version.to_owned(),
        confirmed: false,
    }
}

fn unobserved(reason: UnobservedReason, detail: &str) -> ServiceState {
    ServiceState::Unobserved {
        reason,
        detail: detail.to_owned(),
    }
}

fn process_error(pid: u32, why: &str) -> ObserveError {
    ObserveError::Process {
        pid,
        why: why.to_owned(),
    }
}

/// Catches a state read as another: one case each, with the token the daemon's status shows.
#[test]
fn state_covers_absent_stopped_transitioning_unobserved_unmonitored_and_live() {
    let no_current = ServiceInput {
        current: None,
        ..tierstore(Some("1.5.0"))
    };
    let cases: Vec<(&str, Rig, ServiceInput, ServiceState)> = vec![
        ("no current", rig(), no_current, ServiceState::Absent),
        (
            "no unit known to the manager",
            rig(),
            tierstore(Some("1.5.0")),
            ServiceState::Absent,
        ),
        (
            "no run units",
            rig(),
            with_current(&[], Some(URL_A), Some("1.5.0")),
            ServiceState::Unmonitored,
        ),
        (
            "one unit active",
            rig().serving(),
            tierstore(Some("1.5.0")),
            ServiceState::Live,
        ),
    ];
    for (name, rig, svc, want) in cases {
        let seen = rig.observe(&svc);
        assert_eq!(seen.state, want, "{name}");
    }

    let stopped = rig();
    stopped.manager.set(A, ActiveState::Inactive, None);
    stopped.manager.set(B, ActiveState::Failed, None);
    assert_eq!(
        stopped.observe(&tierstore(Some("1.5.0"))).state,
        ServiceState::Stopped
    );

    // One unit gone from the manager and one stopped is stopped, not absent.
    let half = rig();
    half.manager.set(B, ActiveState::Inactive, None);
    assert_eq!(half.observe(&tierstore(None)).state, ServiceState::Stopped);

    for (a, b) in [
        (ActiveState::Activating, ActiveState::Inactive),
        (ActiveState::Active, ActiveState::Deactivating),
    ] {
        let moving = rig();
        moving.manager.set(A, a, Some(PID));
        moving.manager.set(B, b, None);
        let seen = moving.observe(&tierstore(Some("1.5.0")));
        assert_eq!(seen.state, ServiceState::Transitioning, "{a:?} {b:?}");
        assert_eq!(seen.health, HealthView::NotProbed);
        assert!(moving.health.requests().is_empty());
    }

    let blind = rig();
    blind.manager.fail(A);
    assert_eq!(
        blind.observe(&tierstore(Some("1.5.0"))).state,
        unobserved(
            UnobservedReason::ManagerError,
            "the service manager cannot tell the state of tierstore@9301.service: failed on command"
        )
    );

    let tokens: Vec<&str> = [
        ServiceState::Absent,
        ServiceState::Stopped,
        ServiceState::Transitioning,
        unobserved(UnobservedReason::ProcError, "x"),
        ServiceState::Unmonitored,
        ServiceState::Live,
    ]
    .iter()
    .map(ServiceState::token)
    .collect();
    assert_eq!(
        tokens,
        [
            "absent",
            "stopped",
            "transitioning",
            "unobserved",
            "unmonitored",
            "live"
        ]
    );
}

/// Catches nothing being asked of a service with no current and of one with no run units, and a
/// whole round being one read before and one after.
#[test]
fn the_unit_set_is_read_before_and_after_and_not_at_all_when_there_is_nothing_to_read() {
    let r = rig();
    let no_current = ServiceInput {
        current: None,
        ..tierstore(None)
    };
    r.observe(&no_current);
    r.observe(&with_current(&[], None, None));
    assert!(r.manager.calls().is_empty());
    r.observe(&tierstore(None));
    assert_eq!(r.manager.calls(), [A, B, A, B]);
}

/// Catches a failure of any port read as absent, stopped or unhealthy: each fails in turn, and
/// every round is `unobserved` with the reason of the port that failed.
#[test]
fn an_observation_failure_is_never_read_as_absent_or_stopped() {
    let svc = tierstore(Some("1.5.0"));
    let manager_fails = |r: &Rig| r.manager.fail(B);
    let manager_fails_after = |r: &Rig| {
        r.manager.then(A, Ok(unit(ActiveState::Active, Some(PID))));
        r.manager.then(
            A,
            Err(ObserveError::Manager {
                unit: A.to_owned(),
                why: "timed out".to_owned(),
            }),
        );
    };
    let identity_fails = |r: &Rig| {
        r.process
            .then_identity(PID, Err(process_error(PID, "gone")))
    };
    let identity_fails_after = |r: &Rig| {
        r.process.then_identity(
            PID,
            Ok(ProcessIdentity {
                pid: PID,
                start_time: 5000,
            }),
        );
        r.process
            .then_identity(PID, Err(process_error(PID, "gone")));
    };
    let stat_fails = |r: &Rig| r.process.set_stat(PID, Err(process_error(PID, "no exe")));
    let hash_fails = |r: &Rig| {
        r.process
            .set_digest(PID, Err(process_error(PID, "unreadable")))
    };
    let probe_fails = |r: &Rig| {
        r.health.then(
            URL_A,
            Err(ObserveError::Probe {
                url: URL_A.to_owned(),
                why: "too many open files".to_owned(),
            }),
        );
    };
    // The unit stops while its URL is asked: the URL does not answer, but the unit is no longer
    // active, so the round cannot tell a stop from a failed probe.
    let health_transport_on_a_stopping_unit = |r: &Rig| {
        r.manager.then(A, Ok(unit(ActiveState::Active, Some(PID))));
        r.manager.set(A, ActiveState::Inactive, None);
        r.health.then(
            URL_A,
            Err(ObserveError::Unreachable {
                url: URL_A.to_owned(),
                why: "refused".to_owned(),
            }),
        );
    };
    type Break = Box<dyn Fn(&Rig)>;
    let cases: Vec<(Break, ServiceState)> = vec![
        (
            Box::new(manager_fails),
            unobserved(
                UnobservedReason::ManagerError,
                "the service manager cannot tell the state of tierstore@9302.service: failed on command",
            ),
        ),
        (
            Box::new(manager_fails_after),
            unobserved(
                UnobservedReason::ManagerError,
                "the service manager cannot tell the state of tierstore@9301.service: timed out",
            ),
        ),
        (
            Box::new(identity_fails),
            unobserved(
                UnobservedReason::ProcError,
                "process 101 cannot be read: gone",
            ),
        ),
        (
            Box::new(identity_fails_after),
            unobserved(
                UnobservedReason::ProcError,
                "process 101 cannot be read: gone",
            ),
        ),
        (
            Box::new(stat_fails),
            unobserved(
                UnobservedReason::ProcError,
                "process 101 cannot be read: no exe",
            ),
        ),
        (
            Box::new(hash_fails),
            unobserved(
                UnobservedReason::ProcError,
                "process 101 cannot be read: unreadable",
            ),
        ),
        (
            Box::new(probe_fails),
            unobserved(
                UnobservedReason::HealthError,
                "tierstore: the probe of http://127.0.0.1:9301/healthz could not be made: too many open files",
            ),
        ),
        (
            Box::new(health_transport_on_a_stopping_unit),
            unobserved(
                UnobservedReason::ChangedDuringRound,
                "tierstore@9301.service changed while it was observed: Active pid Some(101), then Inactive pid None",
            ),
        ),
    ];
    for (n, (break_it, want)) in cases.into_iter().enumerate() {
        let r = rig().serving();
        break_it(&r);
        let seen = r.observe(&svc);
        assert_eq!(seen.state, want, "case {n}");
        assert_eq!(seen.version, VersionView::NotLive, "case {n}");
        assert_eq!(seen.health, HealthView::NotProbed, "case {n}");
        assert_eq!(seen.reason_token(), Some("unobserved"), "case {n}");
    }
}

/// Catches a process the fake was never told of being read as anything but an error, for the
/// stat and the digest alike: an unknown pid is a failure of the probe, so `unobserved`.
#[test]
fn a_process_with_no_executable_to_read_is_unobserved() {
    let identity = |start_time| {
        Ok(ProcessIdentity {
            pid: 202,
            start_time,
        })
    };
    let no_stat = rig().serving();
    no_stat.manager.set(A, ActiveState::Active, Some(202));
    no_stat.process.then_identity(202, identity(1));
    assert_eq!(
        no_stat.observe(&tierstore(Some("1.5.0"))).state,
        unobserved(
            UnobservedReason::ProcError,
            "process 202 cannot be read: no such process"
        )
    );

    let no_digest = rig().serving();
    no_digest.manager.set(A, ActiveState::Active, Some(202));
    no_digest.process.then_identity(202, identity(1));
    no_digest.process.set_stat(202, Ok(STAT));
    assert_eq!(
        no_digest.observe(&tierstore(Some("1.5.0"))).state,
        unobserved(
            UnobservedReason::ProcError,
            "process 202 cannot be read: no such process"
        )
    );
    assert_eq!(no_digest.process.digest_calls(), 1);

    let no_identity = rig().serving();
    no_identity.manager.set(A, ActiveState::Active, Some(303));
    assert_eq!(
        no_identity.observe(&tierstore(Some("1.5.0"))).state,
        unobserved(
            UnobservedReason::ProcError,
            "process 303 cannot be read: no such process"
        )
    );
}

/// Catches an aggregation that needs every unit active: tierstore keeps one instance as a
/// standby during a hand-over, inactive by design.
#[test]
fn a_standby_instance_that_is_inactive_is_not_a_stop() {
    let r = rig().serving();
    let seen = r.observe(&tierstore(Some("1.5.0")));
    assert_eq!(
        seen,
        Observation {
            service: "tierstore".to_owned(),
            state: ServiceState::Live,
            version: confirmed("1.5.0"),
            health: HealthView::Healthy,
            recorded: Some("1.5.0".to_owned()),
        }
    );
    assert_eq!(seen.reason_token(), None);
    // The standby is not probed.
    assert_eq!(r.health.requests(), [URL_A]);
}

/// Catches a unit with no main process being hashed or guessed: its version is unknown.
#[test]
fn a_oneshot_remain_after_exit_unit_is_live_with_version_unknown() {
    let r = rig().serving();
    r.manager
        .set("tierstore-setup.service", ActiveState::Active, None);
    let svc = with_current(&["tierstore-setup.service"], None, Some("1.5.0"));
    let seen = r.observe(&svc);
    assert_eq!(seen.state, ServiceState::Live);
    assert_eq!(seen.version, VersionView::Unknown);
    assert_eq!(seen.health, HealthView::NotProbed);
    assert_eq!(seen.reason_token(), Some("version_unknown"));
    assert_eq!(r.process.digest_calls(), 0);
}

/// Catches a version taken from whichever seal the digest matches: two staged versions sealed
/// with the same executable still report the recorded one, and a digest that matches another
/// version's seal and not the recorded one's is a mismatch of the recorded version.
#[test]
fn version_comes_from_the_record_and_the_hash_only_confirms_it() {
    let r = rig().serving();
    r.expected.seal("tierstore", "1.4.2", EXE);
    assert_eq!(
        r.observe(&tierstore(Some("1.5.0"))).version,
        confirmed("1.5.0")
    );
    assert_eq!(
        r.observe(&tierstore(Some("1.4.2"))).version,
        confirmed("1.4.2")
    );

    let other = rig().serving();
    other.expected.seal("tierstore", "1.4.2", EXE);
    other.expected.seal("tierstore", "1.5.0", WRAPPER);
    let seen = other.observe(&tierstore(Some("1.5.0")));
    assert!(
        matches!(&seen.version, VersionView::Mismatch { recorded, .. } if recorded == "1.5.0"),
        "{seen:?}"
    );

    // No version in the step record: live, version unknown, nothing hashed.
    let none = rig().serving();
    assert_eq!(none.observe(&tierstore(None)).version, VersionView::Unknown);
    // No seal for the recorded version: the recorded version, unconfirmed, nothing hashed.
    assert_eq!(
        none.observe(&tierstore(Some("1.6.0"))).version,
        unconfirmed("1.6.0")
    );
    assert_eq!(none.process.digest_calls(), 0);
}

/// Catches a wrapper binary (one that is not the release's executable) passing for the release.
#[test]
fn a_wrapper_process_reads_as_version_mismatch() {
    let r = rig().serving();
    r.process.set_digest(PID, Ok(WRAPPER));
    let seen = r.observe(&tierstore(Some("1.5.0")));
    assert_eq!(seen.state, ServiceState::Live);
    assert_eq!(
        seen.version,
        VersionView::Mismatch {
            recorded: "1.5.0".to_owned(),
            observed_note: format!(
                "the running executable's SHA-256 is {}, not the sealed {}",
                "09".repeat(32),
                "07".repeat(32)
            ),
        }
    );
    assert_eq!(seen.reason_token(), Some("version_mismatch"));
}

/// Catches a digest taken across a restart (or of a reused pid) being believed: the version part
/// of the round is dropped, not reported as a mismatch, and nothing is cached from it.
#[test]
fn pid_reuse_or_restart_between_the_two_reads_discards_the_round() {
    let r = rig().serving();
    r.process.set_digest(PID, Ok(WRAPPER));
    for start_time in [5000, 6000] {
        r.process.then_identity(
            PID,
            Ok(ProcessIdentity {
                pid: PID,
                start_time,
            }),
        );
    }
    let seen = r.observe(&tierstore(Some("1.5.0")));
    assert_eq!(seen.state, ServiceState::Live);
    assert_eq!(seen.version, VersionView::Unknown);
    assert_eq!(seen.health, HealthView::Healthy);
    assert_eq!(r.process.digest_calls(), 1);
    // The process is identified on both sides of the hash, not twice before it.
    assert_eq!(
        r.process.calls(),
        ["identity", "exe_stat", "exe_digest", "identity"]
    );
    // The next round hashes again: the discarded digest was not cached.
    r.process.set_digest(PID, Ok(EXE));
    assert_eq!(
        r.observe(&tierstore(Some("1.5.0"))).version,
        confirmed("1.5.0")
    );
    assert_eq!(r.process.digest_calls(), 2);
}

/// Catches a round believed across a change of the unit set: a unit that starts, or whose main
/// pid changes, between the two reads.
#[test]
fn a_unit_set_that_changes_between_reads_is_unobserved() {
    let starts = rig().serving();
    starts
        .manager
        .then(B, Ok(unit(ActiveState::Inactive, None)));
    starts.manager.set(B, ActiveState::Active, Some(102));
    assert_eq!(
        starts.observe(&tierstore(Some("1.5.0"))).state,
        unobserved(
            UnobservedReason::ChangedDuringRound,
            "tierstore@9302.service changed while it was observed: Inactive pid None, then Active pid Some(102)"
        )
    );

    let restarts = rig().serving();
    restarts
        .manager
        .then(A, Ok(unit(ActiveState::Active, Some(PID))));
    restarts.manager.set(A, ActiveState::Active, Some(103));
    assert_eq!(
        restarts.observe(&tierstore(Some("1.5.0"))).state,
        unobserved(
            UnobservedReason::ChangedDuringRound,
            "tierstore@9301.service changed while it was observed: Active pid Some(101), then Active pid Some(103)"
        )
    );

    // A stopped service that starts between the reads is not reported stopped either.
    let stopped = rig();
    stopped
        .manager
        .then(A, Ok(unit(ActiveState::Inactive, None)));
    stopped.manager.set(A, ActiveState::Activating, Some(PID));
    stopped.manager.set(B, ActiveState::Inactive, None);
    assert!(matches!(
        stopped.observe(&tierstore(None)).state,
        ServiceState::Unobserved {
            reason: UnobservedReason::ChangedDuringRound,
            ..
        }
    ));
}

/// Catches the executable being hashed every round: a process whose pid, start time, size and
/// modification time are unchanged is not hashed again; a changed modification time is.
#[test]
fn an_unchanged_process_is_not_rehashed() {
    let r = rig().serving();
    let svc = tierstore(Some("1.5.0"));
    assert_eq!(r.observe(&svc).version, confirmed("1.5.0"));
    assert_eq!(r.observe(&svc).version, confirmed("1.5.0"));
    assert_eq!(r.process.digest_calls(), 1);
    // Each part of the key on its own: a new start time (a restart under the same pid), size,
    // or modification time in seconds or nanoseconds hashes again.
    let changes = [
        (6000, STAT.size, STAT.mtime_secs, STAT.mtime_nanos),
        (6000, STAT.size + 1, STAT.mtime_secs, STAT.mtime_nanos),
        (6000, STAT.size + 1, STAT.mtime_secs, STAT.mtime_nanos + 1),
    ];
    for (n, (start_time, size, mtime_secs, mtime_nanos)) in changes.into_iter().enumerate() {
        let stat = ExeStat {
            size,
            mtime_secs,
            mtime_nanos,
        };
        r.process.set(PID, start_time, stat, EXE);
        assert_eq!(r.observe(&svc).version, confirmed("1.5.0"), "change {n}");
        assert_eq!(r.process.digest_calls(), n + 2, "change {n}");
    }
    r.process.set_stat(
        PID,
        Ok(ExeStat {
            mtime_secs: 2000,
            size: STAT.size + 1,
            mtime_nanos: STAT.mtime_nanos + 1,
        }),
    );
    r.process.set_digest(PID, Ok(WRAPPER));
    assert!(matches!(
        r.observe(&svc).version,
        VersionView::Mismatch { .. }
    ));
    assert_eq!(r.process.digest_calls(), 5);
    // Another service's entry is its own: tierstore's cached digest is not used for it.
    let other = ServiceInput {
        service: "tiercache".to_owned(),
        ..svc
    };
    r.expected.seal("tiercache", "1.5.0", WRAPPER);
    assert_eq!(r.observe(&other).version, confirmed("1.5.0"));
    assert_eq!(r.process.digest_calls(), 6);
}

/// Catches the switch doing nothing: with the check off nothing is hashed, and the recorded
/// version is reported unconfirmed.
#[test]
fn the_exe_hash_check_can_be_switched_off() {
    assert!(ObserveOptions::default().confirm_exe_hash);
    let r = rig_with(ObserveOptions {
        confirm_exe_hash: false,
    })
    .serving();
    let seen = r.observe(&tierstore(Some("1.5.0")));
    assert_eq!(seen.version, unconfirmed("1.5.0"));
    assert_eq!(seen.reason_token(), Some("version_unconfirmed"));
    assert_eq!(r.process.digest_calls(), 0);
}

/// Catches a status outside 2xx read as healthy, or a 2xx read as unhealthy.
#[test]
fn only_a_2xx_is_healthy() {
    let cases = [
        (
            199,
            HealthView::Unhealthy {
                reason: UnhealthyReason::StatusNon2xx,
            },
        ),
        (200, HealthView::Healthy),
        (204, HealthView::Healthy),
        (299, HealthView::Healthy),
        (
            300,
            HealthView::Unhealthy {
                reason: UnhealthyReason::Redirect,
            },
        ),
        (
            399,
            HealthView::Unhealthy {
                reason: UnhealthyReason::Redirect,
            },
        ),
        (
            404,
            HealthView::Unhealthy {
                reason: UnhealthyReason::StatusNon2xx,
            },
        ),
        (
            500,
            HealthView::Unhealthy {
                reason: UnhealthyReason::StatusNon2xx,
            },
        ),
    ];
    for (status, want) in cases {
        let r = rig().serving();
        r.health.answer(URL_A, status);
        assert_eq!(
            r.observe(&tierstore(Some("1.5.0"))).health,
            want,
            "{status}"
        );
    }
}

/// Catches `{port}` filled from anything but the number after the instance's `@`, and an
/// instance with no such number being probed at all.
#[test]
fn port_placeholder_fills_from_the_number_after_at() {
    let r = rig().serving();
    r.observe(&tierstore(Some("1.5.0")));
    assert_eq!(r.health.requests(), [URL_A]);

    for unit in [
        "tierstore@blue.service",
        "tierstore.service",
        "tierstore@0.service",
        "tierstore@70000.service",
        "tierstore@+93.service",
        "tierstore@9303",
        "tiercache@9303.service",
    ] {
        let bad = rig();
        bad.manager.set(unit, ActiveState::Active, None);
        let seen = bad.observe(&with_current(
            &[unit],
            Some("http://127.0.0.1:{port}/healthz"),
            Some("1.5.0"),
        ));
        assert_eq!(
            seen.health,
            HealthView::Unhealthy {
                reason: UnhealthyReason::UrlInvalid
            },
            "{unit}"
        );
        assert_eq!(seen.reason_token(), Some("url_invalid"), "{unit}");
        assert!(bad.health.requests().is_empty(), "{unit}");
    }

    // A URL the probe refuses (a name, not a loopback literal) is invalid, and not asked.
    let named = rig().serving();
    let svc = with_current(&[A], Some("http://localhost:{port}/healthz"), Some("1.5.0"));
    assert_eq!(
        named.observe(&svc).health,
        HealthView::Unhealthy {
            reason: UnhealthyReason::UrlInvalid
        }
    );
    assert!(named.health.requests().is_empty());
}

/// Catches one probe standing for the whole run set, a failing instance hidden by a passing one,
/// or a URL with no placeholder asked once for several instances.
#[test]
fn one_probe_per_active_instance_and_all_must_pass() {
    let both = |a: u16, b: Option<u16>| {
        let r = rig().serving();
        r.manager.set(B, ActiveState::Active, Some(102));
        r.health.answer(URL_A, a);
        b.into_iter()
            .for_each(|status| r.health.answer(URL_B, status));
        let seen = r.observe(&tierstore(Some("1.5.0")));
        assert_eq!(r.health.requests(), [URL_A, URL_B]);
        seen.health
    };
    let unhealthy = |reason| HealthView::Unhealthy { reason };
    assert_eq!(both(200, Some(204)), HealthView::Healthy);
    assert_eq!(
        both(200, Some(500)),
        unhealthy(UnhealthyReason::StatusNon2xx)
    );
    assert_eq!(
        both(500, Some(200)),
        unhealthy(UnhealthyReason::StatusNon2xx)
    );
    // The first failure in run order is the one reported.
    assert_eq!(both(302, Some(500)), unhealthy(UnhealthyReason::Redirect));
    // An active instance whose URL does not answer: up and not answering.
    assert_eq!(both(200, None), unhealthy(UnhealthyReason::Unreachable));

    let fixed = rig().serving();
    fixed.manager.set(B, ActiveState::Active, Some(102));
    fixed
        .health
        .answer("http://127.0.0.1:8080/ready?deep=1", 200);
    let svc = with_current(
        &[A, B],
        Some("http://127.0.0.1:8080/ready?deep=1"),
        Some("1.5.0"),
    );
    assert_eq!(fixed.observe(&svc).health, HealthView::Healthy);
    assert_eq!(fixed.health.requests().len(), 2);

    // A one-off answer comes before the steady one.
    let once = rig().serving();
    once.health.then(
        URL_A,
        Ok(HealthReply {
            status: 503,
            body_bytes_read: 0,
        }),
    );
    let svc = tierstore(Some("1.5.0"));
    assert_eq!(
        once.observe(&svc).health,
        unhealthy(UnhealthyReason::StatusNon2xx)
    );
    assert_eq!(once.observe(&svc).health, HealthView::Healthy);
}

/// Writes a tierstore 1.5.0 version directory with `manifest` and its unit template.
fn package(dir: &Path, manifest: &str) {
    for (path, text) in [
        ("manifest.toml", manifest),
        ("bin/tierstore", "#!/bin/false\n"),
        ("units/tierstore@.service", "[Service]\n"),
    ] {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }
}

const MANIFEST_HEAD: &str = r#"schema         = 1
name           = "tierstore"
version        = "1.5.0"
min_hostkeeper = "0.4.0"

[hook]
command = ["bin/tierstore", "update-hook"]
verify_deadline_secs = 1600
[hook.timeout_secs]
prepare = 0
cutover = 300
verify = 1000
rollback = 600
cleanup = 1300
"#;

fn load(dir: &Path) -> Manifest {
    let expected = Expected {
        name: ServiceName::new("tierstore").expect("a name"),
        version: Version::new("1.5.0").expect("a version"),
    };
    let running = HostkeeperVersion {
        major: 0,
        minor: 4,
        patch: 0,
    };
    Manifest::load(dir, &expected, &running).expect("the manifest loads")
}

/// Catches a manifest without `[health]`, or without `[units]`, failing to load or to observe:
/// both shapes shipped before health was observed and stay valid.
#[test]
fn old_step_records_and_manifests_without_health_still_observe() {
    let base = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let no_health = Scratch::under(base, "observe-no-health").expect("scratch");
    package(
        no_health.path(),
        &format!("{MANIFEST_HEAD}\n[units]\nrun = [\"{A}\"]\n"),
    );
    let current = Current::from_manifest(&load(no_health.path()));
    assert_eq!(
        current,
        Current {
            run: vec![A.to_owned()],
            health_url: None,
        }
    );
    let r = rig().serving();
    let svc = ServiceInput {
        current: Some(current),
        ..tierstore(Some("1.5.0"))
    };
    let seen = r.observe(&svc);
    assert_eq!(
        (seen.state, seen.version, seen.health),
        (
            ServiceState::Live,
            confirmed("1.5.0"),
            HealthView::NotProbed
        )
    );
    assert!(r.health.requests().is_empty());

    let no_units = Scratch::under(base, "observe-no-units").expect("scratch");
    package(no_units.path(), MANIFEST_HEAD);
    let current = Current::from_manifest(&load(no_units.path()));
    assert!(current.run.is_empty());
    let svc = ServiceInput {
        current: Some(current),
        ..tierstore(Some("1.5.0"))
    };
    let seen = r.observe(&svc);
    assert_eq!(seen.state, ServiceState::Unmonitored);
    assert_eq!(seen.reason_token(), Some("unmonitored"));
}

/// Catches a reason token that changes spelling: the daemon's status shows these.
#[test]
fn reason_tokens_are_stable() {
    let unobserved: Vec<&str> = [
        UnobservedReason::ManagerError,
        UnobservedReason::ProcError,
        UnobservedReason::HealthError,
        UnobservedReason::ChangedDuringRound,
    ]
    .into_iter()
    .map(UnobservedReason::token)
    .collect();
    assert_eq!(
        unobserved,
        [
            "manager_error",
            "proc_error",
            "health_error",
            "changed_during_round"
        ]
    );
    let unhealthy: Vec<&str> = [
        UnhealthyReason::StatusNon2xx,
        UnhealthyReason::Redirect,
        UnhealthyReason::Unreachable,
        UnhealthyReason::UrlInvalid,
    ]
    .into_iter()
    .map(UnhealthyReason::token)
    .collect();
    assert_eq!(
        unhealthy,
        ["status_non_2xx", "redirect", "unreachable", "url_invalid"]
    );
}
