//! Approval by the host inventory through the coordinator: a record that names a role is the
//! approval of the host it was entered for, so a request that matches it exactly, while its
//! window is open, is approved with no `Enrollment.Approve` call: when it is posted, and when
//! the record is added or opened after the daemon posted it. The conditions that leave a request
//! to an operator are `by_record_waits.rs`'s, and the restart's repair `by_record_repair.rs`'s.
//! Both the enrollment's clock and the ops clock read the test's [`FixedClock`], so a window is
//! open or closed by that clock alone.

use hostkeeper_enrollment::enroll::RequestState;
use hostkeeper_proto::hostkeeper::agent::v1::{EnrollResponse, EnrollState, PollState};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use super::support::{
    HostKey, NOW, Rig, add_of, aligned, audit_of, audited, claiming, enrolled, host_id, identity,
    mac_request, requested, signed_poll,
};
use crate::marks_support::operator;
use crate::testutil::Rig as CoordinatorRig;

use proto::EnrollmentRefusal as Refusal;
use proto::RecordState;

/// The data MAC of the record `db-1`.
const DB1_DATA: &str = "0c:c4:7a:00:00:02";

/// The BMC MAC of the record `db-1`.
const DB1_BMC: &str = "0c:c4:7a:00:00:01";

/// The data MAC of the record `db-2`.
const DB2_DATA: &str = "0c:c4:7a:00:00:12";

/// The data MAC of the record `db-3`.
const DB3_DATA: &str = "0c:c4:7a:00:00:22";

/// A day, the length of a window nobody names.
const DAY: u64 = 86_400;

/// The enrollment and the coordinator over it that the tests of this file start from.
struct World {
    enrollment: Rig,
    rig: CoordinatorRig,
}

fn world(name: &str) -> World {
    let enrollment = Rig::new(name);
    let rig = aligned(&format!("{name}-coordinator"), &enrollment);
    World { enrollment, rig }
}

impl World {
    /// Adds the record `request` describes.
    fn add(&self, request: proto::AddRecordRequest) -> proto::AddRecordResponse {
        self.rig
            .coordinator
            .add_record(&operator(), request)
            .expect("recorded")
    }

    /// The `Enroll` of `request` as the daemon makes it, through the coordinator.
    fn post(
        &self,
        request: hostkeeper_proto::hostkeeper::agent::v1::EnrollRequest,
    ) -> EnrollResponse {
        self.rig
            .coordinator
            .enroll(&self.enrollment.enrollment, request)
            .expect("posted")
    }

    /// Where `key`'s request stands, and the host id it was approved as.
    fn standing(&self, key: &HostKey) -> (RequestState, Option<String>) {
        let view = self
            .enrollment
            .enrollment
            .store()
            .request(&key.key_hash())
            .expect("read")
            .expect("held");
        (view.state, view.host_id.map(|host| host.to_string()))
    }

    /// The record of `host`, as `Enrollment.ListRecords` shows it.
    fn record(&self, host: &str) -> proto::InventoryRecord {
        self.rig
            .coordinator
            .list_records(&operator(), proto::ListRecordsRequest::default())
            .expect("listed")
            .records
            .into_iter()
            .find(|record| record.host_id == host)
            .expect("a record")
    }

    /// The one pending request of `key`, as `Enrollment.List` shows it.
    fn shown(&self, key: &HostKey) -> proto::PendingEnrollment {
        let hex = key.key_hash().to_hex();
        self.rig
            .coordinator
            .enrollment_list(&operator(), &proto::ListPendingRequest::default())
            .expect("listed")
            .requests
            .into_iter()
            .find(|pending| pending.request_id == hex)
            .expect("listed")
    }

    /// The host ids and actions of the audit entries after the first `skip`.
    fn actions_after(&self, skip: usize) -> Vec<String> {
        audit_of(&self.rig.coordinator)
            .split_off(skip)
            .into_iter()
            .map(|(_, action, _, _)| action)
            .collect()
    }
}

