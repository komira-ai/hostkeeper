//! From an observation to the heartbeat: the `Health` and version of a `ServiceBeat`, and the
//! reason token the daemon's status shows.

use std::path::PathBuf;
use std::time::Duration;

use hostkeeper_hook::{Step, Verdict};
use hostkeeper_observe::fake::{
    FakeExpectedExe, FakeHealthClient, FakeProcessProbe, FakeServiceManager,
};
use hostkeeper_observe::{
    ActiveState, Current, Deadline, ExeStat, HealthView, Observation, ObserveOptions, Observer,
    Ports, ServiceInput, ServiceState, UnhealthyReason, UnobservedReason, VersionView, beat_health,
};
use hostkeeper_proto::hostkeeper::agent::v1::Health;
use hostkeeper_steps::Record;
use hostkeeper_steps::record::{HookSel, Progress, SCHEMA, State};

const DEADLINE: Deadline = Deadline {
    connect: Duration::from_secs(1),
    read: Duration::from_secs(1),
};
const A: &str = "tierstore@9301.service";
const B: &str = "tierstore@9302.service";
const URL_A: &str = "http://127.0.0.1:9301/healthz";
const EXE: [u8; 32] = [3; 32];

/// The step record of an update of tierstore to 1.5.0 whose `verify` exited 0 and ended the
/// update `Succeeded`: what a hook that lies leaves behind.
fn succeeded_record() -> Record {
    Record {
        schema: SCHEMA,
        service: "tierstore".to_owned(),
        target_version: "1.5.0".to_owned(),
        artifact_digest: "a".repeat(64),
        tarball_digest: "b".repeat(64),
        previous_digest: None,
        generation: 7,
        params: PathBuf::from("params.toml"),
        step: Step::Verify,
        attempt: 1,
        state: State::Finished {
            verdict: Verdict::Succeeded {
                promotion_pending: false,
            },
            reason: None,
        },
        hook: HookSel::New,
        progress: Progress::default(),
    }
}

/// What the daemon feeds the observer for a service whose last record is `record`: the version
/// it names, and the manifest of `current`. The record's verdict has no field here to go to.
fn input(record: &Record) -> ServiceInput {
    ServiceInput {
        service: record.service.clone(),
        current: Some(Current {
            run: vec![A.to_owned(), B.to_owned()],
            health_url: Some("http://127.0.0.1:{port}/healthz".to_owned()),
        }),
        recorded_version: Some(record.target_version.clone()),
    }
}

/// A host whose run units are in `active` and whose health URL answers `status`.
fn host(active: ActiveState, status: u16) -> Observer {
    let manager = FakeServiceManager::default();
    let pid = (active == ActiveState::Active).then_some(101);
    manager.set(A, active, pid);
    manager.set(B, ActiveState::Inactive, None);
    let process = FakeProcessProbe::default();
    let stat = ExeStat {
        size: 10,
        mtime_secs: 1,
        mtime_nanos: 0,
    };
    process.set(101, 77, stat, EXE);
    let expected = FakeExpectedExe::default();
    expected.seal("tierstore", "1.5.0", EXE);
    let health = FakeHealthClient::default();
    health.answer(URL_A, status);
    let ports = Ports {
        manager: Box::new(manager),
        process: Box::new(process),
        health: Box::new(health),
        expected: Box::new(expected),
    };
    Observer::new(ports, ObserveOptions::default())
}

/// Catches a hook's verdict standing in for the service: the same `Succeeded` record gives an
/// unhealthy beat while the units are down or the URL answers 500, and a healthy one only when
/// the units run and the URL answers 200. Bake counts healthy beats, so it never advances on the
/// hook's word alone.
#[test]
fn a_hook_that_says_ok_while_the_service_is_down_never_bakes() {
    let record = succeeded_record();
    let svc = input(&record);
    let v = Some("1.5.0".to_owned());

    let down = host(ActiveState::Inactive, 200).observe(&svc, DEADLINE);
    assert_eq!(down.state, ServiceState::Stopped);
    assert_eq!(beat_health(&down), (Health::Unhealthy, v.clone()));
    assert_eq!(down.reason_token(), Some("stopped"));

    let failing = host(ActiveState::Active, 500).observe(&svc, DEADLINE);
    assert_eq!(failing.state, ServiceState::Live);
    assert_eq!(beat_health(&failing), (Health::Unhealthy, v.clone()));
    assert_eq!(failing.reason_token(), Some("status_non_2xx"));

    let serving = host(ActiveState::Active, 200).observe(&svc, DEADLINE);
    assert_eq!(beat_health(&serving), (Health::Healthy, v));
    assert_eq!(serving.reason_token(), None);
}

fn observation(state: ServiceState, version: VersionView, health: HealthView) -> Observation {
    Observation {
        service: "tierstore".to_owned(),
        state,
        version,
        health,
        recorded: Some("1.5.0".to_owned()),
    }
}

fn recorded(confirmed: bool) -> VersionView {
    VersionView::Recorded {
        version: "1.5.0".to_owned(),
        confirmed,
    }
}

fn unhealthy(reason: UnhealthyReason) -> HealthView {
    HealthView::Unhealthy { reason }
}

