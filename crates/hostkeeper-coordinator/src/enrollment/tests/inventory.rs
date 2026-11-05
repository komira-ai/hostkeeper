//! `Enrollment.List` over the enrollment store and the host inventory: each pending request is
//! shown with the one record it matches, none, or every candidate; a match approves nothing; a
//! MAC bound to an enrolled host is a duplicate claim; the view pages oldest first.

use std::sync::Arc;

use hostkeeper_enrollment::enroll::{ApproveError, Decision, RequestState};
use hostkeeper_enrollment::names::Role;
use hostkeeper_proto::hostkeeper::agent::v1::{EnrollResponse, EnrollState};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use super::support::{HostKey, NOW, Rig, approve, enroll_request, host_id};
use crate::marks_support::operator;
use crate::testutil::{Log, Rig as CoordinatorRig};

use proto::EnrollmentRefusal as Refusal;
use proto::InventoryMatch as Match;

/// A coordinator over the enrollment of `enrollment`.
fn coordinator(name: &str, enrollment: &Rig) -> CoordinatorRig {
    let mut rig = CoordinatorRig::new(name);
    rig.coordinator = CoordinatorRig::start(&rig.dir, &rig.clock)
        .with_enrollment(Arc::clone(&enrollment.enrollment));
    rig
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

fn pending(
    rig: &CoordinatorRig,
    limit: u32,
    token: &str,
) -> Result<proto::ListPendingResponse, Refusal> {
    rig.coordinator.list_pending(
        &operator(),
        proto::ListPendingRequest {
            limit,
            page_token: token.to_owned(),
        },
    )
}

/// The one pending request of `key`.
fn shown(rig: &CoordinatorRig, key: &HostKey) -> proto::PendingEnrollment {
    let hex = key.key_hash().to_hex();
    let all = pending(rig, 0, "").expect("listed").requests;
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

    let decision = Decision {
        host_id: host_id(&view.record_host_id),
        role: Role::parse(&view.record_role).expect("a role"),
        services: Vec::new(),
        replace: false,
    };
    let other = HostKey::new();
    let store = enrollment.enrollment.store();
    let refused = store.approve(&key.key_hash(), &other.key_hash(), &decision);
    assert!(
        matches!(refused, Err(ApproveError::KeyMismatch)),
        "{refused:?}"
    );
    let still = store.request(&key.key_hash()).expect("read").expect("held");
    assert_eq!(still.state, RequestState::Pending);
    assert_eq!(shown(&rig, &key).inventory_match(), Match::Matched);

    let approved = store
        .approve(&key.key_hash(), &key.key_hash(), &decision)
        .expect("its own key is approved");
    assert_eq!(approved.host_id, host_id("db-1"));
    assert_eq!(pending(&rig, 0, "").expect("listed").requests, Vec::new());
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

/// Catches: a pending view that is not oldest first, a page that repeats or skips a request, a
/// next-page token on the last page, a token this call never gave let through, a coordinator
/// without enrollment that refuses instead of listing nothing, and an unreadable store answered
/// as an empty queue.
#[test]
fn the_pending_view_pages_oldest_first_and_says_when_the_store_cannot_be_read() {
    let enrollment = Rig::new("inventory-pages");
    let rig = coordinator("inventory-pages-coordinator", &enrollment);
    let keys: Vec<HostKey> = (0..3).map(|_| HostKey::new()).collect();
    for (n, key) in keys.iter().enumerate().rev() {
        enrollment.clock.set(NOW + u64::try_from(n).expect("small"));
        post(&enrollment, key, &format!("m-{n}"), &[], "");
    }
    let first = pending(&rig, 2, "").expect("a page");
    let ids: Vec<&str> = first
        .requests
        .iter()
        .map(|p| p.request_id.as_str())
        .collect();
    let hex: Vec<String> = keys.iter().map(|k| k.key_hash().to_hex()).collect();
    assert_eq!(ids, [hex[0].as_str(), hex[1].as_str()]);
    assert_eq!(first.next_page_token, format!("{}.{}", NOW + 1, hex[1]));
    assert_eq!(first.requests[0].first_seen_unix, NOW);
    assert_eq!(first.requests[0].inventory_match(), Match::Unmatched);
    let second = pending(&rig, 2, &first.next_page_token).expect("a page");
    let ids: Vec<&str> = second
        .requests
        .iter()
        .map(|p| p.request_id.as_str())
        .collect();
    assert_eq!(
        (ids, second.next_page_token.as_str()),
        (vec![hex[2].as_str()], "")
    );
    let exact = pending(&rig, 3, "").expect("a page");
    assert_eq!(
        (exact.requests.len(), exact.next_page_token.as_str()),
        (3, "")
    );

    let upper = format!("{NOW}.{}", hex[0].to_uppercase());
    for token in [
        "db-1",
        "x.ab",
        &format!("{NOW}"),
        &format!("{NOW}.{}", &hex[0][1..]),
        &upper,
    ] {
        assert_eq!(
            pending(&rig, 0, token).err(),
            Some(Refusal::InvalidRequest),
            "{token}"
        );
    }

    let without = CoordinatorRig::new("inventory-pages-none");
    let none = without
        .coordinator
        .list_pending(&operator(), proto::ListPendingRequest::default())
        .expect("no enrollment, nothing pending");
    assert_eq!(none, proto::ListPendingResponse::default());

    let file = enrollment
        .files
        .store
        .join("requests")
        .join(format!("{}.pb", hex[0]));
    std::fs::write(&file, b"not a request").expect("spoiled");
    assert_eq!(pending(&rig, 0, "").err(), Some(Refusal::StoreFailed));
}