/// Proves: a request that matches a record that names a role exactly, within its window, is
/// approved by the coordinator when it is posted, with no `Approve` call: the answer is
/// `UNSPECIFIED` (poll it), the approval is the record's host id and role, bound to the SHA-256
/// of the request's key (that key's `Poll` is issued the certificate, and another key is not the
/// host), no service is assigned, and the audit holds `enrollment.auto_approved` by `inventory`
/// in the exact text, between the new request and the issue.
#[tokio::test]
async fn one_exact_match_within_the_window_enrolls_with_no_approve_call() {
    let w = world("by-record-exact");
    w.add(add_of("db-1", &[DB1_DATA], DB1_BMC));
    let key = HostKey::new();
    let before = audit_of(&w.rig.coordinator).len();

    let answer = w.post(claiming(&key, "m-1", &[DB1_DATA], DB1_BMC));

    assert_eq!(answer.state, EnrollState::Unspecified as i32);
    assert_eq!(answer.request_id, key.key_hash().to_hex());
    assert_eq!(
        w.standing(&key),
        (RequestState::Approved, Some("db-1".to_owned()))
    );
    let store = w.enrollment.enrollment.store();
    assert!(
        store
            .host_role_for(&host_id("db-1"), &key.key_hash())
            .expect("read")
            .is_some_and(|role| role.as_str() == "storage")
    );
    assert_eq!(
        store
            .host_role_for(&host_id("db-1"), &HostKey::new().key_hash())
            .expect("read"),
        None,
        "the approval is bound to this key alone"
    );
    let approvals = store.recorded_approvals().expect("read");
    let [approval] = approvals.as_slice() else {
        panic!("one approval: {approvals:?}");
    };
    assert_eq!(
        (
            approval.approver.as_str(),
            approval.host_id.as_str(),
            approval.role.as_str(),
            approval.services.len(),
            approval.replace,
            approval.approved_unix,
            approval.reason.as_str(),
        ),
        (
            "inventory",
            "db-1",
            "storage",
            0,
            false,
            NOW,
            "approved by its inventory record db-1"
        )
    );

    let issued = signed_poll(&w.enrollment.enrollment, &key, &key).expect("polled");
    assert_eq!(issued.state, PollState::Issued as i32);
    assert_eq!(issued.host_id, "db-1");
    let who = identity(&w.rig.coordinator, &issued.certificate_der);
    assert_eq!(
        (who.host_id.as_str(), who.role.as_str()),
        ("db-1", "storage")
    );

    assert_eq!(
        audit_of(&w.rig.coordinator).split_off(before),
        [
            requested(&key, None, "pending"),
            audited(
                "inventory",
                "enrollment.auto_approved",
                Some("db-1"),
                &format!(
                    "request {}; record db-1; role storage; services none; claims machine_id=m-1 \
                     addresses={DB1_DATA}; approved by its inventory record db-1",
                    key.key_hash()
                )
            ),
            audited(
                "coordinator",
                "enroll_issued",
                Some("db-1"),
                &format!("request {}; round 0; serial 3", key.key_hash())
            ),
        ]
    );
}

/// Proves: the approval uses the record: it is listed `USED` with the request and the time, and
/// a second request that claims the record's MAC is not approved by it but is a
/// `duplicate_claim`, which still needs `Approve` with `replace`.
#[tokio::test]
async fn a_used_record_approves_nothing_again_and_a_second_request_is_a_duplicate_claim() {
    let w = world("by-record-used");
    w.add(add_of("db-1", &[DB1_DATA], ""));
    let first = HostKey::new();
    w.post(claiming(&first, "m-1", &[DB1_DATA], ""));
    w.enrollment.clock.set(NOW + 5);

    let record = w.record("db-1");
    assert_eq!(
        (
            record.state(),
            record.used_by_request_id.as_str(),
            record.used_unix,
            record.approve_until_unix
        ),
        (
            RecordState::Used,
            first.key_hash().to_hex().as_str(),
            NOW,
            NOW + DAY
        )
    );

    let second = HostKey::new();
    let answer = w.post(claiming(&second, "m-2", &[DB1_DATA], ""));
    assert_eq!(answer.state, EnrollState::DuplicateClaim as i32);
    assert_eq!(w.standing(&second), (RequestState::DuplicateClaim, None));
    assert_eq!(
        w.enrollment
            .enrollment
            .store()
            .host_role(&host_id("db-1"))
            .expect("read")
            .map(|role| role.as_str().to_owned()),
        Some("storage".to_owned())
    );
    assert_eq!(
        w.enrollment
            .enrollment
            .store()
            .host_role_for(&host_id("db-1"), &first.key_hash())
            .expect("read")
            .map(|role| role.as_str().to_owned()),
        Some("storage".to_owned()),
        "the first key still holds the host id"
    );
}

