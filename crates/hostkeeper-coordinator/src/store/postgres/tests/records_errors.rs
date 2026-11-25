//! The enrollment records' refusals: what they will not write, and what they say when the
//! database cannot be read.

use hostkeeper_enrollment::enroll::StoreError;
use hostkeeper_enrollment::enroll::records::{Record, Records, pb};
use hostkeeper_enrollment::names::{HostId, KeyHash};

use super::TestDatabase;
use crate::store::Backend;
use crate::store::postgres::writer::Fault;
use crate::store::postgres::{PgRecords, Postgres};

fn open(db: &TestDatabase) -> (Postgres, PgRecords) {
    let (store, _) = db.open("records").expect("the store opens");
    let records = PgRecords::over(&store);
    (store, records)
}

/// A read of the records whose answer the test drops, keeping its error.
type Read<'a> = dyn Fn() -> Result<(), StoreError> + 'a;

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
            "enrollment_hosts",
            Record::Host(pb::Host {
                first_serving_unix: Some(u64::MAX),
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
            .commit(&[], &[Record::Host(host("h0")), record])
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
        .commit(
            &[],
            &[Record::Request(pb::Request {
                state: i32::MAX,
                ..request(2)
            })],
        )
        .expect_err("a state beyond the column");
    assert!(
        state
            .to_string()
            .starts_with("database:enrollment_requests"),
        "{state}"
    );
    let deny = records
        .commit(
            &[],
            &[Record::Deny(pb::Deny {
                host_id: "h1".to_owned(),
                state: -40_000,
                ..pb::Deny::default()
            })],
        )
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
        .commit(&[], &[Record::Request(waiting), Record::Request(approved)])
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

/// Proves: when the database cannot be read, a read of one record of each kind and the lists of
/// requests and hosts each say so with the table in the place of the error, and the next read
/// makes the reader again; and a host that a commit of unknown outcome wrote is read as the table
/// holds it, with no copy in memory to be reloaded.
///
/// Catches: a read answered from memory a lost commit may have left behind the table, or a
/// failed read whose error names no table.
#[test]
fn records_that_cannot_read_the_database_say_so_and_read_what_a_lost_commit_wrote() {
    super::logging();
    let db = TestDatabase::new();
    let (store, records) = open(&db);
    records
        .commit(&[], &[Record::Host(host("h1"))])
        .expect("the host is kept");

    let key = KeyHash::from_bytes(&[1; 32]).expect("a key");
    // The reading session is made at the first read.
    records.request(&key).expect("the reader is made");
    let reads: [(&str, &Read<'_>); 5] = [
        ("enrollment_requests/", &|| records.request(&key).map(drop)),
        ("enrollment_requests: ", &|| records.requests().map(drop)),
        ("enrollment_hosts/h1", &|| {
            records.host(&host_id("h1")).map(drop)
        }),
        ("enrollment_hosts: ", &|| records.host_ids().map(drop)),
        ("enrollment_deny/h1", &|| {
            records.deny(&host_id("h1")).map(drop)
        }),
    ];
    for (place, read) in reads {
        db.end_sessions();
        let refused = read().expect_err("the reader is gone");
        assert!(
            refused
                .to_string()
                .starts_with(&format!("cannot use database:{place}")),
            "{refused}"
        );
        read().expect("the reader is made again");
    }

    // The writing session was ended too; the first write finds it out, and a revival makes it.
    records
        .commit(&[], &[Record::Host(host("h0"))])
        .expect_err("the writer is gone");
    store.revive();
    store.settle().expect("the writers run");
    store.fault(Fault::LoseAfterCommit);
    records
        .commit(&[], &[Record::Host(host("h2"))])
        .expect_err("the answer is lost");
    let read = records.host(&host_id("h2")).expect("read while it is lost");
    assert_eq!(read.map(|h| h.host_id), Some("h2".to_owned()));
    assert_eq!(
        records.host_ids().expect("listed"),
        [host_id("h1"), host_id("h2")]
    );
}