/// Catches any shape of observation mapped to the wrong `Health`, version or reason token.
#[test]
fn beat_health_mapping_table() {
    use HealthView::{Healthy, NotProbed};
    use ServiceState::{Absent, Live, Stopped, Transitioning, Unmonitored};
    let unobserved = ServiceState::Unobserved {
        reason: UnobservedReason::ManagerError,
        detail: "x".to_owned(),
    };
    let mismatch = VersionView::Mismatch {
        recorded: "1.5.0".to_owned(),
        observed_note: "x".to_owned(),
    };
    let not_live = VersionView::NotLive;
    let v = Some("1.5.0".to_owned());
    let cases = [
        (
            Absent,
            not_live.clone(),
            NotProbed,
            Health::Absent,
            None,
            Some("absent"),
        ),
        (
            Unmonitored,
            not_live.clone(),
            NotProbed,
            Health::Unspecified,
            v.clone(),
            Some("unmonitored"),
        ),
        (
            Stopped,
            not_live.clone(),
            NotProbed,
            Health::Unhealthy,
            v.clone(),
            Some("stopped"),
        ),
        (
            Transitioning,
            not_live.clone(),
            NotProbed,
            Health::Unhealthy,
            v.clone(),
            Some("transitioning"),
        ),
        (
            unobserved,
            not_live,
            NotProbed,
            Health::Unhealthy,
            v.clone(),
            Some("unobserved"),
        ),
        (
            Live,
            recorded(true),
            Healthy,
            Health::Healthy,
            v.clone(),
            None,
        ),
        (
            Live,
            recorded(false),
            Healthy,
            Health::Healthy,
            v.clone(),
            Some("version_unconfirmed"),
        ),
        (
            Live,
            VersionView::Unknown,
            Healthy,
            Health::Healthy,
            v.clone(),
            Some("version_unknown"),
        ),
        (
            Live,
            recorded(true),
            NotProbed,
            Health::Healthy,
            v.clone(),
            None,
        ),
        (
            Live,
            recorded(false),
            NotProbed,
            Health::Healthy,
            v.clone(),
            Some("version_unconfirmed"),
        ),
        (
            Live,
            VersionView::Unknown,
            NotProbed,
            Health::Healthy,
            v.clone(),
            Some("version_unknown"),
        ),
        (
            Live,
            mismatch.clone(),
            Healthy,
            Health::Unhealthy,
            v.clone(),
            Some("version_mismatch"),
        ),
        (
            Live,
            mismatch.clone(),
            NotProbed,
            Health::Unhealthy,
            v.clone(),
            Some("version_mismatch"),
        ),
        (
            Live,
            mismatch,
            unhealthy(UnhealthyReason::Redirect),
            Health::Unhealthy,
            v.clone(),
            Some("version_mismatch"),
        ),
        (
            Live,
            recorded(true),
            unhealthy(UnhealthyReason::StatusNon2xx),
            Health::Unhealthy,
            v.clone(),
            Some("status_non_2xx"),
        ),
        (
            Live,
            recorded(true),
            unhealthy(UnhealthyReason::Redirect),
            Health::Unhealthy,
            v.clone(),
            Some("redirect"),
        ),
        (
            Live,
            VersionView::Unknown,
            unhealthy(UnhealthyReason::Unreachable),
            Health::Unhealthy,
            v.clone(),
            Some("unreachable"),
        ),
        (
            Live,
            recorded(true),
            unhealthy(UnhealthyReason::UrlInvalid),
            Health::Unhealthy,
            v,
            Some("url_invalid"),
        ),
    ];
    for (n, (state, version, health, beat, beat_version, token)) in cases.into_iter().enumerate() {
        let seen = observation(state, version, health);
        assert_eq!(
            beat_health(&seen),
            (beat, beat_version),
            "case {n}: {seen:?}"
        );
        assert_eq!(seen.reason_token(), token, "case {n}: {seen:?}");
    }

    // No version in the step record: an empty one in the beat.
    let unrecorded = Observation {
        recorded: None,
        ..observation(Stopped, VersionView::NotLive, NotProbed)
    };
    assert_eq!(beat_health(&unrecorded), (Health::Unhealthy, None));
}

/// Catches the daemon's status losing the difference between a stopped service, one that runs
/// the wrong executable, and one the daemon could not observe: three tokens, all different.
#[test]
fn status_reports_the_reason_tokens_apart() {
    let stopped = observation(
        ServiceState::Stopped,
        VersionView::NotLive,
        HealthView::NotProbed,
    );
    let mismatch = observation(
        ServiceState::Live,
        VersionView::Mismatch {
            recorded: "1.5.0".to_owned(),
            observed_note: "x".to_owned(),
        },
        HealthView::Healthy,
    );
    let unobserved = observation(
        ServiceState::Unobserved {
            reason: UnobservedReason::ProcError,
            detail: "x".to_owned(),
        },
        VersionView::NotLive,
        HealthView::NotProbed,
    );
    let tokens: Vec<Option<&str>> = [&stopped, &mismatch, &unobserved]
        .into_iter()
        .map(Observation::reason_token)
        .collect();
    assert_eq!(
        tokens,
        [
            Some("stopped"),
            Some("version_mismatch"),
            Some("unobserved")
        ]
    );
    // All three are unhealthy in the beat: the token is what tells them apart.
    for o in [&stopped, &mismatch, &unobserved] {
        assert_eq!(beat_health(o).0, Health::Unhealthy);
    }
}
