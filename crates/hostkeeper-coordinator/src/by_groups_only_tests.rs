//! A coordinator built to place hosts by host group only (`DesiredStore::by_groups_only`, as
//! `serve` builds it) while it holds no group yet: it is in group mode, orders a host nothing,
//! refuses the role-mode calls and the approvals and records that role mode would take.

use std::sync::Arc;

use hostkeeper_enrollment::enroll::records::MemoryRecords;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::EnrollmentRefusal;
use hostkeeper_wire::wire::ServiceReply;

use crate::desired::DesiredStore;
use crate::enrollment::tests::support::{HostKey, Rig as EnrollmentRig, enroll_request};
use crate::host_groups_support::{World, coordinator_on};
use crate::marks_support::{NEW_DAEMON, ask, beat_of, operator, runs, services_with};
use crate::ops::OpsStore;
use crate::service::SetError;
use crate::service::ci_calls::Setter;
use crate::store::{Backend, Fake, Shared};
use crate::testutil::{Rig, ci_token, desired, set_native};
use crate::verdict_file::VerdictFile;
use crate::write_record::WriteAsk;

/// The reason of the calls of the tests.
const WHY: &str = "for the tests";

/// A world as [`World::new`] makes it, but over a desired state that places by groups only. The
/// coordinator also defines `web` for the role `storage`, as a rollout file would.
fn by_groups_only(name: &str) -> World {
    let fake = Fake::new();
    let records = Arc::new(MemoryRecords::new());
    let enrollment = EnrollmentRig::over_records(&format!("{name}-enrollment"), records.clone());
    let mut rig = Rig::new(name);
    let loaded = fake.load().expect("the fake loads");
    let backend: Shared = Arc::new(fake.clone());
    let list = format!(
        "{}services = [\"web\"]\n",
        hostkeeper_oidc::testkit::config_text(hostkeeper_oidc::testkit::STATIC_ISSUER)
    );
    rig.coordinator = Rig::start_over(
        DesiredStore::over(Arc::clone(&backend), loaded.desired).by_groups_only(),
        &rig.clock,
        &list,
    )
    .with_services(services_with(""))
    .with_legacy_service(None)
    .with_ops(OpsStore::over(Arc::clone(&backend), loaded.ops))
    .with_verdicts(VerdictFile::over(backend, loaded.verdicts))
    .with_enrollment(Arc::clone(&enrollment.enrollment));
    rig.at(2_000);
    World {
        rig,
        fake,
        records,
        enrollment,
    }
}

/// The approval of the request of `key` as host `w1`, naming `role`, `host_group` and `services`.
fn approval(
    key: &HostKey,
    role: &str,
    host_group: &str,
    services: &[&str],
) -> proto::ApproveRequest {
    proto::ApproveRequest {
        request_id: key.key_hash().to_hex(),
        key_sha256: key.key_hash().to_hex(),
        host_id: "w1".to_owned(),
        role: role.to_owned(),
        services: services.iter().map(|s| (*s).to_owned()).collect(),
        replace: false,
        reason: WHY.to_owned(),
        host_group: host_group.to_owned(),
    }
}

/// Proves: a coordinator built to place by groups only is in group mode while it holds no group
/// but the built-in `all`, and one built as a test builds it is not.
///
/// Catches: a `serve` whose coordinator falls back to the role-mode paths until a group is put.
#[test]
fn the_mode_follows_the_build_and_not_only_the_groups_held() {
    let only = by_groups_only("bgo-mode");
    let plain = World::new("bgo-mode-plain");
    let fake = Fake::new();
    let clock = Rig::new("bgo-mode-clock").clock;
    let direct = coordinator_on(&fake, &clock, None);

    assert!(only.coordinator().desired.in_group_mode());
    // It holds the built-in group `all`, and no other.
    let held = only.coordinator().desired.host_groups();
    assert_eq!(
        held.0.keys().map(ToString::to_string).collect::<Vec<_>>(),
        ["all"]
    );
    assert!(plain.coordinator().desired.host_groups().is_empty());
    assert!(!plain.coordinator().desired.in_group_mode());
    assert!(!direct.desired.in_group_mode());
}

