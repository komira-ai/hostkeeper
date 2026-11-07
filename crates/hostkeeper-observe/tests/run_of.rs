//! The units an observer is given for a service whose manifest runs none
//! ([`Observer::with_run_of`]): the daemon's own service, whose job the host's installer places.
//! Against the fakes; nothing here sleeps or opens a socket.

use std::time::Duration;

use hostkeeper_observe::fake::{
    FakeExpectedExe, FakeHealthClient, FakeProcessProbe, FakeServiceManager,
};
use hostkeeper_observe::{
    ActiveState, Current, Deadline, ExeStat, HealthView, ObserveOptions, Observer, Ports,
    ServiceInput, ServiceState, UnhealthyReason, VersionView,
};

const DEADLINE: Deadline = Deadline {
    connect: Duration::from_secs(1),
    read: Duration::from_secs(1),
};
/// The daemon's own job.
const JOB: &str = "dev.hostkeeper.hostkeeper";
/// A unit a package names.
const NAMED: &str = "hostkeeper.service";
const PID: u32 = 300;
const EXE: [u8; 32] = [3; 32];
const STAT: ExeStat = ExeStat {
    size: 10,
    mtime_secs: 20,
    mtime_nanos: 30,
};

struct Rig {
    manager: FakeServiceManager,
    process: FakeProcessProbe,
    health: FakeHealthClient,
    expected: FakeExpectedExe,
}

impl Rig {
    fn new() -> Rig {
        Rig {
            manager: FakeServiceManager::default(),
            process: FakeProcessProbe::default(),
            health: FakeHealthClient::default(),
            expected: FakeExpectedExe::default(),
        }
    }

    /// An observer over the rig's fakes, given nothing.
    fn observer(&self) -> Observer {
        Observer::new(
            Ports {
                manager: Box::new(self.manager.clone()),
                process: Box::new(self.process.clone()),
                health: Box::new(self.health.clone()),
                expected: Box::new(self.expected.clone()),
            },
            ObserveOptions::default(),
        )
    }
}

/// `service` at `version`, its manifest running `run` and probing `url`.
fn input(service: &str, run: &[&str], url: Option<&str>) -> ServiceInput {
    ServiceInput {
        service: service.to_owned(),
        current: Some(Current {
            run: run.iter().map(|unit| (*unit).to_owned()).collect(),
            health_url: url.map(str::to_owned),
        }),
        recorded_version: Some("0.1.134".to_owned()),
    }
}

/// Catches a service whose manifest runs no unit left unmonitored though its units were given,
/// or observed through units given for another service: given the daemon's job, `hostkeeper`
/// with no `[units] run` is observed through it exactly as if its manifest named it (live, its
/// version confirmed by the job's process, healthy with no health URL), while another service
/// with no run unit stays unmonitored and asks the service manager nothing.
#[test]
fn a_service_given_units_is_observed_through_them_and_no_other_is() {
    let rig = Rig::new();
    rig.manager.set(JOB, ActiveState::Active, Some(PID));
    rig.process.set(PID, 9, STAT, EXE);
    rig.expected.seal("hostkeeper", "0.1.134", EXE);
    let observer = rig
        .observer()
        .with_run_of("hostkeeper", vec![JOB.to_owned()]);

    let seen = observer.observe(&input("hostkeeper", &[], None), DEADLINE);
    assert_eq!(
        (
            seen.state.clone(),
            seen.version.clone(),
            seen.health.clone()
        ),
        (
            ServiceState::Live,
            VersionView::Recorded {
                version: "0.1.134".to_owned(),
                confirmed: true,
            },
            HealthView::NotProbed
        )
    );
    assert_eq!(seen.reason_token(), None);
    assert_eq!(rig.manager.calls(), [JOB, JOB]);

    let other = observer.observe(&input("web", &[], None), DEADLINE);
    assert_eq!(other.state, ServiceState::Unmonitored);
    assert_eq!(rig.manager.calls(), [JOB, JOB]);
}

/// Catches units given put in place of the units a manifest names, an empty list read as units,
/// and a health URL of a given unit not probed: a manifest that names a unit is observed through
/// it; given no units, a service with no run unit is unmonitored; given again, the last units
/// stand; and the health URL of a manifest with no run unit is probed once per given unit that
/// is active.
#[test]
fn a_manifest_that_names_units_wins_and_the_last_units_given_stand() {
    let rig = Rig::new();
    rig.manager.set(NAMED, ActiveState::Inactive, None);
    rig.manager.set(JOB, ActiveState::Active, Some(PID));
    rig.process.set(PID, 9, STAT, EXE);
    let observer = rig
        .observer()
        .with_run_of("hostkeeper", vec![JOB.to_owned()]);
    let named = observer.observe(&input("hostkeeper", &[NAMED], None), DEADLINE);
    assert_eq!(named.state, ServiceState::Stopped);
    assert_eq!(rig.manager.calls(), [NAMED, NAMED]);

    let none = rig.observer().with_run_of("hostkeeper", Vec::new());
    let seen = none.observe(&input("hostkeeper", &[], None), DEADLINE);
    assert_eq!(seen.state, ServiceState::Unmonitored);
    assert_eq!(rig.manager.calls(), [NAMED, NAMED]);

    let url = "http://127.0.0.1:9400/healthz";
    rig.health.answer(url, 503);
    let again = rig
        .observer()
        .with_run_of("hostkeeper", vec![NAMED.to_owned()])
        .with_run_of("hostkeeper", vec![JOB.to_owned()]);
    let seen = again.observe(&input("hostkeeper", &[], Some(url)), DEADLINE);
    assert_eq!(
        (seen.state.clone(), seen.health.clone()),
        (
            ServiceState::Live,
            HealthView::Unhealthy {
                reason: UnhealthyReason::StatusNon2xx
            }
        )
    );
    assert_eq!(rig.health.requests().len(), 1);
}
