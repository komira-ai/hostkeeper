//! Contested records (network-install design section 7): two pending requests that match one
//! inventory record are each listed `contested_by` the others, and `Enrollment.Approve` of any of
//! them is refused `record_contested`, replace or not, until all but one are rejected. A request
//! that matches several records contests the requests of each; a request that is no longer
//! pending contests nothing; the list names the contest whatever page it shows; and a store that
//! cannot be read is `store_failed`, not an uncontested request. The coordinators here have only
//! closed approval windows ([`super::support::closed_windows`]), so no record approves a request
//! by itself.

use std::sync::Arc;

use hostkeeper_enrollment::enroll::RequestState;
use hostkeeper_enrollment::fs::FaultFs;
use hostkeeper_proto::hostkeeper::agent::v1::{EnrollResponse, EnrollState};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use super::support::{
    HostKey, Rig, approve, audit_after, audit_of, audited, closed_windows, enroll_request,
    mac_request,
};
use crate::enrollment::os_random;
use crate::marks_support::operator;
use crate::testutil::{Log, Rig as CoordinatorRig};

use proto::EnrollmentRefusal as Refusal;
use proto::InventoryMatch as Match;

/// The data MAC of the record `db-1`.
const DB1_DATA: &str = "0c:c4:7a:00:00:02";

/// The BMC MAC of the record `db-1`.
const DB1_BMC: &str = "0c:c4:7a:00:00:01";

/// The data MAC of the record `db-2`.
const DB2_DATA: &str = "0c:c4:7a:00:00:12";

/// A coordinator over the enrollment of `enrollment`, whose records' windows are closed
/// ([`closed_windows`]): a matching request waits for an operator here.
fn coordinator(name: &str, enrollment: &Rig) -> CoordinatorRig {
    closed_windows(name, enrollment)
}

/// Records `host` (role `storage`) with the data MAC `data`, the BMC MAC `bmc` and the serial
/// `serial` (each empty for none).
fn record(rig: &CoordinatorRig, host: &str, data: &str, bmc: &str, serial: &str) {
    rig.coordinator
        .add_record(
            &operator(),
            proto::AddRecordRequest {
                host_id: host.to_owned(),
                role: "storage".to_owned(),
                data_macs: [data]
                    .into_iter()
                    .filter(|m| !m.is_empty())
                    .map(str::to_owned)
                    .collect(),
                bmc_mac: bmc.to_owned(),
                serial: serial.to_owned(),
                ..proto::AddRecordRequest::default()
            },
        )
        .expect("recorded");
}

/// Posts the request of `key` from machine `machine` claiming the addresses `addresses` and no
/// BMC MAC.
fn post(enrollment: &Rig, key: &HostKey, machine: &str, addresses: &[&str]) -> EnrollResponse {
    let mut request = enroll_request(key, machine);
    request.claims.as_mut().expect("claims").addresses =
        addresses.iter().map(|a| (*a).to_owned()).collect();
    enrollment.enrollment.enroll(request).expect("posted")
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
    }
}

/// Rejects `key`'s request.
fn reject(rig: &CoordinatorRig, key: &HostKey) {
    rig.coordinator
        .enrollment_reject(
            &operator(),
            &proto::RejectRequest {
                request_id: key.key_hash().to_hex(),
                reason: "not the machine on the console".to_owned(),
            },
        )
        .expect("rejected");
}

/// The pending list, one page of `limit` (0 for the cap) after `page_token`.
fn page(rig: &CoordinatorRig, limit: u32, page_token: &str) -> proto::ListPendingResponse {
    rig.coordinator
        .enrollment_list(
            &operator(),
            &proto::ListPendingRequest {
                limit,
                page_token: page_token.to_owned(),
                unenrolled: false,
            },
        )
        .expect("listed")
}

/// The one listed request of `key`.
fn shown(rig: &CoordinatorRig, key: &HostKey) -> proto::PendingEnrollment {
    let hex = key.key_hash().to_hex();
    let mut found = page(rig, 0, "")
        .requests
        .into_iter()
        .filter(|p| p.request_id == hex);
    let one = found.next().expect("listed");
    assert!(found.next().is_none());
    one
}

/// The request ids of `keys`, sorted, as `contested_by` lists them.
fn ids(keys: &[&HostKey]) -> Vec<String> {
    let mut ids: Vec<String> = keys.iter().map(|k| k.key_hash().to_hex()).collect();
    ids.sort();
    ids
}