/// Proves: a request that matches the record after its window ended waits for an operator:
/// pending, listed `MATCHED` against a `CLOSED` record with its end; and the window is open
/// while the time is before its end and closed at it.
#[tokio::test]
async fn a_match_after_the_window_is_pending_and_the_window_ends_at_its_end() {
    let w = world("by-record-window");
    w.add(add_of("db-1", &[DB1_DATA], ""));
    w.add(add_of("db-2", &[DB2_DATA], ""));

    w.enrollment.clock.set(NOW + DAY - 1);
    let inside = HostKey::new();
    assert_eq!(
        w.post(claiming(&inside, "m-1", &[DB1_DATA], "")).state,
        EnrollState::Unspecified as i32
    );
    assert_eq!(w.standing(&inside).0, RequestState::Approved);

    w.enrollment.clock.set(NOW + DAY);
    let at_the_end = HostKey::new();
    assert_eq!(
        w.post(claiming(&at_the_end, "m-2", &[DB2_DATA], "")).state,
        EnrollState::Pending as i32
    );
    assert_eq!(w.standing(&at_the_end), (RequestState::Pending, None));
    let shown = w.shown(&at_the_end);
    assert_eq!(
        (
            shown.inventory_match(),
            shown.record_host_id.as_str(),
            shown.record_state(),
            shown.record_approve_until_unix
        ),
        (
            proto::InventoryMatch::Matched,
            "db-2",
            RecordState::Closed,
            NOW + DAY
        )
    );
    assert_eq!(w.record("db-2").state(), RecordState::Closed);
}

/// Proves: a request that claims the addresses of two records is ambiguous and waits for an
/// operator, whatever the windows.
#[tokio::test]
async fn two_matching_records_are_ambiguous_and_the_request_is_pending() {
    let w = world("by-record-ambiguous");
    w.add(add_of("db-1", &[DB1_DATA], ""));
    w.add(add_of("db-2", &[DB2_DATA], ""));
    let key = HostKey::new();

    let answer = w.post(claiming(&key, "m-1", &[DB1_DATA, DB2_DATA], ""));

    assert_eq!(answer.state, EnrollState::Pending as i32);
    assert_eq!(w.standing(&key), (RequestState::Pending, None));
    let shown = w.shown(&key);
    assert_eq!(shown.inventory_match(), proto::InventoryMatch::Ambiguous);
    assert_eq!(shown.candidate_host_ids, ["db-1", "db-2"]);
    assert_eq!(shown.record_state(), RecordState::Unspecified);
    assert_eq!(shown.record_approve_until_unix, 0);
}

