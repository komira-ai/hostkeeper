//! `Enrollment.List` over the enrollment store and the host inventory: each pending request is
//! shown with the one record it matches, none, or every candidate; a match against a record whose
//! window is closed approves nothing (the coordinators here have only closed windows); a
//! MAC bound to an enrolled host is a duplicate claim. Paging, tokens and an unreadable store are
//! `list.rs`'s.

use hostkeeper_enrollment::enroll::RequestState;
use hostkeeper_proto::hostkeeper::agent::v1::{EnrollResponse, EnrollState};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use super::support::{HostKey, Rig, approve, closed_windows, enroll_request};
use crate::marks_support::operator;
use crate::testutil::{Log, Rig as CoordinatorRig};

use proto::EnrollmentRefusal as Refusal;
use proto::InventoryMatch as Match;

/// A coordinator over the enrollment of `enrollment`, whose records' windows are closed
/// ([`closed_windows`]): a matching request waits for an operator here.
fn coordinator(name: &str, enrollment: &Rig) -> CoordinatorRig {
    closed_windows(name, enrollment)
}

/// Records `host` (role `storage`) with the data MACs `data` and the BMC MAC `bmc`.
fn record(rig: &CoordinatorRig, host: &str, data: &[&str], bmc: &str) {
    rig.coordinator
        .add_record(
            &operator(),
            proto::AddRecordRequest {
                host_id: host.to_owned(),
                role: "storage".to_owned(),
                data_macs: data.iter().map(|m| (*m).to_owned()).collect(),
                bmc_mac: bmc.to_owned(),
                ..proto::AddRecordRequest::default()
            },
        )
        .expect("recorded");
}

/// Posts the request of `key` from machine `machine` claiming the addresses `addresses` and the
/// BMC MAC `bmc` (empty for none).
fn post(
    enrollment: &Rig,
    key: &HostKey,
    machine: &str,
    addresses: &[&str],
    bmc: &str,
) -> EnrollResponse {
    let mut request = enroll_request(key, machine);
    let claims = request.claims.as_mut().expect("claims");
    claims.addresses = addresses.iter().map(|a| (*a).to_owned()).collect();
    claims.bmc_mac = bmc.to_owned();
    enrollment.enrollment.enroll(request).expect("posted")
}

fn pending(rig: &CoordinatorRig) -> proto::ListPendingResponse {
    rig.coordinator
        .enrollment_list(&operator(), &proto::ListPendingRequest::default())
        .expect("listed")
}

/// The one pending request of `key`.
fn shown(rig: &CoordinatorRig, key: &HostKey) -> proto::PendingEnrollment {
    let hex = key.key_hash().to_hex();
    let all = pending(rig).requests;
    let mut found = all.into_iter().filter(|p| p.request_id == hex);
    let one = found.next().expect("listed");
    assert!(found.next().is_none());
    one
}

/// Catches: a pending request shown without its match, a match filled in from the wrong record,
/// a BMC MAC lost on the way from the request to the view, a missing BMC claim taken as a
/// mismatch, several matching records reduced to one, and a removed record still matched.
#[test]
fn the_pending_view_shows_the_one_matching_record_none_or_every_candidate() {
    let enrollment = Rig::new("inventory-view");
    let rig = coordinator("inventory-view-coordinator", &enrollment);
    record(&rig, "db-1", &["0c:c4:7a:00:00:02"], "0c:c4:7a:00:00:01");
    record(&rig, "db-2", &["0c:c4:7a:00:00:12"], "0c:c4:7a:00:00:11");

    let exact = HostKey::new();
    post(
        &enrollment,
        &exact,
        "m-exact",
        &["0C-C4-7A-00-00-02"],
        "0C:C4:7A:00:00:01",
    );
    let view = shown(&rig, &exact);
    assert_eq!(
        (
            view.inventory_match(),
            view.record_host_id.as_str(),
            view.record_role.as_str(),
            view.candidate_host_ids.len()
        ),
        (Match::Matched, "db-1", "storage", 0)
    );
    assert_eq!(view.key_sha256, exact.key_hash().to_hex());
    assert_eq!(
        view.claims.as_ref().map(|c| c.bmc_mac.as_str()),
        Some("0C:C4:7A:00:00:01")
    );
    assert!(!view.duplicate_claim);

    let no_bmc = HostKey::new();
    post(&enrollment, &no_bmc, "m-no-bmc", &["0c:c4:7a:00:00:12"], "");
    let view = shown(&rig, &no_bmc);
    assert_eq!(
        (view.inventory_match(), view.record_host_id.as_str()),
        (Match::Matched, "db-2")
    );

    let stranger = HostKey::new();
    post(
        &enrollment,
        &stranger,
        "m-stranger",
        &["0c:c4:7a:00:00:99"],
        "",
    );
    let view = shown(&rig, &stranger);
    assert_eq!(
        (
            view.inventory_match(),
            view.record_host_id.as_str(),
            view.record_role.as_str()
        ),
        (Match::Unmatched, "", "")
    );

    let both = HostKey::new();
    post(
        &enrollment,
        &both,
        "m-both",
        &["0c:c4:7a:00:00:02", "0c:c4:7a:00:00:12"],
        "",
    );
    let view = shown(&rig, &both);
    assert_eq!(
        (view.inventory_match(), view.record_host_id.as_str()),
        (Match::Ambiguous, "")
    );
    assert_eq!(view.candidate_host_ids, ["db-1", "db-2"]);

    rig.coordinator
        .remove_record(
            &operator(),
            proto::RemoveRecordRequest {
                host_id: "db-1".to_owned(),
                reason: "rebuilt".to_owned(),
            },
        )
        .expect("removed");
    assert_eq!(shown(&rig, &exact).inventory_match(), Match::Unmatched);
    assert_eq!(shown(&rig, &both).inventory_match(), Match::Matched);
}