/// Where `key`'s request stands.
fn state(enrollment: &Rig, key: &HostKey) -> RequestState {
    enrollment
        .enrollment
        .store()
        .request(&key.key_hash())
        .expect("read")
        .expect("held")
        .state
}

/// Catches: two requests matching one record listed as plain `matched` rows, a contest that
/// names only one side, or names the request itself; a request of another record, or of none,
/// marked contested; and the record's serial and BMC MAC left off a matched request (the
/// hard-to-copy values the approval screen puts beside the claims).
#[test]
fn two_pending_requests_that_match_one_record_each_name_the_other() {
    let enrollment = Rig::new("contest-list");
    let rig = coordinator("contest-list-coordinator", &enrollment);
    record(&rig, "db-1", DB1_DATA, DB1_BMC, "SN-1");
    record(&rig, "db-2", DB2_DATA, "", "");
    let (real, copy, other, stray) = (
        HostKey::new(),
        HostKey::new(),
        HostKey::new(),
        HostKey::new(),
    );
    post(&enrollment, &real, "m-real", &[DB1_DATA]);
    post(&enrollment, &copy, "m-copy", &["0C-C4-7A-00-00-02"]);
    post(&enrollment, &other, "m-other", &[DB2_DATA]);
    post(&enrollment, &stray, "m-stray", &["0c:c4:7a:00:00:99"]);

    let (real_view, copy_view) = (shown(&rig, &real), shown(&rig, &copy));
    assert_eq!(real_view.contested_by, ids(&[&copy]));
    assert_eq!(copy_view.contested_by, ids(&[&real]));
    for view in [&real_view, &copy_view] {
        assert_eq!(
            (
                view.inventory_match(),
                view.record_host_id.as_str(),
                view.record_serial.as_str(),
                view.record_bmc_mac.as_str()
            ),
            (Match::Matched, "db-1", "SN-1", DB1_BMC)
        );
    }
    let other_view = shown(&rig, &other);
    assert_eq!(other_view.contested_by, Vec::<String>::new());
    assert_eq!(
        (
            other_view.record_host_id.as_str(),
            other_view.record_serial.as_str(),
            other_view.record_bmc_mac.as_str()
        ),
        ("db-2", "", "")
    );
    let stray_view = shown(&rig, &stray);
    assert_eq!(stray_view.inventory_match(), Match::Unmatched);
    assert_eq!(stray_view.contested_by, Vec::<String>::new());
}

/// Catches: a contested request approved by a hurried click (with or without a host id, with or
/// without `replace`), a refusal under another token, one that is not audited or changes the
/// request; and a contest that outlives the rejection of the other request, so the real machine
/// cannot be approved.
#[test]
fn a_contested_request_is_refused_until_the_other_is_rejected() {
    let enrollment = Rig::new("contest-approve");
    let rig = coordinator("contest-approve-coordinator", &enrollment);
    record(&rig, "db-1", DB1_DATA, "", "");
    let (real, copy) = (HostKey::new(), HostKey::new());
    post(&enrollment, &copy, "m-copy", &[DB1_DATA]);
    post(&enrollment, &real, "m-real", &[DB1_DATA]);
    let before = audit_of(&rig.coordinator).len();

    for (key, host, replace) in [
        (&copy, "", false),
        (&real, "", false),
        (&real, "db-1", false),
        (&real, "db-1", true),
        (&real, "db-9", true),
    ] {
        assert_eq!(
            rig.coordinator
                .enrollment_approve(&operator(), approval(key, host, replace)),
            Err(Refusal::RecordContested),
            "{host} {replace}"
        );
    }
    assert_eq!(
        (state(&enrollment, &real), state(&enrollment, &copy)),
        (RequestState::Pending, RequestState::Pending)
    );
    let refused = |key: &HostKey, host: Option<&str>| {
        audited(
            "operator",
            "enroll_approve_refused",
            host,
            &format!("record_contested; request {}", key.key_hash()),
        )
    };
    assert_eq!(
        audit_after(&rig.coordinator, before),
        [
            refused(&copy, None),
            refused(&real, None),
            refused(&real, Some("db-1")),
            refused(&real, Some("db-1")),
            refused(&real, Some("db-9")),
        ]
    );

    reject(&rig, &copy);
    assert_eq!(shown(&rig, &real).contested_by, Vec::<String>::new());
    let approved = rig
        .coordinator
        .enrollment_approve(&operator(), approval(&real, "", false))
        .expect("approved once the other is rejected");
    assert_eq!(approved.host_id, "db-1");
    assert_eq!(state(&enrollment, &real), RequestState::Approved);
    assert_eq!(state(&enrollment, &copy), RequestState::Denied);
}

