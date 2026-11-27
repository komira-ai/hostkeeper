//! The services an enrolled host was updating when last heard, as a restarted coordinator reads
//! them back from its enrollment ([`super::updating`]).

use hostkeeper_enrollment::enroll::{Claims, EnrolledHost, HostRunning, RunningService};
use hostkeeper_enrollment::names::{HostId, Role};
use hostkeeper_wire::Platform;

use super::updating;
use crate::testutil::{id, version};

/// `host`, a daemon that claims `mac_serial`, whose record lists `services` as (name, version,
/// updating).
fn enrolled(mac_serial: &str, services: &[(&str, &str, Option<&str>)]) -> EnrolledHost {
    EnrolledHost {
        host_id: HostId::parse("d1").expect("a host id"),
        claims: Claims {
            mac_serial: mac_serial.to_owned(),
            ..Claims::default()
        },
        approved_unix: 0,
        first_heartbeat_unix: None,
        first_serving_unix: None,
        role: Role::parse("daemon").expect("a role"),
        host_group: String::new(),
        moving_from: None,
        running: Some(HostRunning {
            agent_name: "hostkeeper".to_owned(),
            agent_version: "0.2.0".to_owned(),
            services: services
                .iter()
                .map(|(service, running, to)| RunningService {
                    service: (*service).to_owned(),
                    version: (*running).to_owned(),
                    installer: "deb".to_owned(),
                    updating: to.map(str::to_owned),
                })
                .collect(),
        }),
        running_unix: Some(0),
        bmc: None,
    }
}

/// Proves: each service the record says was updating is read back with the host, its role, its
/// platform (a Mac for a host that claimed a Mac's serial number) and both versions; a service not
/// updating, or one whose versions do not parse, holds nothing, and a host with no record of what
/// it runs holds nothing.
///
/// Catches: a restart that holds the slot of a service the host was not updating, takes a Mac
/// for a Linux host, or fails on a record it cannot read instead of holding nothing for it.
#[test]
fn the_services_a_host_was_updating_are_read_back_and_nothing_else() {
    let linux = enrolled(
        "",
        &[
            ("web", "1.0", Some("2.0")),
            ("db", "1.0", None),
            ("bad-to", "1.0", Some("not a version!")),
            ("bad-from", "not a version!", Some("2.0")),
        ],
    );
    assert_eq!(
        updating(&linux).collect::<Vec<_>>(),
        [(
            "web".to_owned(),
            id("d1"),
            "daemon".to_owned(),
            Platform::LinuxX86_64,
            (version("1.0"), version("2.0"))
        )]
    );
    let mac = enrolled("C02XYZ", &[("web", "1.0", Some("2.0"))]);
    let platforms: Vec<Platform> = updating(&mac)
        .map(|(_, _, _, platform, _)| platform)
        .collect();
    assert_eq!(platforms, [Platform::DarwinArm64]);

    let mut unheard = enrolled("", &[]);
    unheard.running = None;
    assert_eq!(updating(&unheard).count(), 0);
}