/// Catches: a match against an operator's record that approves, or lets an approval through for
/// a key other than the request's: the record filled in the host id, and the approval still
/// names the key hash (design 5.7, step 3).
#[test]
fn a_matched_request_still_refuses_another_key() {
    let enrollment = Rig::new("inventory-key");
    let rig = coordinator("inventory-key-coordinator", &enrollment);
    record(&rig, "db-1", &["0c:c4:7a:00:00:02"], "0c:c4:7a:00:00:01");
    let key = HostKey::new();
    let posted = post(
        &enrollment,
        &key,
        "m-1",
        &["0c:c4:7a:00:00:02"],
        "0c:c4:7a:00:00:01",
    );
    assert_eq!(posted.state, EnrollState::Pending as i32);
    let view = shown(&rig, &key);
    assert_eq!(view.inventory_match(), Match::Matched);

    let approval = |key_sha256: String| proto::ApproveRequest {
        request_id: key.key_hash().to_hex(),
        key_sha256,
        host_id: view.record_host_id.clone(),
        role: view.record_role.clone(),
        services: Vec::new(),
        replace: false,
        reason: "built today".to_owned(),
        host_group: String::new(),
    };
    let other = HostKey::new();
    let refused = rig
        .coordinator
        .enrollment_approve(&operator(), approval(other.key_hash().to_hex()));
    assert_eq!(refused.err(), Some(Refusal::KeyMismatch));
    let store = enrollment.enrollment.store();
    let still = store.request(&key.key_hash()).expect("read").expect("held");
    assert_eq!(still.state, RequestState::Pending);
    assert_eq!(shown(&rig, &key).inventory_match(), Match::Matched);

    let approved = rig
        .coordinator
        .enrollment_approve(&operator(), approval(key.key_hash().to_hex()))
        .expect("its own key is approved");
    assert_eq!(approved.host_id, "db-1");
    assert_eq!(pending(&rig).requests, Vec::new());
}

/// Catches: a second machine claiming a MAC of an enrolled host shown as an ordinary pending
/// request, with no alert; the duplicate is flagged whatever record it matches.
#[test]
fn a_mac_bound_to_an_enrolled_host_shows_as_a_duplicate_claim_with_an_alert() {
    let enrollment = Rig::new("inventory-bound");
    let rig = coordinator("inventory-bound-coordinator", &enrollment);
    let log = Log::default();
    let _guard = log.install();
    record(&rig, "db-1", &["0c:c4:7a:00:00:02"], "0c:c4:7a:00:00:01");
    let first = HostKey::new();
    post(
        &enrollment,
        &first,
        "m-1",
        &["0c:c4:7a:00:00:02"],
        "0c:c4:7a:00:00:01",
    );
    approve(&enrollment.enrollment, &first, "db-1", false);
    assert!(!log.text().contains("duplicate enrollment claim"));

    let second = HostKey::new();
    let posted = post(&enrollment, &second, "m-2", &["0c-c4-7a-00-00-02"], "");
    assert_eq!(posted.state, EnrollState::DuplicateClaim as i32);
    let view = shown(&rig, &second);
    assert!(view.duplicate_claim);
    assert_eq!(view.inventory_match(), Match::Matched);
    let text = log.text();
    assert!(
        text.contains("duplicate enrollment claim")
            && text.contains("a MAC is bound to an enrolled host"),
        "{text}"
    );
}