/// Catches: a contest that ends when one of several others is rejected (all but one must be),
/// and a contest that names only some of the others.
#[test]
fn three_requests_on_one_record_stay_contested_until_all_but_one_are_rejected() {
    let enrollment = Rig::new("contest-three");
    let rig = coordinator("contest-three-coordinator", &enrollment);
    record(&rig, "db-1", DB1_DATA, "", "");
    let keys = [HostKey::new(), HostKey::new(), HostKey::new()];
    for (n, key) in keys.iter().enumerate() {
        post(&enrollment, key, &format!("m-{n}"), &[DB1_DATA]);
    }
    let [real, first, second] = &keys;
    assert_eq!(shown(&rig, real).contested_by, ids(&[first, second]));
    assert_eq!(shown(&rig, first).contested_by, ids(&[real, second]));

    reject(&rig, first);
    assert_eq!(shown(&rig, real).contested_by, ids(&[second]));
    assert_eq!(
        rig.coordinator
            .enrollment_approve(&operator(), approval(real, "", false)),
        Err(Refusal::RecordContested)
    );

    reject(&rig, second);
    assert_eq!(
        rig.coordinator
            .enrollment_approve(&operator(), approval(real, "", false))
            .expect("approved")
            .host_id,
        "db-1"
    );
}

/// Catches: a request that matches several records (ambiguous) left out of the contest of each,
/// so a request of one of them is approved while the ambiguous one stands; and two requests of
/// different records contesting each other through it.
#[test]
fn a_request_that_matches_two_records_contests_the_requests_of_each() {
    let enrollment = Rig::new("contest-ambiguous");
    let rig = coordinator("contest-ambiguous-coordinator", &enrollment);
    record(&rig, "db-1", DB1_DATA, "", "");
    record(&rig, "db-2", DB2_DATA, "", "");
    let (one, two, both) = (HostKey::new(), HostKey::new(), HostKey::new());
    post(&enrollment, &one, "m-1", &[DB1_DATA]);
    post(&enrollment, &two, "m-2", &[DB2_DATA]);
    post(&enrollment, &both, "m-both", &[DB1_DATA, DB2_DATA]);

    assert_eq!(shown(&rig, &both).inventory_match(), Match::Ambiguous);
    assert_eq!(shown(&rig, &both).contested_by, ids(&[&one, &two]));
    assert_eq!(shown(&rig, &one).contested_by, ids(&[&both]));
    assert_eq!(shown(&rig, &two).contested_by, ids(&[&both]));
    for (key, host) in [(&one, ""), (&both, "db-3")] {
        assert_eq!(
            rig.coordinator
                .enrollment_approve(&operator(), approval(key, host, false)),
            Err(Refusal::RecordContested)
        );
    }
}

/// Catches: the contest computed over the page shown rather than every pending request, so a
/// page of one request shows it uncontested.
#[test]
fn the_contest_is_named_on_every_page() {
    let enrollment = Rig::new("contest-pages");
    let rig = coordinator("contest-pages-coordinator", &enrollment);
    record(&rig, "db-1", DB1_DATA, "", "");
    let (first, second) = (HostKey::new(), HostKey::new());
    post(&enrollment, &first, "m-1", &[DB1_DATA]);
    post(&enrollment, &second, "m-2", &[DB1_DATA]);

    let one = page(&rig, 1, "");
    assert_eq!(one.requests.len(), 1);
    let two = page(&rig, 1, &one.next_page_token);
    assert_eq!(two.requests.len(), 1);
    for (shown, other) in [
        (&one.requests[0], &two.requests[0]),
        (&two.requests[0], &one.requests[0]),
    ] {
        assert_eq!(shown.contested_by, std::slice::from_ref(&other.request_id));
    }
}