/// Proves: when the record holds a BMC MAC the request must claim it: a request that claims
/// another BMC MAC does not match the record at all, one that claims none matches it without
/// being exact and waits (the match for the operator is stricter than what the list shows), and
/// one that claims the same BMC MAC, in any spelling, is approved.
#[tokio::test]
async fn the_bmc_mac_must_be_claimed_and_agree() {
    let w = world("by-record-bmc");
    w.add(add_of("db-1", &[DB1_DATA], DB1_BMC));
    w.add(add_of("db-2", &[DB2_DATA], "0c:c4:7a:00:00:11"));
    w.add(add_of("db-3", &[DB3_DATA], "0c:c4:7a:00:00:21"));

    let other = HostKey::new();
    w.post(claiming(&other, "m-1", &[DB1_DATA], "0c:c4:7a:00:00:99"));
    assert_eq!(w.standing(&other), (RequestState::Pending, None));
    assert_eq!(
        w.shown(&other).inventory_match(),
        proto::InventoryMatch::Unmatched
    );
    assert_eq!(w.record("db-1").state(), RecordState::Open);

    let silent = HostKey::new();
    w.post(claiming(&silent, "m-2", &[DB2_DATA], ""));
    assert_eq!(w.standing(&silent), (RequestState::Pending, None));
    let shown = w.shown(&silent);
    assert_eq!(
        (shown.inventory_match(), shown.record_state()),
        (proto::InventoryMatch::Matched, RecordState::Open),
        "matched against an open record, and still waiting"
    );
    assert_eq!(w.record("db-2").state(), RecordState::Open);

    let agreeing = HostKey::new();
    let answer = w.post(claiming(&agreeing, "m-3", &[DB3_DATA], "0C-C4-7A-00-00-21"));
    assert_eq!(answer.state, EnrollState::Unspecified as i32);
    assert_eq!(
        w.standing(&agreeing),
        (RequestState::Approved, Some("db-3".to_owned()))
    );
}

/// Proves: a Mac's request is approved by the record that holds its serial (letters compared
/// without case), with no MAC to agree.
#[tokio::test]
async fn a_mac_serial_match_approves() {
    let w = world("by-record-serial");
    let mut mac = add_of("mac-1", &[], "");
    mac.serial = "C02XK1ABJG5J".to_owned();
    mac.role = "build".to_owned();
    w.add(mac);
    let key = HostKey::new();

    let answer = w.post(mac_request(&key, "m-mac", "c02xk1abjg5j", b""));

    assert_eq!(answer.state, EnrollState::Unspecified as i32);
    assert_eq!(
        w.standing(&key),
        (RequestState::Approved, Some("mac-1".to_owned()))
    );
    assert!(
        w.enrollment
            .enrollment
            .store()
            .host_role_for(&host_id("mac-1"), &key.key_hash())
            .expect("read")
            .is_some_and(|role| role.as_str() == "build")
    );
    assert_eq!(w.record("mac-1").state(), RecordState::Used);
}

/// Proves: a record entered after the daemon posted its request approves it: the daemon posts
/// once and then polls, so the add itself tries the pending requests that match; the answer of
/// the add is the record as added, and the record is listed used afterwards.
#[tokio::test]
async fn a_record_added_after_the_request_was_posted_approves_it() {
    let w = world("by-record-after");
    let key = HostKey::new();
    let answer = w.post(claiming(&key, "m-1", &[DB1_DATA], ""));
    assert_eq!(answer.state, EnrollState::Pending as i32);
    let before = audit_of(&w.rig.coordinator).len();

    let added = w.add(add_of("db-1", &[DB1_DATA], ""));

    assert_eq!(
        added
            .record
            .as_ref()
            .map(|r| (r.state(), r.approve_until_unix)),
        Some((RecordState::Open, NOW + DAY))
    );
    assert_eq!(
        w.standing(&key),
        (RequestState::Approved, Some("db-1".to_owned()))
    );
    assert_eq!(w.record("db-1").state(), RecordState::Used);
    assert_eq!(
        w.actions_after(before),
        ["record_add", "enrollment.auto_approved"]
    );
    let issued = signed_poll(&w.enrollment.enrollment, &key, &key).expect("polled");
    assert_eq!(issued.state, PollState::Issued as i32);
}

