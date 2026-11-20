//! What the Linux kinds' `verify` reads of the services a host runs ([`HostServices`]): each
//! installed service's health as the heartbeat reports it, over the daemon's observer (here the
//! rig's, reading fakes); and the kinds a host that cannot be checked holds ([`host_kinds`]).

mod support;

use std::fs;

use hostkeeper_daemon::maintenance_hook::linux::{HostServices, ServiceHealth, host_kinds};
use hostkeeper_daemon::maintenance_hook::{Kind, KindError, MaintainParams};
use hostkeeper_daemon::selfupdate::binary_dir;
use hostkeeper_daemon::testing::manifest;
use hostkeeper_launchd::Platform;
use hostkeeper_observe::{ActiveState, ExeStat};
use hostkeeper_proto::hostkeeper::agent::v1::{
    Health, Maintain, MaintainAction, MaintenanceKind as WireKind,
};
use support::{OWN, Rig, d};

/// The digest of the executable the live service runs.
const RUNNING: [u8; 32] = [7; 32];

/// The health URL of the live service.
const URL: &str = "http://127.0.0.1:9301/healthz";

/// `service` at `version` installed under the rig's packages directory and made `current`: its
/// unit `<service>.service`, with `URL` as its health check when `health`.
fn installed(rig: &Rig, service: &str, version: &str, health: bool) {
    let unit = format!("{service}.service");
    let mut text = manifest(service, version) + &format!("\n[units]\nrun = [\"{unit}\"]\n");
    if health {
        text.push_str(&format!("\n[health]\nurl = \"{URL}\"\n"));
    }
    rig.store.add(service, version, &text);
    rig.store.seal(service, version, &d());
    rig.store.link(service, "current", version);
}

/// The services of `rig`, as the check reads them.
fn services_of(rig: &Rig) -> HostServices {
    HostServices::new(
        rig.packages.clone(),
        binary_dir(&rig.state),
        OWN,
        rig.observer.clone(),
    )
}

/// Proves: every installed service with a `current` is listed in name order with the health its
/// heartbeat reports: one live and passing its health check healthy, one whose unit is stopped
/// unhealthy; a service with no `current` is not observed and left out.
///
/// Catches: a check that reads health from anything but the observer the heartbeat reports, or
/// that judges a service the host does not run.
#[test]
fn each_installed_service_is_listed_with_its_heartbeat_health() {
    let rig = Rig::new("mh-services");
    installed(&rig, "live", "1.0.0", true);
    installed(&rig, "stopped", "1.0.0", false);
    rig.store
        .add("staged", "1.0.0", &manifest("staged", "1.0.0"));
    rig.manager
        .set("live.service", ActiveState::Active, Some(4242));
    let stat = ExeStat {
        size: 10,
        mtime_secs: 20,
        mtime_nanos: 30,
    };
    rig.process.set(4242, 77, stat, RUNNING);
    rig.expected.seal("live", "1.0.0", RUNNING);
    rig.health.answer(URL, 200);
    rig.manager
        .set("stopped.service", ActiveState::Inactive, None);

    assert_eq!(
        services_of(&rig).services(),
        Ok(vec![
            ("live".to_owned(), Health::Healthy),
            ("stopped".to_owned(), Health::Unhealthy),
        ])
    );
}

/// Proves: a host with no packages directory runs no service, and a packages directory that
/// cannot be read is an error naming it, never an empty list.
///
/// Catches: an unreadable packages directory read as "no service", which passes a host whose
/// services nobody looked at.
#[test]
fn no_packages_directory_is_no_service_and_an_unreadable_one_is_an_error() {
    let rig = Rig::new("mh-services-none");
    assert_eq!(services_of(&rig).services(), Ok(Vec::new()));

    fs::write(&rig.packages, "not a directory").expect("a file in its place");
    let error = fs::read_dir(&rig.packages).expect_err("not a directory");
    assert_eq!(
        services_of(&rig).services(),
        Err(format!("{}: {error}", rig.packages.display()))
    );
}

/// Proves: a Mac, and a build whose own version is not `major.minor.patch`, hold their
/// platform's stubs, whose `verify` fails `unsupported_kind` without reading the host.
///
/// Catches: a host that cannot be checked made to look checked.
#[test]
fn a_host_that_cannot_be_checked_holds_the_stubs() {
    let params = MaintainParams::new(Maintain {
        run_id: "run-1".to_owned(),
        action: MaintainAction::Run.into(),
        kinds: vec![WireKind::Reboot.into()],
        ..Maintain::default()
    })
    .expect("valid parameters");
    let cases = [
        (
            host_kinds(&Platform::macos(), "0.1.0"),
            vec![Kind::Macos, Kind::Xcode, Kind::Reboot],
        ),
        (
            host_kinds(&Platform::linux(), "0.1"),
            vec![Kind::Linux, Kind::Reboot],
        ),
    ];
    for (registry, kinds) in cases {
        assert_eq!(registry.kinds().collect::<Vec<_>>(), kinds);
        for kind in kinds {
            let work = registry.get(kind).expect("held");
            assert_eq!(work.verify(&params), Err(KindError::unsupported(kind)));
        }
    }
}
