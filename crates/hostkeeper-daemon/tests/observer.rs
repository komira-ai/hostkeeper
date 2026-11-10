//! The daemon's observer: what the daemon's status holds of how each installed service runs, that a
//! status read never waits for a running step, which executable a version seals, and the
//! observer of a host.

mod support;
mod support_apt;
mod support_boot;

use std::fs;
use std::sync::Arc;

use hostkeeper_config::names::ServiceName;
use hostkeeper_daemon::Reply;
use hostkeeper_daemon::adapters::MAIN_UNIT;
use hostkeeper_daemon::observer::{
    OwnService, STATUS_DEADLINE, host_observer, observe, service_health, service_input,
};
use hostkeeper_daemon::selfupdate::binary_dir;
use hostkeeper_daemon::testing::{manifest, spawn};
use hostkeeper_hook::Step;
use hostkeeper_observe::{
    ActiveState, Current, ExeStat, HealthView, Observation, ServiceInput, ServiceState as Observed,
    UnhealthyReason, UnobservedReason, VersionView, beat_health,
};
use hostkeeper_proto::hostkeeper::agent::v1::Health;
use support::{OWN, d};
use support_boot::Bench;

/// The digest the running executable of the tests' live service has.
const RUNNING: [u8; 32] = [7; 32];

/// The health URL of the tests' services.
const URL: &str = "http://127.0.0.1:9301/healthz";

/// The daemon of `rig` running as `unit`, its binary directory under the rig's state directory.
fn own_service(rig: &Bench, unit: &str) -> OwnService {
    OwnService {
        unit: unit.to_owned(),
        binary_dir: binary_dir(&rig.state),
    }
}

fn name(text: &str) -> ServiceName {
    ServiceName::new(text).expect("a service name")
}

/// `service` at `version` with one run unit, `<service>.service`, and `current` naming it; its
/// manifest has the health URL [`URL`] when `health` is set.
fn installed(rig: &Bench, service: &str, version: &str, health: bool) {
    let unit = format!("{service}.service");
    let mut text = manifest(service, version) + &format!("\n[units]\nrun = [\"{unit}\"]\n");
    if health {
        text.push_str(&format!("\n[health]\nurl = \"{URL}\"\n"));
    }
    rig.store.add(service, version, &text);
    rig.store.seal(service, version, &d());
    rig.store.link(service, "current", version);
}

fn stat() -> ExeStat {
    ExeStat {
        size: 10,
        mtime_secs: 20,
        mtime_nanos: 30,
    }
}

/// Catches an observation dropped, read for the wrong service or left off the wire: two
/// installed services, one live with a confirmed version and a healthy probe and one stopped,
/// are each reported in the library's observation with its reason token and in the health the
/// heartbeat reports; a service with no `current` has no observation and no health.
#[test]
fn status_reports_the_observation_and_the_installed_version_per_service() {
    let rig = Bench::new("observer-status");
    installed(&rig, "live", "1.0.0", true);
    installed(&rig, "stopped", "1.0.0", false);
    rig.stage("bare", "1.0.0");
    rig.manager
        .set("live.service", ActiveState::Active, Some(4242));
    rig.process.set(4242, 77, stat(), RUNNING);
    rig.expected.seal("live", "1.0.0", RUNNING);
    rig.health.answer(URL, 200);
    rig.manager
        .set("stopped.service", ActiveState::Inactive, None);
    let daemon = Arc::new(rig.daemon());

    let status = daemon.status().expect("status");
    let [bare, live, stopped] = &status.services[..] else {
        panic!("three services");
    };
    assert_eq!(
        live.observation,
        Some(Observation {
            service: "live".to_owned(),
            state: Observed::Live,
            version: VersionView::Recorded {
                version: "1.0.0".to_owned(),
                confirmed: true,
            },
            health: HealthView::Healthy,
            recorded: Some("1.0.0".to_owned()),
        })
    );
    assert_eq!(
        live.observation
            .as_ref()
            .and_then(Observation::reason_token),
        None
    );
    assert_eq!(
        stopped.observation,
        Some(Observation {
            service: "stopped".to_owned(),
            state: Observed::Stopped,
            version: VersionView::NotLive,
            health: HealthView::NotProbed,
            recorded: Some("1.0.0".to_owned()),
        })
    );
    assert_eq!(
        stopped
            .observation
            .as_ref()
            .and_then(Observation::reason_token),
        Some("stopped")
    );
    assert_eq!(bare.observation, None);
    assert_eq!(
        service_health(live),
        Some((Health::Healthy, Some("1.0.0".to_owned())))
    );
    assert_eq!(
        service_health(stopped),
        Some((Health::Unhealthy, Some("1.0.0".to_owned())))
    );
    assert_eq!(service_health(bare), None);
    assert_eq!(rig.health.requests(), [URL]);
}

