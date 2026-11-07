//! The enrollment records' refusals: what they will not write, and what they say when the
//! database cannot be read.

use hostkeeper_enrollment::enroll::records::{Record, Records, pb};
use hostkeeper_enrollment::names::{HostId, KeyHash};

use super::TestDatabase;
use crate::store::Backend;
use crate::store::postgres::writer::Fault;
use crate::store::postgres::{PgRecords, Postgres};

fn open(db: &TestDatabase) -> (Postgres, PgRecords) {
    let (store, _) = db.open("records").expect("the store opens");
    let records = PgRecords::over(&store).expect("the records open");
    (store, records)
}

fn host(id: &str) -> pb::Host {
    pb::Host {
        host_id: id.to_owned(),
        key_hash: vec![7; 32],
        machine_id: "m".to_owned(),
        role: "storage".to_owned(),
        ..pb::Host::default()
    }
}

fn host_id(id: &str) -> HostId {
    HostId::parse(id).expect("a host id")
}

fn request(id: u8) -> pb::Request {
    pb::Request {
        request_id: vec![id; 32],
        ..pb::Request::default()
    }
}

/// Proves: a record with a counter above what the database keeps, or a state that does not fit
/// its column, is refused at the write with the text that names the table and the counter, and
/// nothing of the commit is written.
///
/// Catches: a counter written wrapped, and a half commit of the records before the refused one.
#[test]
fn a_record_the_database_cannot_hold_is_refused_whole() {
    super::logging();
    let db = TestDatabase::new();
    let (_store, records) = open(&db);
    let over = "is above the largest counter the database keeps";
    let cases: Vec<(&str, Record)> = vec![
        (
            "enrollment_requests",
            Record::Request(pb::Request {
                expired_unix: u64::MAX,
                ..request(1)
            }),
        ),
        (
            "enrollment_requests",
            Record::Request(pb::Request {
                approved: Some(pb::Approval {
                    approved_unix: u64::MAX,
                    ..pb::Approval::default()
                }),
                ..request(1)
            }),
        ),
        (
            "enrollment_hosts",
            Record::Host(pb::Host {
                first_heartbeat_unix: Some(u64::MAX),
                ..host("h1")
            }),
        ),
        (
            "mac_nonces",
            Record::MacNonce(pb::MacNonce {
                nonce_sha256: vec![3; 32],
                issued_unix: u64::MAX,
                ..pb::MacNonce::default()
            }),
        ),
    ];
    for (table, record) in cases {
        let error = records
            .commit(&[Record::Host(host("h0")), record])
            .expect_err("the record is refused");
        assert_eq!(
            error.to_string(),
            format!(
                "database:{table} is not a store file this release reads: {} {over}",
                u64::MAX
            ),
            "{table}"
        );
    }
    assert_eq!(records.host(&host_id("h0")).expect("a read"), None);

    let state = records
        .commit(&[Record::Request(pb::Request {
            state: i32::MAX,
            ..request(2)
        })])
        .expect_err("a state beyond the column");
    assert!(
        state
            .to_string()
            .starts_with("database:enrollment_requests"),
        "{state}"
    );
    let deny = records
        .commit(&[Record::Deny(pb::Deny {
            host_id: "h1".to_owned(),
            state: -40_000,
            ..pb::Deny::default()
        })])
        .expect_err("a state beyond the column");
    assert!(
        deny.to_string().starts_with("database:enrollment_deny"),
        "{deny}"
    );
}

/// Proves: a request that is not approved is kept and read back without an approval, one that is
/// is read back with it, and the records show nothing of themselves in a `Debug`.
///
/// Catches: an approval invented for a request that has none.
#[test]
fn a_request_without_an_approval_is_read_back_without_one() {
    super::logging();
    let db = TestDatabase::new();
    let (_store, records) = open(&db);
    assert_eq!(format!("{records:?}"), "PgRecords");
    let waiting = request(1);
    let approved = pb::Request {
        approved: Some(pb::Approval {
            host_id: "h1".to_owned(),
            role: "storage".to_owned(),
            approved_unix: 5,
            ..pb::Approval::default()
        }),
        ..request(2)
    };
    records
        .commit(&[Record::Request(waiting), Record::Request(approved)])
        .expect("the requests are kept");
    let first = records
        .request(&KeyHash::from_bytes(&[1; 32]).expect("a key"))
        .expect("a read")
        .expect("a request");
    let second = records
        .request(&KeyHash::from_bytes(&[2; 32]).expect("a key"))
        .expect("a read")
        .expect("a request");
    assert_eq!(first.approved, None);
    assert_eq!(
        second.approved.map(|a| (a.host_id, a.approved_unix)),
        Some(("h1".to_owned(), 5))
    );
}

/// Proves: when the database cannot be read, a read of one record, the list of requests and the
/// open of the records each say so with the table in the place of the error; and a commit of
/// unknown outcome makes the copy of the hosts stale, which the next read of it loads again
/// before it answers (and says so when it cannot).
///
/// Catches: a heartbeat answered from a copy a lost commit may have changed.
#[test]
fn records_that_cannot_read_the_database_say_so_and_a_stale_copy_is_loaded_again() {
    super::logging();
    let db = TestDatabase::new();
    let (store, records) = open(&db);
    records
        .commit(&[Record::Host(host("h1"))])
        .expect("the host is kept");

    db.end_sessions();
    let key = KeyHash::from_bytes(&[1; 32]).expect("a key");
    let read = records.request(&key).expect_err("the reader is gone");
    assert!(
        read.to_string()
            .starts_with("cannot use database:enrollment_requests/"),
        "{read}"
    );
    records.request(&key).expect("the reader is made again");
    db.end_sessions();
    let list = records.requests().expect_err("the reader is gone");
    assert!(
        list.to_string()
            .starts_with("cannot use database:enrollment_requests: "),
        "{list}"
    );
    records.request(&key).expect("the reader is made again");
    db.end_sessions();
    let opened = PgRecords::over(&store).expect_err("the reader is gone");
    assert!(
        opened
            .to_string()
            .starts_with("cannot use database:enrollment_hosts: "),
        "{opened}"
    );

    // The writing session was ended too; the first write finds it out, and a revival makes it.
    records
        .commit(&[Record::Host(host("h0"))])
        .expect_err("the writer is gone");
    store.revive();
    store.on_writer(|_| ()).expect("the writer runs");
    store
        .on_writer(|w| w.fault = Some(Fault::LoseAfterCommit))
        .expect("the fault is set");
    records
        .commit(&[Record::Host(host("h2"))])
        .expect_err("the answer is lost");
    store.revive();
    store.on_writer(|_| ()).expect("the writer runs");
    records.request(&key).expect("the reader is made again");
    db.end_sessions();
    let stale = records
        .host(&host_id("h2"))
        .expect_err("the copy cannot be loaded");
    assert!(
        stale
            .to_string()
            .starts_with("cannot use database:enrollment_hosts: "),
        "{stale}"
    );
    let loaded = records
        .host(&host_id("h2"))
        .expect("the copy is loaded again");
    assert_eq!(loaded.map(|h| h.host_id), Some("h2".to_owned()));
}
