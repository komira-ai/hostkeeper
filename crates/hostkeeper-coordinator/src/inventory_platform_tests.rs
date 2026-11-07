//! The platform of an inventory record (network-install design, section 11, "The record's
//! platform"): `AddRecord` takes a Linux platform or none, `ListRecords` shows it, a restart
//! keeps it, the audit reason names it, and a platform that is not a Linux one is refused
//! before anything is written.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::Platform;

use crate::inventory::platform_proto;
use crate::marks_support::{operator, restart, rig};
use crate::testutil::Rig;

use proto::EnrollmentRefusal as Refusal;

/// An add of `host` with one data MAC and the platform number `platform`.
fn add_with(host: &str, mac: &str, platform: i32) -> proto::AddRecordRequest {
    proto::AddRecordRequest {
        host_id: host.to_owned(),
        role: "storage".to_owned(),
        data_macs: vec![mac.to_owned()],
        platform,
        ..proto::AddRecordRequest::default()
    }
}

fn add(rig: &Rig, request: proto::AddRecordRequest) -> Result<proto::AddRecordResponse, Refusal> {
    rig.coordinator.add_record(&operator(), request)
}

/// The platforms the coordinator lists, by host id, as the numbers on the wire.
fn listed(rig: &Rig) -> Vec<(String, i32)> {
    rig.coordinator
        .list_records(&operator(), proto::ListRecordsRequest::default())
        .expect("listed")
        .records
        .into_iter()
        .map(|r| (r.host_id, r.platform))
        .collect()
}

/// Proves: a record added with each Linux platform, and one with none, answers the platform it
/// was given, lists it, and keeps it across a restart (read back from the halts file), in
/// host id order.
///
/// Catches: a platform dropped between the request and the record, or lost when the files are
/// read again.
#[test]
fn a_record_keeps_the_linux_platform_it_was_added_with_across_a_restart() {
    let mut rig = rig("inventory-platform-add", "");
    let x86 = add(
        &rig,
        add_with(
            "db-1",
            "0c:c4:7a:00:00:01",
            agent::Platform::LinuxX8664 as i32,
        ),
    )
    .expect("added");
    assert_eq!(
        x86.record.expect("the record").platform,
        agent::Platform::LinuxX8664 as i32
    );
    let arm = add(
        &rig,
        add_with(
            "db-2",
            "0c:c4:7a:00:00:02",
            agent::Platform::LinuxArm64 as i32,
        ),
    )
    .expect("added");
    assert_eq!(
        arm.record.expect("the record").platform,
        agent::Platform::LinuxArm64 as i32
    );
    let none = add(
        &rig,
        add_with(
            "db-3",
            "0c:c4:7a:00:00:03",
            agent::Platform::Unspecified as i32,
        ),
    )
    .expect("added");
    assert_eq!(none.record.expect("the record").platform, 0);

    let want = vec![
        ("db-1".to_owned(), agent::Platform::LinuxX8664 as i32),
        ("db-2".to_owned(), agent::Platform::LinuxArm64 as i32),
        ("db-3".to_owned(), 0),
    ];
    assert_eq!(listed(&rig), want);
    restart(&mut rig, "");
    assert_eq!(listed(&rig), want);
}

/// Proves: the audit reason of `record_add` ends with `platform <name>` when the record states
/// one, and has no such part when it states none.
///
/// Catches: a platform that changes the record without the audit saying so.
#[test]
fn the_record_add_audit_reason_names_the_platform() {
    let rig = rig("inventory-platform-audit", "");
    let with = add(
        &rig,
        add_with(
            "db-1",
            "0c:c4:7a:00:00:01",
            agent::Platform::LinuxArm64 as i32,
        ),
    )
    .expect("added");
    let until = with.record.as_ref().expect("the record").approve_until_unix;
    assert_eq!(
        with.entry.expect("audited").reason,
        Some(format!(
            "role storage; data 0c:c4:7a:00:00:01; approve until {until}; platform linux-arm64"
        ))
    );
    let without = add(&rig, add_with("db-2", "0c:c4:7a:00:00:02", 0)).expect("added");
    let until = without
        .record
        .as_ref()
        .expect("the record")
        .approve_until_unix;
    assert_eq!(
        without.entry.expect("audited").reason,
        Some(format!(
            "role storage; data 0c:c4:7a:00:00:02; approve until {until}"
        ))
    );
}

/// Proves: an add naming the Mac platform, or a number this release does not know (also a
/// negative one), is refused `invalid_request`; nothing is listed, kept or audited.
///
/// Catches: a record that `hostkeeper-pxe` could never install, or a number kept that no
/// release can name, accepted into the inventory.
#[test]
fn a_platform_that_is_not_linux_is_refused_and_nothing_is_written() {
    let rig = rig("inventory-platform-refused", "");
    for number in [agent::Platform::DarwinArm64 as i32, 99, -1] {
        assert_eq!(
            add(&rig, add_with("db-1", "0c:c4:7a:00:00:01", number)).err(),
            Some(Refusal::InvalidRequest),
            "platform {number}"
        );
    }
    assert!(listed(&rig).is_empty());
    let state = rig.coordinator.ops.state();
    assert!(state.inventory.is_empty());
    assert!(state.audit.is_empty());
}

/// Proves: every platform maps to the one `hostkeeper.agent.v1` names the same way (its
/// `PLATFORM_` name, upper-cased and with `-` read as `_`).
///
/// Catches: two platforms crossed in the mapping, or a platform added to [`Platform`] and not
/// to the proto.
#[test]
fn each_platform_maps_to_the_proto_platform_of_its_name() {
    for platform in Platform::ALL {
        let shown = platform_proto(platform).as_str_name();
        let name = platform.as_str().to_uppercase().replace('-', "_");
        assert_eq!(shown, format!("PLATFORM_{name}"));
    }
}
