//! What leaves a request to an operator when a record could approve it: each condition of
//! [`Enrollment::approve_by_record`], tried against records built by hand (the ops file may be
//! edited), and a store that cannot be read. Nothing is audited, the request stays pending and
//! the record untouched in each.

use std::collections::BTreeSet;
use std::sync::Arc;

use hostkeeper_enrollment::enroll::RequestState;
use hostkeeper_enrollment::fs::FaultFs;
use hostkeeper_enrollment::names::KeyHash;
use hostkeeper_proto::hostkeeper::operator::v1::EnrollmentRefusal;

use super::support::{HostKey, NOW, Rig, claiming, enrolled, host_id};
use crate::enrollment::{ByRecord, Waits, os_random};
use crate::inventory::{InventoryRecord, RecordUse};

/// The BMC MAC of the records of the tests.
const BMC: &str = "0c:c4:7a:00:00:01";

/// A record of `host` with the data MAC `data`, a storage host whose window is open until an hour
/// after [`NOW`] and which no approval used.
fn open_record(host: &str, data: &str) -> InventoryRecord {
    InventoryRecord {
        host_group: String::new(),
        host_id: host.to_owned(),
        role: "storage".to_owned(),
        data_macs: vec![data.to_owned()],
        bmc_mac: String::new(),
        serial: String::new(),
        notes: String::new(),
        by: "operator".to_owned(),
        at_unix: NOW,
        platform: None,
        approve_until_unix: NOW + 3600,
        used: None,
    }
}

/// Posts a request of a new key from machine `machine` claiming the data MAC `data` and the BMC
/// MAC `bmc` (empty for none), and gives the key.
fn post(rig: &Rig, machine: &str, data: &str, bmc: &str) -> HostKey {
    let key = HostKey::new();
    rig.enrollment
        .enroll(claiming(&key, machine, &[data], bmc))
        .expect("posted");
    key
}

fn state_of(rig: &Rig, key: &HostKey) -> RequestState {
    rig.enrollment
        .store()
        .request(&key.key_hash())
        .expect("read")
        .expect("held")
        .state
}

/// Proves: each condition of the approval by a record, when it is false, leaves the request
/// pending and says which it was: the record is another's (no match), is used, names no role, has
/// closed (at its end exactly), holds a host id or a role that breaks its rule, or holds a BMC
/// MAC the request does not claim; and a record that holds every condition approves, as its host
/// id.
#[test]
fn each_condition_that_fails_leaves_the_request_pending_and_says_which() {
    let rig = Rig::new("by-record-conditions");
    let cases: Vec<(&str, InventoryRecord, &str, &str, ByRecord)> = vec![
        (
            "the record holds another MAC",
            open_record("db-1", "0c:c4:7a:00:00:99"),
            "0c:c4:7a:00:01:01",
            "",
            ByRecord::Waits(Waits::Unmatched),
        ),
        (
            "used",
            InventoryRecord {
                used: Some(RecordUse {
                    request_id: "0".repeat(64),
                    at_unix: NOW - 1,
                }),
                ..open_record("db-2", "0c:c4:7a:00:01:02")
            },
            "0c:c4:7a:00:01:02",
            "",
            ByRecord::Waits(Waits::Used),
        ),
        (
            "no role",
            InventoryRecord {
                role: String::new(),
                ..open_record("db-3", "0c:c4:7a:00:01:03")
            },
            "0c:c4:7a:00:01:03",
            "",
            ByRecord::Waits(Waits::NoRole),
        ),
        (
            "no window",
            InventoryRecord {
                approve_until_unix: 0,
                ..open_record("db-4", "0c:c4:7a:00:01:04")
            },
            "0c:c4:7a:00:01:04",
            "",
            ByRecord::Waits(Waits::Closed),
        ),
        (
            "closed at its end",
            InventoryRecord {
                approve_until_unix: NOW,
                ..open_record("db-5", "0c:c4:7a:00:01:05")
            },
            "0c:c4:7a:00:01:05",
            "",
            ByRecord::Waits(Waits::Closed),
        ),
        (
            "a host id that breaks its rule",
            open_record("Not_A_Host", "0c:c4:7a:00:01:06"),
            "0c:c4:7a:00:01:06",
            "",
            ByRecord::Waits(Waits::BadRecord),
        ),
        (
            "a role that breaks its rule",
            InventoryRecord {
                role: "9lives".to_owned(),
                ..open_record("db-7", "0c:c4:7a:00:01:07")
            },
            "0c:c4:7a:00:01:07",
            "",
            ByRecord::Waits(Waits::BadRecord),
        ),
        (
            "the request claims no BMC MAC the record holds",
            InventoryRecord {
                bmc_mac: BMC.to_owned(),
                ..open_record("db-8", "0c:c4:7a:00:01:08")
            },
            "0c:c4:7a:00:01:08",
            "",
            ByRecord::Waits(Waits::Inexact),
        ),
    ];
    for (why, record, data, bmc, waits) in cases {
        let key = post(&rig, why, data, bmc);
        let before = rig
            .enrollment
            .store()
            .recorded_approvals()
            .expect("read")
            .len();

        let done = rig
            .enrollment
            .approve_by_record(&key.key_hash(), std::slice::from_ref(&record));

        assert_eq!(done, waits, "{why}");
        assert_eq!(state_of(&rig, &key), RequestState::Pending, "{why}");
        assert_eq!(
            rig.enrollment
                .store()
                .recorded_approvals()
                .expect("read")
                .len(),
            before,
            "{why}"
        );
    }

    let key = post(&rig, "m-ok", "0c:c4:7a:01:ff:01", BMC);
    let record = InventoryRecord {
        bmc_mac: BMC.to_owned(),
        ..open_record("db-ok", "0c:c4:7a:01:ff:01")
    };
    assert_eq!(
        rig.enrollment
            .approve_by_record(&key.key_hash(), std::slice::from_ref(&record)),
        ByRecord::Approved {
            host_id: host_id("db-ok"),
            entry: None
        }
    );
    assert_eq!(state_of(&rig, &key), RequestState::Approved);
}

