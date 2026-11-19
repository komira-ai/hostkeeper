//! The rules a BMC report keeps, and a report heard by a coordinator without enrollment.

use std::net::{IpAddr, Ipv4Addr};

use hostkeeper_proto::hostkeeper::agent::v1 as agent;

use super::{FACT, read};
use crate::marks_support::who;
use crate::testutil::{OPERATOR, Rig};

fn report(ip: &str, mac: &str) -> agent::BmcReport {
    agent::BmcReport {
        ip: ip.to_owned(),
        mac: mac.to_owned(),
    }
}

/// Proves: an IPv4 address in dotted decimal and a MAC as the inventory takes one are read, the
/// MAC in any case; an address no BMC can be reached at, an IPv6 or malformed address, and a
/// malformed or all-zero MAC are refused.
///
/// Catches: `0.0.0.0` (a BMC with no lease yet) or a loopback, link-local, multicast or broadcast address
/// kept and dialled, an IPv6 address taken where the report says IPv4, and the all-zero MAC of
/// an unconfigured channel kept.
#[test]
fn a_report_is_read_only_with_a_usable_ipv4_address_and_a_mac() {
    let mac = "0C:C4:7A:00:00:01";
    let read_ip = |ip: &str| read(&report(ip, mac)).map(|bmc| bmc.ip);
    assert_eq!(
        read_ip("192.0.2.12"),
        Some(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 12)))
    );
    assert_eq!(
        read(&report("192.0.2.12", mac)).map(|bmc| bmc.mac.to_string()),
        Some("0c:c4:7a:00:00:01".to_owned())
    );
    // A multicast and a link-local address, written from their octets.
    let multicast = Ipv4Addr::new(224, 0, 0, 1).to_string();
    let link_local = Ipv4Addr::new(169, 254, 0, 12).to_string();
    for refused in [
        "0.0.0.0",
        "127.0.0.1",
        multicast.as_str(),
        "255.255.255.255",
        link_local.as_str(),
        "::ffff:192.0.2.12",
        "2001:db8::12",
        "192.0.2",
        "",
    ] {
        assert_eq!(read_ip(refused), None, "{refused}");
    }
    for refused in ["00:00:00:00:00:00", "0c:c4:7a:00:00", "", "not a mac"] {
        assert_eq!(read(&report("192.0.2.12", refused)), None, "{refused}");
    }
}

/// Proves: a heartbeat with no report and one with a report to a coordinator without enrollment
/// change nothing and count nothing; a report that breaks a rule is counted under `bmc`.
///
/// Catches: a coordinator without enrollment failing on a report, and a broken report not
/// counted.
#[test]
fn a_report_without_enrollment_is_dropped_quietly_and_a_broken_one_counted() {
    let rig = Rig::new("bmc-report-no-enrollment");
    let host = who("s1", "storage");
    let dropped = |rig: &Rig| {
        let status = rig.coordinator.status(Some(OPERATOR)).expect("status");
        status.extras_dropped.get(FACT).copied()
    };
    rig.coordinator
        .hear_bmc(&host, &agent::HeartbeatRequest::default());
    let with = agent::HeartbeatRequest {
        bmc: Some(report("192.0.2.12", "0c:c4:7a:00:00:01")),
        ..agent::HeartbeatRequest::default()
    };
    rig.coordinator.hear_bmc(&host, &with);
    assert_eq!(dropped(&rig), None);
    let broken = agent::HeartbeatRequest {
        bmc: Some(report("0.0.0.0", "0c:c4:7a:00:00:01")),
        ..agent::HeartbeatRequest::default()
    };
    rig.coordinator.hear_bmc(&host, &broken);
    assert_eq!(dropped(&rig), Some(1));
}
