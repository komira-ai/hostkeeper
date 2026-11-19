//! The expiry of waiting enrollment requests as one conditional write (`PgRecords::expire_waiting`;
//! `docs/design/coordinator-multi-instance.md`, section 9, job `request-expiry`): which requests
//! it takes, that two coordinators never both take one, and what it says when it cannot write.
//!
//! No test reads a clock: every call is given the `now` it expires at, and the races are
//! ordered by a barrier.

use std::collections::BTreeSet;
use std::sync::{Arc, Barrier};
use std::thread;

use hostkeeper_enrollment::enroll::records::Records;
use hostkeeper_enrollment::enroll::{
    Claims, EnrollmentStore, NewRequest, RequestState, RequestView,
};
use hostkeeper_enrollment::names::KeyHash;

use super::TestDatabase;
use crate::enrollment::tests::support::HostKey;
use crate::store::postgres::{PgRecords, Postgres};

/// The request TTL of these tests, in seconds.
const TTL: u64 = 72 * 3600;

/// When the first requests of a test were posted.
const POSTED: u64 = 1_800_000_000;

fn store_over(store: &Postgres) -> EnrollmentStore {
    EnrollmentStore::new(Arc::new(PgRecords::over(store).expect("the records open")))
}

/// Posts a request of a new key from `machine_id` at `at`.
fn post(store: &EnrollmentStore, machine_id: &str, at: u64) -> KeyHash {
    store
        .post(
            NewRequest {
                csr_der: HostKey::new().csr(),
                claims: Claims {
                    machine_id: machine_id.to_owned(),
                    ..Claims::default()
                },
                image: "hostkeeper-image-1".to_owned(),
                renewal_of: None,
                mac_nonce_presented: false,
            },
            at,
        )
        .expect("posted")
        .request_id
}

fn ids(views: &[RequestView]) -> Vec<KeyHash> {
    views.iter().map(|view| view.request_id).collect()
}

fn sorted(mut ids: Vec<KeyHash>) -> Vec<KeyHash> {
    ids.sort();
    ids
}

fn hex(id: &KeyHash) -> String {
    id.as_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

/// Sets the state of the request `id` straight in the table.
fn set_state(db: &TestDatabase, id: &KeyHash, state: i16) {
    db.run(&format!(
        "UPDATE enrollment_requests SET state = {state} WHERE request_id = '\\x{}'",
        hex(id)
    ))
    .expect("the state is set");
}

/// Proves: two coordinators expiring at once over one database return every overdue request
/// from exactly one of them, round after round, and each expired row is written once; a request
/// exactly at the TTL is not expired and one second past it is; approved, issued, denied and
/// already expired requests are left as they are; a duplicate claim expires like a pending
/// request.
///
/// Catches: an expiry that reads then writes (two coordinators then both audit one expiry), an
/// off-by-one at the TTL, and a write that touches a request no longer waiting.
#[test]
fn expiry_audits_each_request_once() {
    let db = TestDatabase::new();
    let (first, _) = db.open("first").expect("the first store opens");
    let (second, _) = db.open("second").expect("the second store opens");
    let (one, two) = (Arc::new(store_over(&first)), Arc::new(store_over(&second)));

    // The requests that must not expire, by the state they are in.
    let approved = post(&one, "m-approved", POSTED);
    let issued = post(&one, "m-issued", POSTED);
    let denied = post(&one, "m-denied", POSTED);
    set_state(&db, &approved, 3);
    set_state(&db, &issued, 4);
    set_state(&db, &denied, 5);
    let before =
        db.rows("SELECT request_id, state, expired_unix FROM enrollment_requests ORDER BY 1");

    // At the TTL, nothing waits too long; a second later the request does.
    let edge = post(&one, "m-edge", POSTED + 10);
    let now = POSTED + 10 + TTL;
    assert_eq!(two.expire(now, TTL).expect("a sweep"), []);
    assert_eq!(one.pending().expect("pending").len(), 1);
    let swept = two.expire(now + 1, TTL).expect("a sweep");
    assert_eq!(ids(&swept), [edge]);
    assert_eq!(swept[0].state, RequestState::Expired);
    assert_eq!(swept[0].expiry.map(|e| e.expired_unix), Some(now + 1));
    assert_eq!(swept[0].expiry.map(|e| e.ttl_secs), Some(TTL));
    assert_eq!(one.expire(now + 5, TTL).expect("a sweep"), [], "once");

    // Expired already: a later sweep under a shorter TTL leaves its marks alone.
    let marks = db.rows("SELECT expired_unix, expired_after_secs FROM enrollment_requests WHERE state = 1 AND expired_unix <> 0");
    assert_eq!(
        marks,
        vec![vec![Some((now + 1).to_string()), Some(TTL.to_string())]]
    );

    // The race: each round posts overdue requests (a duplicate claim among them) and both
    // coordinators sweep at once.
    for round in 0..4_u64 {
        let machine = format!("m-round-{round}");
        let mut overdue = vec![
            post(&one, &format!("{machine}-a"), POSTED + round),
            post(&one, &format!("{machine}-b"), POSTED + round),
            post(&one, &machine, POSTED + round),
        ];
        // The same machine id from a second key is a duplicate claim.
        overdue.push(post(&one, &machine, POSTED + round));
        let claims = db.rows(&format!(
            "SELECT count(*) FROM enrollment_requests WHERE state = 2 AND machine_id = '{machine}'"
        ));
        assert_eq!(
            claims,
            vec![vec![Some("1".to_owned())]],
            "a duplicate claim"
        );
        let at = POSTED + round + TTL + 1_000;
        let barrier = Arc::new(Barrier::new(2));
        let sweeps: Vec<_> = [one.clone(), two.clone()]
            .into_iter()
            .map(|store| {
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    store.expire(at, TTL).expect("a sweep")
                })
            })
            .collect();
        let got: Vec<Vec<KeyHash>> = sweeps
            .into_iter()
            .map(|sweep| ids(&sweep.join().expect("the sweep ran")))
            .collect();
        let all: Vec<KeyHash> = got.concat();
        assert_eq!(
            sorted(all.clone()),
            sorted(overdue),
            "round {round}: every overdue request is returned, once"
        );
        assert_eq!(
            all.iter().collect::<BTreeSet<_>>().len(),
            all.len(),
            "round {round}: none twice"
        );
    }

    // The requests that were not waiting are as they were.
    let after = db.rows(
        "SELECT request_id, state, expired_unix FROM enrollment_requests \
         WHERE state IN (3, 4, 5) ORDER BY 1",
    );
    let kept: Vec<_> = before
        .into_iter()
        .filter(|row| matches!(row[1].as_deref(), Some("3" | "4" | "5")))
        .collect();
    assert_eq!(after, kept);
    assert_eq!(one.pending().expect("pending"), []);
    first.stop();
    second.stop();
}