/// Proves: more than one record matching a request is ambiguous, and a request the store does
/// not hold, one already approved, a renewal and a duplicate claim each wait with the reason of
/// their own; none of them is approved by an open record that matches it.
#[test]
fn an_ambiguous_match_and_a_request_that_is_not_plainly_pending_wait() {
    let rig = Rig::new("by-record-states");
    let both = HostKey::new();
    rig.enrollment
        .enroll(claiming(
            &both,
            "m-both",
            &["0c:c4:7a:02:00:01", "0c:c4:7a:02:00:02"],
            "",
        ))
        .expect("posted");
    let records = [
        open_record("db-1", "0c:c4:7a:02:00:01"),
        open_record("db-2", "0c:c4:7a:02:00:02"),
    ];
    assert_eq!(
        rig.enrollment.approve_by_record(&both.key_hash(), &records),
        ByRecord::Waits(Waits::Ambiguous)
    );

    let unknown = HostKey::new().key_hash();
    assert_eq!(
        rig.enrollment.approve_by_record(&unknown, &records),
        ByRecord::Waits(Waits::NotPending)
    );

    let approved = post(&rig, "m-approved", "0c:c4:7a:02:00:03", "");
    super::support::approve(&rig.enrollment, &approved, "s-approved", false);
    assert_eq!(
        rig.enrollment.approve_by_record(
            &approved.key_hash(),
            &[open_record("db-3", "0c:c4:7a:02:00:03")]
        ),
        ByRecord::Waits(Waits::NotPending)
    );

    let renewing = HostKey::new();
    enrolled(&rig.enrollment, &renewing, "m-renew", "s6");
    let mut renewal = claiming(&renewing, "m-renew", &["0c:c4:7a:02:00:04"], "");
    renewal.renewal_of = "s6".to_owned();
    rig.enrollment.enroll(renewal).expect("a renewal");
    assert_eq!(
        rig.enrollment.approve_by_record(
            &renewing.key_hash(),
            &[open_record("db-4", "0c:c4:7a:02:00:04")]
        ),
        ByRecord::Waits(Waits::Renewal)
    );

    let holder = post(&rig, "m-holder", "0c:c4:7a:02:00:05", "");
    super::support::approve(&rig.enrollment, &holder, "s-holder", false);
    let copy = post(&rig, "m-copy", "0c:c4:7a:02:00:05", "");
    assert_eq!(state_of(&rig, &copy), RequestState::DuplicateClaim);
    assert_eq!(
        rig.enrollment.approve_by_record(
            &copy.key_hash(),
            &[open_record("db-5", "0c:c4:7a:02:00:05")]
        ),
        ByRecord::Waits(Waits::DuplicateClaim)
    );
}

/// Proves: two pending requests that match one record contest it, and neither is approved by it.
#[test]
fn a_contested_record_approves_neither_of_its_requests() {
    let rig = Rig::new("by-record-contest");
    let first = post(&rig, "m-1", "0c:c4:7a:03:00:01", "");
    let second = post(&rig, "m-2", "0c:c4:7a:03:00:01", "");
    let records = [open_record("db-1", "0c:c4:7a:03:00:01")];

    for key in [&first, &second] {
        assert_eq!(
            rig.enrollment.approve_by_record(&key.key_hash(), &records),
            ByRecord::Waits(Waits::Contested)
        );
        assert_eq!(state_of(&rig, key), RequestState::Pending);
    }
}

