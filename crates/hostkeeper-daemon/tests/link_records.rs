//! The daemon as the installed services and the service observer of the heartbeat loop
//! (`hostkeeper_link`): the loop lists what `current` names, with the target of an unfinished
//! step record, and reports each service as the daemon's observer saw it.

mod support;

use std::fs;
use std::sync::Arc;

use hostkeeper_config::names::Version;
use hostkeeper_daemon::link_adapters::{DaemonObserver, DaemonRecords, NOT_OBSERVED, observation};
use hostkeeper_daemon::testing::spawn;
use hostkeeper_hook::Step;
use hostkeeper_link::{
    Health, Liveness, Observation, RecordedService, ServiceObserver, ServiceRecords,
};
use hostkeeper_observe::{
    HealthView, ServiceState, UnhealthyReason, UnobservedReason, VersionView,
};
use hostkeeper_steps::record::RECORD_FILE;
use support::force::succeeded;
use support::{Rig, d, install, name, update};

fn version(text: &str) -> Version {
    Version::new(text).expect("a version")
}

/// `service` listed at `at`, updating to `updating`, with no step record's try.
fn listed(service: &str, at: &str, updating: Option<&str>) -> RecordedService {
    tried(service, at, updating, None)
}

/// `service` listed at `at`, updating to `updating`, its step record at the try `record_try`.
fn tried(
    service: &str,
    at: &str,
    updating: Option<&str>,
    record_try: Option<u64>,
) -> RecordedService {
    RecordedService {
        name: name(service),
        version: version(at),
        updating: updating.map(version),
        last_result: None,
        record_try,
    }
}

/// Catches a list of installed services read from somewhere else than `current`: a service whose
/// `current` names a version is listed at it, by name; a package with no `current`, a file and a
/// directory whose name is not a service name are not; a service with no step record updates
/// nothing.
#[test]
fn the_installed_services_are_those_whose_current_names_a_version() {
    let rig = Rig::new("records-current");
    rig.running("web", "1.0.0");
    rig.running("api", "3.1.0");
    rig.stage("db", "2.0.0");
    fs::write(rig.packages.join("stray"), "a file").expect("a file");
    fs::create_dir(rig.packages.join(".hidden")).expect("a directory of no service");
    let records = DaemonRecords::new(Arc::new(rig.daemon()));
    assert_eq!(
        records.installed().expect("the packages directory is read"),
        [listed("api", "3.1.0", None), listed("web", "1.0.0", None)]
    );
}

/// Catches a heartbeat that does not say an update runs, or that keeps saying it after it ended:
/// while an update of `web` to 2.0.0 is held in `verify`, the step record is unfinished and `web`
/// is listed updating to 2.0.0 (at the version `current` names by then); once it ended it is
/// listed at 2.0.0 updating nothing. Both times the listing carries the record's try, 1.
#[test]
fn an_unfinished_step_record_is_listed_as_updating_to_its_target() {
    let rig = Rig::new("records-updating");
    rig.add("web", "2.0.0");
    let daemon = Arc::new(rig.daemon());
    let records = DaemonRecords::new(Arc::clone(&daemon));
    assert_eq!(
        records.installed().expect("read"),
        [listed("web", "0.0.0", None)]
    );
    let gate = rig.launcher.hold("web", Step::Verify);
    let running = Arc::clone(&daemon);
    let order = spawn("the order", move || {
        running.order(&update("web", "2.0.0", &d(), 1))
    });
    gate.entered(&order).expect("the held step is entered");
    assert_eq!(
        records.installed().expect("read"),
        [tried("web", "2.0.0", Some("2.0.0"), Some(1))]
    );
    gate.release();
    order.join().expect("the order's thread");
    assert_eq!(
        records.installed().expect("read"),
        [tried("web", "2.0.0", None, Some(1))]
    );
}

/// Catches a heartbeat that leaves out the try a finished step record holds, which a coordinator
/// that never heard the order's verdict needs to number past (the install of a self-update, which
/// the daemon ran across its own restart): after an install of `web` at try 1 and a restart of
/// the daemon, `web` is listed at 1.0.0, updating nothing, with the record's try 1.
#[test]
fn the_try_of_a_finished_step_record_is_listed_after_a_restart() {
    let rig = Rig::new("records-try");
    rig.stage("web", "1.0.0");
    let first = rig.daemon();
    assert_eq!(first.order(&install("web", "1.0.0", &d(), 1)), succeeded());
    drop(first);
    let records = DaemonRecords::new(Arc::new(rig.daemon()));
    assert_eq!(
        records.installed().expect("read"),
        [tried("web", "1.0.0", None, Some(1))]
    );
}

/// Catches a step record that cannot be read hiding the service, or making it look updating: the
/// service is listed at its `current` version, updating nothing.
#[test]
fn a_step_record_that_cannot_be_read_updates_nothing() {
    let rig = Rig::new("records-unreadable");
    rig.running("web", "1.0.0");
    let dir = rig.state.join("web");
    fs::create_dir_all(&dir).expect("the service's state directory");
    fs::write(dir.join(RECORD_FILE), "not json").expect("a broken record");
    let records = DaemonRecords::new(Arc::new(rig.daemon()));
    assert_eq!(
        records.installed().expect("read"),
        [listed("web", "1.0.0", None)]
    );
}

