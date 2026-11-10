//! `Enrollment.Approve` of a request by the host inventory: a request that matches exactly one
//! record is approved as that record's host id (an empty host id is it; another needs `replace`),
//! and a request that matches no record or several, or is a renewal, is approved as the host id
//! named, as before the inventory. The view of each match is `inventory.rs`'s. The coordinators
//! here have only closed approval windows ([`super::support::closed_windows`]), so a request
//! waits for the operator's approval.

use hostkeeper_enrollment::enroll::RequestState;
use hostkeeper_proto::hostkeeper::agent::v1::{EnrollRequest, EnrollState};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use super::support::{
    HostKey, Rig, audit_after, audit_of, audited, closed_windows, enroll_request, enrolled, host_id,
};
use crate::inventory::InventoryRecord;
use crate::marks_support::operator;
use crate::testutil::Rig as CoordinatorRig;

use proto::EnrollmentRefusal as Refusal;

/// The data MAC of the record `db-1`.
const DB1_DATA: &str = "0c:c4:7a:00:00:02";

/// The data MAC of the record `db-2`.
const DB2_DATA: &str = "0c:c4:7a:00:00:12";

/// A coordinator over the enrollment of `enrollment`, whose records' windows are closed
/// ([`closed_windows`]): a matching request waits for an operator here.
fn coordinator(name: &str, enrollment: &Rig) -> CoordinatorRig {
    closed_windows(name, enrollment)
}

/// Records `host` (role `storage`) with the data MAC `data`.
fn record(rig: &CoordinatorRig, host: &str, data: &str) {
    rig.coordinator
        .add_record(
            &operator(),
            proto::AddRecordRequest {
                host_id: host.to_owned(),
                role: "storage".to_owned(),
                data_macs: vec![data.to_owned()],
                ..proto::AddRecordRequest::default()
            },
        )
        .expect("recorded");
}

/// Posts the request of `key` from machine `machine` claiming the addresses `addresses` and
/// no BMC MAC.
fn post(enrollment: &Rig, key: &HostKey, machine: &str, addresses: &[&str]) {
    let mut request = enroll_request(key, machine);
    request.claims.as_mut().expect("claims").addresses =
        addresses.iter().map(|a| (*a).to_owned()).collect();
    let posted = enrollment.enrollment.enroll(request).expect("posted");
    assert_eq!(posted.state, EnrollState::Pending as i32);
}

/// The approval of `key`'s request as `host` (empty for none named), a storage host.
fn approval(key: &HostKey, host: &str, replace: bool) -> proto::ApproveRequest {
    proto::ApproveRequest {
        request_id: key.key_hash().to_hex(),
        key_sha256: key.key_hash().to_hex(),
        host_id: host.to_owned(),
        role: "storage".to_owned(),
        services: Vec::new(),
        replace,
        reason: "built today".to_owned(),
        host_group: String::new(),
    }
}

/// Where `key`'s request stands, and the host id it was approved as.
fn standing(enrollment: &Rig, key: &HostKey) -> (RequestState, Option<String>) {
    let view = enrollment
        .enrollment
        .store()
        .request(&key.key_hash())
        .expect("read")
        .expect("held");
    (view.state, view.host_id.map(|host| host.to_string()))
}

/// Catches: an empty host id refused (or approved as anything but the record's) for a request
/// that matches one record, the defaulted host id missing from the answer, and the approval or
/// its audit entry naming another host id.
#[test]
fn a_matched_request_approved_without_a_host_id_is_approved_as_the_records() {
    let enrollment = Rig::new("inventory-default");
    let rig = coordinator("inventory-default-coordinator", &enrollment);
    record(&rig, "db-1", DB1_DATA);
    let key = HostKey::new();
    post(&enrollment, &key, "m-1", &[DB1_DATA]);
    let before = audit_of(&rig.coordinator).len();

    let approved = rig
        .coordinator
        .enrollment_approve(&operator(), approval(&key, "", false))
        .expect("approved as the record's host id");

    assert_eq!(approved.host_id, "db-1");
    assert_eq!(
        standing(&enrollment, &key),
        (RequestState::Approved, Some("db-1".to_owned()))
    );
    let entries = audit_after(&rig.coordinator, before);
    assert_eq!(
        entries
            .iter()
            .map(|(_, action, node, _)| (action.as_str(), node.as_deref()))
            .collect::<Vec<_>>(),
        [("enroll_approve", Some("db-1"))]
    );
    assert_eq!(
        approved.entry.map(|entry| entry.node_id),
        Some(Some("db-1".to_owned()))
    );
}

/// Catches: a matched request approved under a host id other than its record's without
/// `replace`, a refused one that changes the request or is not audited `record_mismatch`, and
/// `replace` not letting the operator's host id through.
#[test]
fn a_host_id_other_than_the_matched_records_is_refused_unless_the_approval_replaces() {
    let enrollment = Rig::new("inventory-mismatch");
    let rig = coordinator("inventory-mismatch-coordinator", &enrollment);
    record(&rig, "db-1", DB1_DATA);
    let key = HostKey::new();
    post(&enrollment, &key, "m-1", &[DB1_DATA]);
    let before = audit_of(&rig.coordinator).len();

    let refused = rig
        .coordinator
        .enrollment_approve(&operator(), approval(&key, "db-9", false));

    assert_eq!(refused.err(), Some(Refusal::RecordMismatch));
    assert_eq!(standing(&enrollment, &key), (RequestState::Pending, None));
    assert_eq!(
        enrollment
            .enrollment
            .store()
            .host_role(&host_id("db-9"))
            .expect("read"),
        None
    );
    assert_eq!(
        audit_after(&rig.coordinator, before),
        [audited(
            "operator",
            "enroll_approve_refused",
            Some("db-9"),
            &format!("record_mismatch; request {}", key.key_hash())
        )]
    );

    let approved = rig
        .coordinator
        .enrollment_approve(&operator(), approval(&key, "db-9", true))
        .expect("replace approves the host id named");
    assert_eq!(approved.host_id, "db-9");
    assert_eq!(
        standing(&enrollment, &key),
        (RequestState::Approved, Some("db-9".to_owned()))
    );
    assert_eq!(
        approved.entry.map(|entry| (entry.action, entry.node_id)),
        Some(("enroll_replace".to_owned(), Some("db-9".to_owned())))
    );
}