/// Proves: a `now` or a TTL above what the database keeps is refused with the text that names
/// `enrollment_requests` and the counter, and nothing is written.
///
/// Catches: a counter wrapped to a negative number, which would expire every request.
#[test]
fn expiry_refuses_a_time_the_database_cannot_hold() {
    let db = TestDatabase::new();
    let (store, _) = db.open("expiry").expect("the store opens");
    let enrollment = store_over(&store);
    let waiting = post(&enrollment, "m-1", POSTED);
    let records = PgRecords::over(&store).expect("the records open");
    for (now, ttl) in [(u64::MAX, TTL), (POSTED + TTL + 1, u64::MAX)] {
        let error = records.expire_waiting(now, ttl).expect_err("refused");
        assert_eq!(
            error.to_string(),
            format!(
                "database:enrollment_requests is not a store file this release reads: {} is above the largest counter the database keeps",
                u64::MAX
            )
        );
    }
    assert_eq!(ids(&enrollment.pending().expect("pending")), [waiting]);
    store.stop();
}

/// Proves: an expiry over a lost store is an error that names `enrollment_requests`, and the
/// request is not marked expired.
///
/// Catches: a lost write reported as an empty sweep, so that no expiry is ever audited.
#[test]
fn expiry_over_a_lost_store_is_an_error() {
    let db = TestDatabase::new();
    let (store, _) = db.open("lost").expect("the store opens");
    let enrollment = store_over(&store);
    let _waiting = post(&enrollment, "m-1", POSTED);
    let records = PgRecords::over(&store).expect("the records open");
    store.stop();
    let error = records
        .expire_waiting(POSTED + TTL + 1, TTL)
        .expect_err("a stopped store cannot write");
    assert!(error.to_string().contains("enrollment_requests"), "{error}");
    assert_eq!(
        db.rows("SELECT count(*) FROM enrollment_requests WHERE expired_unix <> 0"),
        vec![vec![Some("0".to_owned())]]
    );
}

/// Proves: a waiting row whose id is not a 32-byte key hash makes the expiry fail with the
/// unkeyed-record error instead of returning a request under no id.
///
/// Catches: an overdue row with a malformed id silently skipped, so it would expire without an
/// audit line.
#[test]
fn expiry_refuses_a_waiting_row_without_a_key_hash() {
    let db = TestDatabase::new();
    let (store, _) = db.open("unkeyed").expect("the store opens");
    let enrollment = store_over(&store);
    let _waiting = post(&enrollment, "m-1", POSTED);
    db.run(
        "INSERT INTO enrollment_requests (request_id, state, posted_unix, round, image, csr_der, \
         spki_der, mac_nonce_presented, machine_id, dmi_serial, dmi_product_uuid, addresses, \
         mac_serial, mac_platform_uuid, bmc_mac) \
         VALUES ('\\x0102', 1, 5, 0, '', '\\x', '\\x', false, 'm-short', '', '', '{}', '', '', '')",
    )
    .expect("the row is made");
    let records = PgRecords::over(&store).expect("the records open");
    let error = records
        .expire_waiting(POSTED + TTL + 1, TTL)
        .expect_err("an unkeyed row");
    assert_eq!(
        error.to_string(),
        "a request record whose key does not parse is not kept"
    );
    store.stop();
}