/// Proves: `OpenRecord` opens the window of a record that ended again, from now, and a request
/// that was waiting is approved by it; the call is audited `record_open` with the end and the
/// reason, and its answer is the record with its new window.
#[tokio::test]
async fn open_record_reopens_a_closed_record_and_approves_the_waiting_request() {
    let w = world("by-record-open");
    let mut short = add_of("db-1", &[DB1_DATA], "");
    short.approve_for_secs = 60;
    w.add(short);
    w.enrollment.clock.set(NOW + 120);
    let key = HostKey::new();
    w.post(claiming(&key, "m-1", &[DB1_DATA], ""));
    assert_eq!(w.standing(&key), (RequestState::Pending, None));
    assert_eq!(w.record("db-1").state(), RecordState::Closed);
    let before = audit_of(&w.rig.coordinator).len();

    let opened = w
        .rig
        .coordinator
        .open_record(
            &operator(),
            proto::OpenRecordRequest {
                host_id: "db-1".to_owned(),
                approve_for_secs: 3600,
                reason: "rack 4 is ready".to_owned(),
                ..proto::OpenRecordRequest::default()
            },
        )
        .expect("opened");

    let record = opened.record.expect("a record");
    assert_eq!(
        (
            record.host_id.as_str(),
            record.state(),
            record.approve_until_unix
        ),
        ("db-1", RecordState::Open, NOW + 120 + 3600)
    );
    let entry = opened.entry.expect("an entry");
    assert_eq!(
        (
            entry.action.as_str(),
            entry.node_id.as_deref(),
            entry.reason.as_deref()
        ),
        (
            "record_open",
            Some("db-1"),
            Some(format!("until {}; rack 4 is ready", NOW + 120 + 3600).as_str())
        )
    );
    assert_eq!(
        w.standing(&key),
        (RequestState::Approved, Some("db-1".to_owned()))
    );
    assert_eq!(w.record("db-1").state(), RecordState::Used);
    assert_eq!(
        w.actions_after(before),
        ["record_open", "enrollment.auto_approved"]
    );
}

/// Proves: `OpenRecord` of a record the coordinator approved a request by is refused
/// `record_used`, and changes nothing: not its window, not the audit.
#[tokio::test]
async fn open_record_of_a_used_record_is_refused_record_used() {
    let w = world("by-record-open-used");
    w.add(add_of("db-1", &[DB1_DATA], ""));
    w.post(claiming(&HostKey::new(), "m-1", &[DB1_DATA], ""));
    let before = audit_of(&w.rig.coordinator).len();
    let record = w.record("db-1");

    let refused = w.rig.coordinator.open_record(
        &operator(),
        proto::OpenRecordRequest {
            host_id: "db-1".to_owned(),
            approve_for_secs: 60,
            ..proto::OpenRecordRequest::default()
        },
    );

    assert_eq!(refused.err(), Some(Refusal::RecordUsed));
    assert_eq!(w.record("db-1"), record);
    assert_eq!(w.actions_after(before), Vec::<String>::new());
}

/// Proves: two pending requests that match one record contest it, so neither is approved by it;
/// once an operator rejects one, the other, posted again, is approved.
#[tokio::test]
async fn a_contested_record_approves_neither_request_until_one_is_rejected() {
    let w = world("by-record-contested");
    let first = HostKey::new();
    let second = HostKey::new();
    w.post(claiming(&first, "m-1", &[DB1_DATA], ""));
    w.post(claiming(&second, "m-2", &[DB1_DATA], ""));

    // Both waited before the record was entered; entering it tries both, and neither wins.
    w.add(add_of("db-1", &[DB1_DATA], ""));

    assert_eq!(w.standing(&first), (RequestState::Pending, None));
    assert_eq!(w.standing(&second), (RequestState::Pending, None));
    assert_eq!(w.shown(&first).contested_by, [second.key_hash().to_hex()]);
    assert_eq!(w.shown(&first).record_state(), RecordState::Open);
    assert_eq!(w.record("db-1").state(), RecordState::Open);

    w.rig
        .coordinator
        .enrollment_reject(
            &operator(),
            &proto::RejectRequest {
                request_id: second.key_hash().to_hex(),
                reason: String::new(),
            },
        )
        .expect("rejected");
    let again = w.post(claiming(&first, "m-1", &[DB1_DATA], ""));

    assert_eq!(again.state, EnrollState::Unspecified as i32);
    assert_eq!(
        w.standing(&first),
        (RequestState::Approved, Some("db-1".to_owned()))
    );
}