/// Proves: the store's checks under its lock are the operator's: a host id another key holds
/// leaves the request pending as `host_id_taken`, and a denied one as `denied`; the refusal is
/// the outcome and nothing is approved.
#[test]
fn a_refusal_of_the_store_leaves_the_request_pending_with_its_refusal() {
    let rig = Rig::new("by-record-refused");
    let holder = HostKey::new();
    enrolled(&rig.enrollment, &holder, "m-holder", "db-1");
    let taken = post(&rig, "m-1", "0c:c4:7a:04:00:01", "");
    assert_eq!(
        rig.enrollment.approve_by_record(
            &taken.key_hash(),
            &[open_record("db-1", "0c:c4:7a:04:00:01")]
        ),
        ByRecord::Waits(Waits::Refused(EnrollmentRefusal::HostIdTaken))
    );

    rig.enrollment
        .store()
        .deny(&host_id("db-2"))
        .expect("denied");
    let denied = post(&rig, "m-2", "0c:c4:7a:04:00:02", "");
    assert_eq!(
        rig.enrollment.approve_by_record(
            &denied.key_hash(),
            &[open_record("db-2", "0c:c4:7a:04:00:02")]
        ),
        ByRecord::Waits(Waits::Refused(EnrollmentRefusal::Denied))
    );
    assert_eq!(state_of(&rig, &taken), RequestState::Pending);
    assert_eq!(state_of(&rig, &denied), RequestState::Pending);
}

/// Proves: a store that fails at any one operation of the approval never leaves it half done:
/// every operation of a successful approval is made to fail in turn, and each time the outcome is
/// the request unreadable or the store refusing (`store_failed`); the request is pending, or, when
/// the failing operation came after the write that approved it, approved, and a second try
/// finds it so (`not_pending`) or approves it.
#[test]
fn a_store_that_fails_at_any_operation_leaves_the_request_pending() {
    let operations = {
        let fs = Arc::new(FaultFs::new());
        let rig = Rig::over("by-record-count", fs.clone(), os_random());
        let key = post(&rig, "m-1", "0c:c4:7a:05:00:01", "");
        let at = fs.count();
        let done = rig
            .enrollment
            .approve_by_record(&key.key_hash(), &[open_record("db-1", "0c:c4:7a:05:00:01")]);
        assert!(matches!(done, ByRecord::Approved { .. }), "{done:?}");
        fs.count() - at
    };
    assert!(operations >= 4, "{operations}");
    let mut outcomes = BTreeSet::new();
    for n in 1..=operations {
        let fs = Arc::new(FaultFs::new());
        let rig = Rig::over(&format!("by-record-failing-{n}"), fs.clone(), os_random());
        let key = post(&rig, "m-1", "0c:c4:7a:05:00:01", "");
        let records = [open_record("db-1", "0c:c4:7a:05:00:01")];

        fs.fail_at(fs.count() + n);
        let done = rig.enrollment.approve_by_record(&key.key_hash(), &records);

        let ByRecord::Waits(waits) = done else {
            panic!("operation {n} failed and the request was approved: {done:?}");
        };
        outcomes.insert(format!("{waits:?}"));
        let state = state_of(&rig, &key);
        let again = rig.enrollment.approve_by_record(&key.key_hash(), &records);
        match state {
            RequestState::Pending => {
                assert!(matches!(again, ByRecord::Approved { .. }), "{n}: {again:?}");
            }
            state => {
                assert_eq!(state, RequestState::Approved, "operation {n}");
                assert_eq!(again, ByRecord::Waits(Waits::NotPending), "operation {n}");
            }
        }
    }
    assert_eq!(
        outcomes,
        BTreeSet::from(["Refused(StoreFailed)".to_owned(), "Unreadable".to_owned()])
    );
}

/// Proves: trying the requests that match one record reaches those, in the store's order, and no
/// other: a request that matches another record is left alone, and a store whose requests cannot
/// be listed tries none.
#[test]
fn only_the_requests_that_match_the_record_are_tried() {
    let fs = Arc::new(FaultFs::new());
    let rig = Rig::over("by-record-matching", fs.clone(), os_random());
    let mine = post(&rig, "m-1", "0c:c4:7a:06:00:01", "");
    let other = post(&rig, "m-2", "0c:c4:7a:06:00:02", "");
    let records = [
        open_record("db-1", "0c:c4:7a:06:00:01"),
        open_record("db-2", "0c:c4:7a:06:00:02"),
    ];

    fs.fail_at(fs.count() + 1);
    assert_eq!(
        rig.enrollment.approve_matching("db-1", &records),
        Vec::new()
    );
    assert_eq!(state_of(&rig, &mine), RequestState::Pending);

    let tried = rig.enrollment.approve_matching("db-1", &records);
    let ids: Vec<KeyHash> = tried.iter().map(|(id, _)| *id).collect();
    assert_eq!(ids, [mine.key_hash()]);
    assert!(matches!(tried[0].1, ByRecord::Approved { .. }), "{tried:?}");
    assert_eq!(state_of(&rig, &mine), RequestState::Approved);
    assert_eq!(state_of(&rig, &other), RequestState::Pending);
}