/// Catches an observation that takes the service's lock or the step lock: a step of `web` is
/// held in `prepare` while an update runs, and `status` answers with the observation of the live
/// service, the lock shown as held, before the step is released.
#[test]
fn an_observation_never_waits_for_a_running_step() {
    let rig = Bench::new("observer-unlocked");
    rig.add_units(
        "web",
        support::BASE,
        &["web.service"],
        &["web.service"],
        &[],
    );
    rig.store.link("web", "current", support::BASE);
    rig.add("web", "2.0.0");
    rig.manager
        .set("web.service", ActiveState::Active, Some(4242));
    rig.process.set(4242, 77, stat(), RUNNING);
    rig.expected.seal("web", support::BASE, RUNNING);
    let gate = rig.launcher.hold("web", Step::Prepare);
    let daemon = Arc::new(rig.daemon());
    let order = rig.update("web", "2.0.0", 1);
    let running = {
        let daemon = Arc::clone(&daemon);
        spawn("the order", move || daemon.order(&order))
    };
    gate.entered(&running).expect("the held step is entered");

    let status = daemon.status().expect("status");
    let [web] = &status.services[..] else {
        panic!("one service");
    };
    assert!(web.locked & web.step_running());
    assert_eq!(
        web.observation,
        Some(Observation {
            service: "web".to_owned(),
            state: Observed::Live,
            version: VersionView::Recorded {
                version: support::BASE.to_owned(),
                confirmed: true,
            },
            health: HealthView::NotProbed,
            recorded: Some(support::BASE.to_owned()),
        })
    );
    assert!(Arc::ptr_eq(daemon.observer(), &rig.observer));

    gate.release();
    let reply = running.join().expect("the update");
    assert!(!matches!(reply, Reply::Refused { .. }), "{reply:?}");
}

/// Catches a service observed when it should not be: one with no `current`, one whose `current`
/// names a version whose manifest does not load, and one whose manifest is gone have no input;
/// one whose manifest loads has the version `current` names as its recorded version and the
/// units and health URL of its manifest.
#[test]
fn a_service_with_no_current_or_an_unreadable_manifest_is_not_observed() {
    let rig = Bench::new("observer-input");
    let observer = &rig.observer;
    let (packages, web) = (&rig.packages, name("web"));
    let bin = &binary_dir(&rig.state);
    rig.stage("web", "1.0.0");
    assert_eq!(service_input(packages, bin, &OWN, &web), None);
    assert_eq!(observe(observer, packages, bin, &OWN, &web), None);

    rig.store.link("web", "current", "1.0.0");
    assert_eq!(
        service_input(packages, bin, &OWN, &web),
        Some(ServiceInput {
            service: "web".to_owned(),
            current: Some(Current {
                run: Vec::new(),
                health_url: None,
            }),
            recorded_version: Some("1.0.0".to_owned()),
        })
    );

    installed(&rig, "api", "1.0.0", true);
    assert_eq!(
        service_input(packages, bin, &OWN, &name("api")).and_then(|input| input.current),
        Some(Current {
            run: vec!["api.service".to_owned()],
            health_url: Some(URL.to_owned()),
        })
    );

    let target = packages.join("web/1.0.0/target.json");
    fs::remove_file(&target).expect("remove target.json");
    fs::write(&target, "{").expect("break the manifest");
    assert_eq!(service_input(packages, bin, &OWN, &web), None);
    rig.store.link("web", "current", "9.9.9");
    assert_eq!(service_input(packages, bin, &OWN, &web), None);
}

/// A `Live` observation with a recorded version `1.0.0`.
fn live(version: VersionView, health: HealthView) -> Observation {
    observation(Observed::Live, version, health)
}

fn observation(state: Observed, version: VersionView, health: HealthView) -> Observation {
    Observation {
        service: "web".to_owned(),
        state,
        version,
        health,
        recorded: Some("1.0.0".to_owned()),
    }
}

fn confirmed() -> VersionView {
    VersionView::Recorded {
        version: "1.0.0".to_owned(),
        confirmed: true,
    }
}

fn unconfirmed() -> VersionView {
    VersionView::Recorded {
        version: "1.0.0".to_owned(),
        confirmed: false,
    }
}

fn unhealthy(reason: UnhealthyReason) -> HealthView {
    HealthView::Unhealthy { reason }
}

fn unobserved(reason: UnobservedReason) -> Observed {
    Observed::Unobserved {
        reason,
        detail: "why".to_owned(),
    }
}