/// Proves: a renewal is never approved by a record, even one that matches it exactly in an open
/// window: it waits for an operator, who approves it as the host it renews.
#[tokio::test]
async fn a_renewal_waits_for_an_operator() {
    let w = world("by-record-renewal");
    let renewing = HostKey::new();
    enrolled(&w.enrollment.enrollment, &renewing, "m-renew", "s6");
    w.add(add_of("db-1", &[DB1_DATA], ""));
    let mut renewal = claiming(&renewing, "m-renew", &[DB1_DATA], "");
    renewal.renewal_of = "s6".to_owned();

    let answer = w.post(renewal);

    assert_eq!(answer.state, EnrollState::Pending as i32);
    assert_eq!(w.standing(&renewing).0, RequestState::Pending);
    assert_eq!(w.record("db-1").state(), RecordState::Open);
}

/// Proves: a host id another key holds is not taken by a record: the store refuses the
/// approval, the request stays pending, nothing is audited as an auto approval, and the record
/// stays open and unused; the refusal is logged at info with its token.
#[tokio::test]
async fn a_host_id_another_key_holds_leaves_the_request_pending() {
    let w = world("by-record-taken");
    let holder = HostKey::new();
    enrolled(&w.enrollment.enrollment, &holder, "m-holder", "db-1");
    w.add(add_of("db-1", &[DB1_DATA], ""));
    let key = HostKey::new();
    let before = audit_of(&w.rig.coordinator).len();
    let log = crate::testutil::Log::default();
    let guard = log.install();

    let answer = w.post(claiming(&key, "m-1", &[DB1_DATA], ""));
    drop(guard);

    assert_eq!(answer.state, EnrollState::Pending as i32);
    assert_eq!(w.standing(&key), (RequestState::Pending, None));
    assert_eq!(w.record("db-1").state(), RecordState::Open);
    assert_eq!(w.actions_after(before), ["enroll_requested"]);
    let text = log.text();
    assert!(
        text.contains("an inventory record did not approve its request")
            && text.contains("host_id_taken"),
        "{text}"
    );
    assert_eq!(w.shown(&key).record_state(), RecordState::Open);
}

/// Proves: an approval an operator makes does not use the record, and the one host per host id
/// keeps the record from approving a second key: a request that waited for lack of a BMC MAC is
/// approved by the operator, the record stays open and unused, and the request that then claims
/// everything the record holds is left pending, the host id being taken.
#[tokio::test]
async fn an_operator_approval_does_not_mark_the_record_used() {
    let w = world("by-record-manual");
    w.add(add_of("db-1", &[DB1_DATA], DB1_BMC));
    let silent = HostKey::new();
    w.post(claiming(&silent, "m-1", &[DB1_DATA], ""));
    assert_eq!(w.standing(&silent).0, RequestState::Pending);

    let approved = w
        .rig
        .coordinator
        .enrollment_approve(
            &operator(),
            proto::ApproveRequest {
                request_id: silent.key_hash().to_hex(),
                key_sha256: silent.key_hash().to_hex(),
                host_id: String::new(),
                role: "storage".to_owned(),
                services: Vec::new(),
                replace: false,
                reason: "checked by hand".to_owned(),
                host_group: String::new(),
            },
        )
        .expect("approved");
    assert_eq!(approved.host_id, "db-1");
    let record = w.record("db-1");
    assert_eq!(
        (
            record.state(),
            record.used_by_request_id.as_str(),
            record.used_unix
        ),
        (RecordState::Open, "", 0)
    );

    let exact = HostKey::new();
    // The MACs are now bound to the host the operator approved: a duplicate claim, not approved.
    let answer = w.post(claiming(&exact, "m-2", &[DB1_DATA], DB1_BMC));
    assert_eq!(answer.state, EnrollState::DuplicateClaim as i32);
    assert_eq!(w.standing(&exact), (RequestState::DuplicateClaim, None));
    assert_eq!(w.record("db-1").state(), RecordState::Open);
}