/// Catches: an unmatched request held to the inventory (refused, or given a host id), and an
/// empty host id for it approved as anything at all: with no record matching, the approval is
/// what it was before the inventory, so a farm with no inventory approves as it did.
#[test]
fn an_unmatched_request_is_approved_as_the_host_id_named_and_needs_one() {
    let enrollment = Rig::new("inventory-unmatched");
    let rig = coordinator("inventory-unmatched-coordinator", &enrollment);
    let key = HostKey::new();
    post(&enrollment, &key, "m-1", &[DB1_DATA]);

    assert_eq!(
        rig.coordinator
            .enrollment_approve(&operator(), approval(&key, "", false))
            .err(),
        Some(Refusal::InvalidHostId)
    );
    assert_eq!(standing(&enrollment, &key), (RequestState::Pending, None));

    record(&rig, "db-2", DB2_DATA);
    let approved = rig
        .coordinator
        .enrollment_approve(&operator(), approval(&key, "web-1", false))
        .expect("an unmatched request is approved as named");
    assert_eq!(approved.host_id, "web-1");
    assert_eq!(
        standing(&enrollment, &key),
        (RequestState::Approved, Some("web-1".to_owned()))
    );
}

/// Catches: an ambiguous match that picks one of its candidates for an empty host id, or that
/// refuses the host id the operator chose; and a renewal held to a record it matches rather
/// than to the host id it renews.
#[test]
fn an_ambiguous_match_or_a_renewal_is_approved_as_the_host_id_named() {
    let enrollment = Rig::new("inventory-ambiguous");
    let rig = coordinator("inventory-ambiguous-coordinator", &enrollment);
    record(&rig, "db-1", DB1_DATA);
    record(&rig, "db-2", DB2_DATA);
    let both = HostKey::new();
    post(&enrollment, &both, "m-both", &[DB1_DATA, DB2_DATA]);

    assert_eq!(
        rig.coordinator
            .enrollment_approve(&operator(), approval(&both, "", false))
            .err(),
        Some(Refusal::InvalidHostId)
    );
    assert_eq!(
        rig.coordinator
            .enrollment_approve(&operator(), approval(&both, "db-2", false))
            .expect("the operator chooses among the candidates")
            .host_id,
        "db-2"
    );

    let renewing = HostKey::new();
    enrolled(&enrollment.enrollment, &renewing, "m-renew", "s6");
    let mut renewal = EnrollRequest {
        renewal_of: "s6".to_owned(),
        ..enroll_request(&renewing, "m-renew")
    };
    renewal.claims.as_mut().expect("claims").addresses = vec![DB2_DATA.to_owned()];
    enrollment.enrollment.enroll(renewal).expect("a renewal");
    // The renewal matches db-2's record alone, and is approved as the host it renews.
    assert_eq!(
        rig.coordinator
            .enrollment_approve(&operator(), approval(&renewing, "", false))
            .err(),
        Some(Refusal::InvalidHostId)
    );
    assert_eq!(
        rig.coordinator
            .enrollment_approve(&operator(), approval(&renewing, "s6", false))
            .expect("a renewal is approved as the host it renews")
            .host_id,
        "s6"
    );
}

/// Catches: a record whose host id breaks the grammar (an ops file edited by hand) approved
/// under it, or answered as anything but `invalid_host_id`; and a key mismatch hidden behind
/// the record's check.
#[test]
fn a_record_whose_host_id_breaks_its_rule_names_no_host() {
    let enrollment = Rig::new("inventory-bad-record");
    let key = HostKey::new();
    post(&enrollment, &key, "m-1", &[DB1_DATA]);
    let inventory = [InventoryRecord {
        host_id: "Not_A_Host".to_owned(),
        role: String::new(),
        data_macs: vec![DB1_DATA.to_owned()],
        bmc_mac: String::new(),
        serial: String::new(),
        notes: String::new(),
        by: "operator".to_owned(),
        at_unix: 0,
        approve_until_unix: 0,
        used: None,
        platform: None,
    }];
    let approve = |request| {
        enrollment
            .enrollment
            .approve("operator", request, &|_| true, &inventory)
            .map(|approved| approved.host_id.to_string())
    };

    assert_eq!(
        approve(approval(&key, "", false)),
        Err(Refusal::InvalidHostId)
    );
    let other = HostKey::new();
    assert_eq!(
        approve(proto::ApproveRequest {
            key_sha256: other.key_hash().to_hex(),
            ..approval(&key, "db-9", false)
        }),
        Err(Refusal::KeyMismatch)
    );
    assert_eq!(standing(&enrollment, &key), (RequestState::Pending, None));
    assert_eq!(approve(approval(&key, "db-9", true)), Ok("db-9".to_owned()));
}