/// Catches an observation put in the wrong heartbeat field: each state, version view and health
/// view has its `Health` and the version the beat carries (the recorded one, none for an absent
/// service), and its reason token by the rules of `Observation::reason_token`.
#[test]
fn the_heartbeat_health_and_the_reason_say_what_the_observation_says() {
    let mismatch = VersionView::Mismatch {
        recorded: "1.0.0".to_owned(),
        observed_note: "another executable".to_owned(),
    };
    let none = VersionView::NotLive;
    let not_probed = HealthView::NotProbed;
    let v = Some("1.0.0");
    // The observation, then the health and version of the beat and the reason token.
    let table = [
        (
            observation(Observed::Absent, none.clone(), not_probed.clone()),
            (Health::Absent, None, Some("absent")),
        ),
        (
            observation(Observed::Stopped, none.clone(), not_probed.clone()),
            (Health::Unhealthy, v, Some("stopped")),
        ),
        (
            observation(Observed::Transitioning, none.clone(), not_probed.clone()),
            (Health::Unhealthy, v, Some("transitioning")),
        ),
        (
            observation(
                unobserved(UnobservedReason::ManagerError),
                none.clone(),
                not_probed.clone(),
            ),
            (Health::Unhealthy, v, Some("unobserved")),
        ),
        (
            observation(
                unobserved(UnobservedReason::ChangedDuringRound),
                none.clone(),
                not_probed.clone(),
            ),
            (Health::Unhealthy, v, Some("unobserved")),
        ),
        (
            observation(Observed::Unmonitored, none.clone(), HealthView::Healthy),
            (Health::Healthy, v, Some("unmonitored")),
        ),
        (
            observation(
                Observed::Unmonitored,
                none,
                unhealthy(UnhealthyReason::Unreachable),
            ),
            (Health::Unhealthy, v, Some("unreachable")),
        ),
        (
            live(confirmed(), HealthView::Healthy),
            (Health::Healthy, v, None),
        ),
        (
            live(unconfirmed(), not_probed.clone()),
            (Health::Healthy, v, Some("version_unconfirmed")),
        ),
        (
            live(mismatch.clone(), HealthView::Healthy),
            (Health::Unhealthy, v, Some("version_mismatch")),
        ),
        (
            live(mismatch, unhealthy(UnhealthyReason::StatusNon2xx)),
            (Health::Unhealthy, v, Some("version_mismatch")),
        ),
        (
            live(confirmed(), unhealthy(UnhealthyReason::StatusNon2xx)),
            (Health::Unhealthy, v, Some("status_non_2xx")),
        ),
        (
            live(confirmed(), unhealthy(UnhealthyReason::Unreachable)),
            (Health::Unhealthy, v, Some("unreachable")),
        ),
        (
            live(confirmed(), unhealthy(UnhealthyReason::Redirect)),
            (Health::Unhealthy, v, Some("redirect")),
        ),
        (
            live(confirmed(), unhealthy(UnhealthyReason::UrlInvalid)),
            (Health::Unhealthy, v, Some("url_invalid")),
        ),
        (
            live(VersionView::Unknown, not_probed.clone()),
            (Health::Healthy, v, Some("version_unknown")),
        ),
        (
            Observation {
                recorded: None,
                ..live(VersionView::Unknown, not_probed)
            },
            (Health::Healthy, None, Some("version_unknown")),
        ),
    ];
    for (observation, (health, version, reason)) in table {
        assert_eq!(
            (beat_health(&observation), observation.reason_token()),
            ((health, version.map(str::to_owned)), reason),
            "{observation:?}"
        );
    }
}

/// Catches an observer of the host that reads the host while it is made, or is wired to nothing:
/// it is made from the packages directory, answers `absent` for a service with no `current` and
/// `unmonitored` for one that runs no unit, asking the service manager, the processes and the
/// health URL nothing.
#[test]
fn the_host_observer_answers_without_asking_the_host() {
    let rig = Bench::new("host-observer");
    let observer = host_observer(&rig.packages, OWN, &own_service(&rig, MAIN_UNIT));
    let input = |current: Option<Current>| ServiceInput {
        service: "web".to_owned(),
        current,
        recorded_version: None,
    };
    let quiet = Current {
        run: Vec::new(),
        health_url: None,
    };
    assert_eq!(
        observer.observe(&input(None), STATUS_DEADLINE).state,
        Observed::Absent
    );
    assert_eq!(
        observer.observe(&input(Some(quiet)), STATUS_DEADLINE).state,
        Observed::Unmonitored
    );
    assert_eq!(
        (STATUS_DEADLINE.connect, STATUS_DEADLINE.read),
        (
            std::time::Duration::from_secs(1),
            std::time::Duration::from_secs(1)
        )
    );
}