/// Catches: a request already approved counted as a contester, so the next request of its
/// record (a reinstall, whose MACs make it a duplicate claim) is refused for ever; and two
/// duplicate claims of one record not contesting each other, so `replace` takes the host for
/// the request that posted first.
#[test]
fn only_pending_requests_contest_and_duplicate_claims_are_pending() {
    let enrollment = Rig::new("contest-duplicate");
    let rig = coordinator("contest-duplicate-coordinator", &enrollment);
    record(&rig, "db-1", DB1_DATA, "", "");
    let installed = HostKey::new();
    post(&enrollment, &installed, "m-1", &[DB1_DATA]);
    approve(&enrollment.enrollment, &installed, "db-1", false);

    let (copy, reinstalled) = (HostKey::new(), HostKey::new());
    let posted = post(&enrollment, &copy, "m-copy", &[DB1_DATA]);
    assert_eq!(posted.state, EnrollState::DuplicateClaim as i32);
    assert_eq!(shown(&rig, &copy).contested_by, Vec::<String>::new());
    let posted = post(&enrollment, &reinstalled, "m-1", &[DB1_DATA]);
    assert_eq!(posted.state, EnrollState::DuplicateClaim as i32);
    assert_eq!(shown(&rig, &copy).contested_by, ids(&[&reinstalled]));
    assert_eq!(shown(&rig, &reinstalled).contested_by, ids(&[&copy]));
    assert_eq!(
        rig.coordinator
            .enrollment_approve(&operator(), approval(&copy, "", true)),
        Err(Refusal::RecordContested)
    );

    reject(&rig, &copy);
    let approved = rig
        .coordinator
        .enrollment_approve(&operator(), approval(&reinstalled, "", true))
        .expect("the reinstall is approved with replace");
    assert_eq!(approved.host_id, "db-1");
}

/// Catches: Mac requests left out of the contest: two requests claiming one Mac serial match its
/// record by serial and contest it as Linux requests contest a data MAC.
#[test]
fn two_mac_requests_with_one_serial_contest_its_record() {
    let enrollment = Rig::new("contest-mac");
    let rig = coordinator("contest-mac-coordinator", &enrollment);
    record(&rig, "mac-1", "", "", "C02XK1ABJG5J");
    let (real, copy) = (HostKey::new(), HostKey::new());
    for (key, machine, serial) in [
        (&real, "m-1", "C02XK1ABJG5J"),
        (&copy, "m-2", "c02xk1abjg5j"),
    ] {
        enrollment
            .enrollment
            .enroll(mac_request(key, machine, serial, &[]))
            .expect("posted");
    }
    assert_eq!(shown(&rig, &real).contested_by, ids(&[&copy]));
    assert_eq!(
        rig.coordinator
            .enrollment_approve(&operator(), approval(&copy, "", false)),
        Err(Refusal::RecordContested)
    );
}

/// Catches: a store that cannot be read while the contest is looked for answered as an
/// uncontested request (approved) or an uncontested list: both are `store_failed`, the refused
/// approval is audited, and the request stays pending.
#[test]
fn a_contest_the_store_cannot_read_is_store_failed() {
    let fs = Arc::new(FaultFs::new());
    let enrollment = Rig::over("contest-failing", fs.clone(), os_random());
    let rig = coordinator("contest-failing-coordinator", &enrollment);
    record(&rig, "db-1", DB1_DATA, "", "");
    let key = HostKey::new();
    post(&enrollment, &key, "m-1", &[DB1_DATA]);
    // A list is the page's list of the requests' directory and read of the one request, then
    // the contest's same two.
    let at = fs.count();
    page(&rig, 0, "");
    assert_eq!(fs.count() - at, 4);
    let before = audit_of(&rig.coordinator).len();
    let log = Log::default();
    let guard = log.install();

    // The read of the request (1), then the contest's list of the requests' directory (2).
    fs.fail_at(fs.count() + 2);
    assert_eq!(
        rig.coordinator
            .enrollment_approve(&operator(), approval(&key, "", false)),
        Err(Refusal::StoreFailed)
    );
    let approving = log.text();
    // The contest's list of the directory, after the page's two operations.
    fs.fail_at(fs.count() + 3);
    assert_eq!(
        rig.coordinator
            .enrollment_list(&operator(), &proto::ListPendingRequest::default()),
        Err(Refusal::StoreFailed)
    );
    drop(guard);

    assert!(
        approving.contains("cannot read the enrollment requests")
            && !approving.contains("cannot read a pending request")
            && !approving.contains("an approval could not be written"),
        "{approving}"
    );
    assert_eq!(
        audit_after(&rig.coordinator, before),
        [audited(
            "operator",
            "enroll_approve_refused",
            None,
            &format!("store_failed; request {}", key.key_hash())
        )]
    );
    assert_eq!(state(&enrollment, &key), RequestState::Pending);
    assert_eq!(shown(&rig, &key).contested_by, Vec::<String>::new());
}