/// Proves: with no group held, a host of a role the coordinator's `Services` defines `web` for is
/// ordered nothing although it runs `web` and the store holds a newer desired state of `web`,
/// while the same host of a coordinator that places by role is ordered an update; and CI's
/// `SetDesired` of `web` is refused as in no host group.
///
/// Catches: a coordinator without groups that places by role and orders hosts a service no
/// group lists.
#[tokio::test]
async fn with_no_group_a_host_is_ordered_nothing() {
    let beat = || beat_of(NEW_DAEMON, vec![runs("web", "1.0")]);
    let plain = World::new("bgo-orders-plain");
    set_native(
        plain.coordinator(),
        r#"{"service": "web", "version": "2.0"}"#,
    )
    .await;
    plain.enroll("w1");
    assert!(matches!(
        ask(&plain.rig, "w1", "storage", beat()),
        ServiceReply::Update { .. }
    ));

    let only = by_groups_only("bgo-orders");
    only.coordinator()
        .desired
        .set("web", desired("2.0", 0, 0))
        .expect("set");
    only.enroll("w1");
    assert_eq!(
        ask(&only.rig, "w1", "storage", beat()),
        ServiceReply::Nothing
    );

    let verified = only
        .coordinator()
        .ci(Some(&ci_token()))
        .await
        .expect("a token");
    let refused = only
        .coordinator()
        .set_desired_with(
            &Setter::Ci(&verified),
            br#"{"service": "web", "version": "3.0"}"#,
            &WriteAsk::default(),
            None,
        )
        .await;
    assert!(
        matches!(refused, Err(SetError::NotInHostGroup)),
        "{refused:?}"
    );
}

/// Proves: with no group held, `Services.Assign` and `Services.Unassign` are refused
/// `placed_by_host_group` and write nothing.
///
/// Catches: an assignment written that no decision reads.
#[test]
fn with_no_group_an_assignment_is_refused() {
    let world = by_groups_only("bgo-assign");
    let before = world.state();

    assert_eq!(
        world
            .coordinator()
            .assign(
                &operator(),
                proto::AssignRequest {
                    node_id: "w1".to_owned(),
                    service: "web".to_owned(),
                    role: String::new(),
                },
            )
            .err(),
        Some(proto::ServicesRefusal::PlacedByHostGroup)
    );
    assert_eq!(
        world
            .coordinator()
            .unassign(
                &operator(),
                proto::UnassignRequest {
                    node_id: "w1".to_owned(),
                    service: "web".to_owned(),
                    uninstall: false,
                    role: String::new(),
                },
            )
            .err(),
        Some(proto::ServicesRefusal::PlacedByHostGroup)
    );
    assert_eq!(world.state(), before);
}

/// Proves: with no group held, an approval naming services is refused `services_retired`, one
/// naming a group is refused `unknown_host_group`, and one naming a role that is no group is
/// refused `unknown_host_group`; nothing is approved.
///
/// Catches: an approval that places a host by role while the coordinator holds no group.
#[test]
fn with_no_group_an_approval_naming_services_or_an_unheld_group_is_refused() {
    let world = by_groups_only("bgo-approve");
    let key = HostKey::new();
    world
        .enrollment
        .enrollment
        .enroll(enroll_request(&key, "machine-1"))
        .expect("posted");
    let approve = |request| {
        world
            .coordinator()
            .enrollment_approve(&operator(), request)
            .map(|answer| answer.host_id)
    };

    assert_eq!(
        approve(approval(&key, "", "web", &["web"])),
        Err(EnrollmentRefusal::ServicesRetired)
    );
    assert_eq!(
        approve(approval(&key, "", "web", &[])),
        Err(EnrollmentRefusal::UnknownHostGroup)
    );
    assert_eq!(
        approve(approval(&key, "storage", "", &[])),
        Err(EnrollmentRefusal::UnknownHostGroup)
    );
    assert!(
        world
            .enrollment
            .enrollment
            .store()
            .recorded_approvals()
            .expect("read")
            .is_empty()
    );
}

/// Proves: with no group held, an inventory record naming a group (by `host_group` or by `role`)
/// is refused `unknown_host_group`, and one naming neither is kept.
///
/// Catches: a record that names a group no coordinator holds being accepted at serve's start.
#[test]
fn with_no_group_a_record_naming_a_group_is_refused() {
    let world = by_groups_only("bgo-record");
    let add = |host_group: &str, role: &str| {
        world.coordinator().add_record(
            &operator(),
            proto::AddRecordRequest {
                host_id: "db-1".to_owned(),
                role: role.to_owned(),
                host_group: host_group.to_owned(),
                data_macs: vec!["0c:c4:7a:00:00:02".to_owned()],
                bmc_mac: "0c:c4:7a:00:00:01".to_owned(),
                ..proto::AddRecordRequest::default()
            },
        )
    };

    assert_eq!(
        add("web", "").err(),
        Some(EnrollmentRefusal::UnknownHostGroup)
    );
    assert_eq!(
        add("", "web").err(),
        Some(EnrollmentRefusal::UnknownHostGroup)
    );
    assert!(add("", "").is_ok());
}
