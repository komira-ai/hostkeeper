//! A service with no run units (`unmonitored`): the service manager and the process are not
//! asked, and a health URL in its manifest is probed once.

use std::time::Duration;

use hostkeeper_observe::fake::{
    FakeExpectedExe, FakeHealthClient, FakeProcessProbe, FakeServiceManager,
};
use hostkeeper_observe::{
    Current, Deadline, HealthView, Observation, ObserveError, ObserveOptions, Observer, Ports,
    ServiceInput, ServiceState, UnhealthyReason, UnobservedReason, VersionView,
};

const DEADLINE: Deadline = Deadline {
    connect: Duration::from_secs(2),
    read: Duration::from_secs(3),
};
const URL_A: &str = "http://127.0.0.1:9301/healthz";

/// An observer over fakes, and a clone of each fake the tests read.
struct Rig {
    manager: FakeServiceManager,
    process: FakeProcessProbe,
    health: FakeHealthClient,
    observer: Observer,
}

fn rig() -> Rig {
    let (manager, process) = (FakeServiceManager::default(), FakeProcessProbe::default());
    let health = FakeHealthClient::default();
    let ports = Ports {
        manager: Box::new(manager.clone()),
        process: Box::new(process.clone()),
        health: Box::new(health.clone()),
        expected: Box::new(FakeExpectedExe::default()),
    };
    let observer = Observer::new(ports, ObserveOptions::default());
    Rig {
        manager,
        process,
        health,
        observer,
    }
}

impl Rig {
    fn observe(&self, svc: &ServiceInput) -> Observation {
        self.observer.observe(svc, DEADLINE)
    }
}

/// tierstore with `run` units, the health URL `url`, at `recorded`.
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

fn unobserved(reason: UnobservedReason, detail: &str) -> ServiceState {
    ServiceState::Unobserved {
        reason,
        detail: detail.to_owned(),
    }
}

/// Catches a service with no run units whose manifest has a health URL being left unprobed, or
/// probed for anything but that URL once: a 2xx is healthy, any other answer and no answer are
/// unhealthy, a `{port}` URL (no instance to fill it from) is invalid and not asked, and a probe
/// that cannot be made is `unobserved`. Neither the service manager nor the process is asked.
#[test]
fn an_unmonitored_service_with_a_health_url_is_probed_once() {
    let unhealthy = |reason| HealthView::Unhealthy { reason };
    let cases = [
        (Some(200), URL_A, HealthView::Healthy, None),
        (
            Some(500),
            URL_A,
            unhealthy(UnhealthyReason::StatusNon2xx),
            Some("status_non_2xx"),
        ),
        (
            Some(301),
            URL_A,
            unhealthy(UnhealthyReason::Redirect),
            Some("redirect"),
        ),
        (
            None,
            URL_A,
            unhealthy(UnhealthyReason::Unreachable),
            Some("unreachable"),
        ),
        (
            Some(200),
            "http://127.0.0.1:{port}/healthz",
            unhealthy(UnhealthyReason::UrlInvalid),
            Some("url_invalid"),
        ),
    ];
    for (status, url, want, token) in cases {
        let r = rig();
        status
            .into_iter()
            .for_each(|status| r.health.answer(URL_A, status));
        let seen = r.observe(&with_current(&[], Some(url), Some("1.5.0")));
        assert_eq!(seen.state, ServiceState::Unmonitored, "{url} {status:?}");
        assert_eq!(seen.version, VersionView::NotLive, "{url} {status:?}");
        assert_eq!(seen.health, want, "{url} {status:?}");
        assert_eq!(
            seen.reason_token(),
            token.or(Some("unmonitored")),
            "{url} {status:?}"
        );
        let asked = usize::from(!url.contains("{port}"));
        assert_eq!(r.health.requests().len(), asked, "{url} {status:?}");
        assert!(r.manager.calls().is_empty());
        assert!(r.process.calls().is_empty());
    }

    let broken = rig();
    broken.health.then(
        URL_A,
        Err(ObserveError::Probe {
            url: URL_A.to_owned(),
            why: "too many open files".to_owned(),
        }),
    );
    assert_eq!(
        broken.observe(&with_current(&[], Some(URL_A), None)).state,
        unobserved(
            UnobservedReason::HealthError,
            "tierstore: the probe of http://127.0.0.1:9301/healthz could not be made: too many open files"
        )
    );
}