/// Catches a packages directory that cannot be listed read as a host that runs nothing: the
/// error is the loop's, which then sends no heartbeat.
#[test]
fn a_packages_directory_that_cannot_be_listed_is_an_error() {
    let rig = Rig::new("records-unlisted");
    fs::write(&rig.packages, "a file where the packages directory goes").expect("a file");
    let records = DaemonRecords::new(Arc::new(rig.daemon()));
    assert!(records.installed().is_err());
}

/// Catches a service the daemon cannot observe reported as absent or stopped: one with no
/// `current`, and one whose manifest does not load, are `unobserved`, unhealthy, with the detail
/// `not_observed`. A service that runs no unit is `unmonitored`, and its health, which nothing
/// probes, is unhealthy.
#[test]
fn a_service_is_observed_through_the_daemons_observer() {
    let rig = Rig::new("observer-adapter");
    rig.running("web", "1.0.0");
    rig.stage("db", "2.0.0");
    rig.store.add("api", "1.0.0", "schema = 1\n");
    rig.store.link("api", "current", "1.0.0");
    let observer = DaemonObserver::new(Arc::new(rig.daemon()));
    let unobserved = Observation {
        liveness: Liveness::Unobserved,
        version: None,
        health: Health::Unhealthy,
        health_detail: Some(NOT_OBSERVED.to_owned()),
    };
    assert_eq!(observer.observe(&name("db")), unobserved);
    assert_eq!(observer.observe(&name("api")), unobserved);
    assert_eq!(
        observer.observe(&name("web")),
        Observation {
            liveness: Liveness::Unmonitored,
            version: None,
            health: Health::Unhealthy,
            health_detail: Some("unmonitored".to_owned()),
        }
    );
}

fn seen(
    state: ServiceState,
    version: VersionView,
    health: HealthView,
) -> hostkeeper_observe::Observation {
    hostkeeper_observe::Observation {
        service: "web".to_owned(),
        state,
        version,
        health,
        recorded: Some("1.0.0".to_owned()),
    }
}

fn reported(
    liveness: Liveness,
    version: Option<&str>,
    health: Health,
    detail: Option<&str>,
) -> Observation {
    Observation {
        liveness,
        version: version.map(self::version),
        health,
        health_detail: detail.map(str::to_owned),
    }
}

/// Catches an observation reported in other words than `Daemon::status`'s: every state of the
/// observer is its liveness (live is running); the version is reported only when the running
/// executable confirmed it; the health is the heartbeat's (a mismatch or a failed probe is
/// unhealthy, an absent service absent, a health nobody probed unhealthy); the detail is the
/// observer's token for what is not plainly live, healthy and confirmed.
#[test]
fn every_observation_maps_to_the_loops_words() {
    let confirmed = || VersionView::Recorded {
        version: "1.0.0".to_owned(),
        confirmed: true,
    };
    let cases = [
        (
            seen(ServiceState::Live, confirmed(), HealthView::Healthy),
            reported(Liveness::Running, Some("1.0.0"), Health::Healthy, None),
        ),
        (
            seen(
                ServiceState::Live,
                VersionView::Recorded {
                    version: "1.0.0".to_owned(),
                    confirmed: false,
                },
                HealthView::NotProbed,
            ),
            reported(
                Liveness::Running,
                None,
                Health::Healthy,
                Some("version_unconfirmed"),
            ),
        ),
        (
            seen(
                ServiceState::Live,
                VersionView::Mismatch {
                    recorded: "1.0.0".to_owned(),
                    observed_note: "another executable".to_owned(),
                },
                HealthView::Healthy,
            ),
            reported(
                Liveness::Running,
                None,
                Health::Unhealthy,
                Some("version_mismatch"),
            ),
        ),
        (
            seen(
                ServiceState::Live,
                VersionView::Unknown,
                HealthView::Unhealthy {
                    reason: UnhealthyReason::Unreachable,
                },
            ),
            reported(
                Liveness::Running,
                None,
                Health::Unhealthy,
                Some("unreachable"),
            ),
        ),
        (
            seen(
                ServiceState::Absent,
                VersionView::NotLive,
                HealthView::NotProbed,
            ),
            reported(Liveness::Absent, None, Health::Absent, Some("absent")),
        ),
        (
            seen(
                ServiceState::Stopped,
                VersionView::NotLive,
                HealthView::NotProbed,
            ),
            reported(Liveness::Stopped, None, Health::Unhealthy, Some("stopped")),
        ),
        (
            seen(
                ServiceState::Transitioning,
                VersionView::NotLive,
                HealthView::NotProbed,
            ),
            reported(
                Liveness::Transitioning,
                None,
                Health::Unhealthy,
                Some("transitioning"),
            ),
        ),
        (
            seen(
                ServiceState::Unobserved {
                    reason: UnobservedReason::ManagerError,
                    detail: "systemctl failed".to_owned(),
                },
                VersionView::NotLive,
                HealthView::NotProbed,
            ),
            reported(
                Liveness::Unobserved,
                None,
                Health::Unhealthy,
                Some("unobserved"),
            ),
        ),
        (
            seen(
                ServiceState::Unmonitored,
                VersionView::NotLive,
                HealthView::Healthy,
            ),
            reported(
                Liveness::Unmonitored,
                None,
                Health::Healthy,
                Some("unmonitored"),
            ),
        ),
        (
            seen(
                ServiceState::Unmonitored,
                VersionView::NotLive,
                HealthView::NotProbed,
            ),
            reported(
                Liveness::Unmonitored,
                None,
                Health::Unhealthy,
                Some("unmonitored"),
            ),
        ),
    ];
    for (observed, expected) in cases {
        assert_eq!(observation(&observed), expected, "{observed:?}");
    }
}
