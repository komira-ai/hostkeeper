//! The heartbeat's reading of the daemon's own service ([`super::own_running`]).

use hostkeeper_config::names::ServiceName;
use hostkeeper_link::{Health, Liveness, Observation};

use super::{OWN_UNIT_NOT_LOADED, own_running};

fn name(text: &str) -> ServiceName {
    ServiceName::new(text).expect("a service name")
}

fn seen(liveness: Liveness, health: Health, detail: &str) -> Observation {
    Observation {
        liveness,
        version: None,
        health,
        health_detail: Some(detail.to_owned()),
        drift: None,
    }
}

/// Catches the daemon reporting its own service absent while it runs from its installed package
/// (a unit an earlier move left hides the package's, so the service manager finds no unit): the
/// coordinator reads absent as not installed and orders the install of what is installed. The
/// daemon's own service is reported running and unhealthy, `own_unit_not_loaded`.
#[test]
fn the_daemons_own_service_observed_absent_is_running_and_unhealthy() {
    assert_eq!(
        own_running(
            &name("hostkeeper"),
            seen(Liveness::Absent, Health::Absent, "absent")
        ),
        seen(Liveness::Running, Health::Unhealthy, OWN_UNIT_NOT_LOADED)
    );
}

/// Catches the reading reaching past its one case: another service observed absent is absent,
/// and the daemon's own service observed otherwise is reported as observed.
#[test]
fn another_service_or_another_health_is_reported_as_observed() {
    let absent = seen(Liveness::Absent, Health::Absent, "absent");
    assert_eq!(own_running(&name("web"), absent.clone()), absent);
    for observed in [
        seen(Liveness::Stopped, Health::Unhealthy, "stopped"),
        seen(Liveness::Running, Health::Healthy, "healthy"),
    ] {
        assert_eq!(own_running(&name("hostkeeper"), observed.clone()), observed);
    }
}
