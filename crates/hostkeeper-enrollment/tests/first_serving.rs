//! The first heartbeat the coordinator accepted from an approved host that reported a service it
//! manages for the host installed and healthy ([`EnrollmentStore::note_serving`]): written once
//! into the host file beside the first heartbeat and the duplicate mark, kept by later reports and
//! a reopened store, cleared by every approval of the host id (a new key or a renewal), and never
//! written for a host id the store never approved.

mod support;

use std::path::Path;
use std::sync::Arc;

use hostkeeper_enrollment::enroll::{EnrollmentStore, StoreError};
use hostkeeper_enrollment::fs::{FaultFs, RealFs};
use support::{HostKey, NOW, TestCa, decision, host_id, renewal, request, scratch};

fn open(dir: &Path) -> EnrollmentStore {
    EnrollmentStore::open(Arc::new(RealFs), dir).expect("open")
}

/// Posts and approves `key`'s request from `machine` as `host`, taking the id over when
/// `replace`.
fn enroll(store: &EnrollmentStore, key: &HostKey, machine: &str, host: &str, replace: bool) {
    let id = store
        .post(request(key, machine), NOW)
        .expect("posted")
        .request_id;
    store
        .approve(&id, &key.key_hash(), &decision(host, replace))
        .expect("approved");
}

/// The first heartbeat time and the first serving time `store` answers for `host`.
fn times(store: &EnrollmentStore, host: &str) -> (Option<u64>, Option<u64>) {
    let enrolled = store
        .enrolled_host(&host_id(host))
        .expect("read")
        .expect("enrolled");
    (enrolled.first_heartbeat_unix, enrolled.first_serving_unix)
}

/// Catches: a time before the host served, a later report replacing the first, a time noted
/// for one host given to another, a time kept in memory only (lost when the store is opened
/// again), and the serving time written over the first heartbeat or the duplicate mark, or they
/// over it.
#[test]
fn the_first_serving_heartbeat_is_written_once_beside_the_first_heartbeat() {
    let dir = scratch("first-serving-once");
    let store = open(dir.path());
    enroll(&store, &HostKey::new(), "m-1", "web-1", false);
    enroll(&store, &HostKey::new(), "m-2", "web-2", false);
    let web = host_id("web-1");
    store.note_heartbeat(&web, NOW + 60).expect("noted");
    assert_eq!(
        times(&store, "web-1"),
        (Some(NOW + 60), None),
        "not serving"
    );

    store.note_serving(&web, NOW + 120).expect("noted");
    store.note_serving(&web, NOW + 135).expect("noted");
    assert_eq!(times(&store, "web-1"), (Some(NOW + 60), Some(NOW + 120)));
    assert_eq!(times(&store, "web-2"), (None, None), "another host");

    assert!(!store.report_live(&web, "m-1", NOW + 140).expect("live"));
    assert!(store.report_live(&web, "m-9", NOW + 150).expect("live"));
    let reopened = open(dir.path());
    reopened.note_serving(&web, NOW + 200).expect("noted");
    reopened.note_heartbeat(&web, NOW + 200).expect("noted");
    assert_eq!(times(&reopened, "web-1"), (Some(NOW + 60), Some(NOW + 120)));
    assert!(reopened.is_duplicate(&web).expect("read"), "the mark kept");
}

/// Catches: a re-enrollment, by a renewal of the same host or by a new key that takes the id
/// over, keeping the serving time of the approval before, in the file or in memory.
#[test]
fn every_approval_of_the_host_clears_the_first_serving_heartbeat() {
    let ca = TestCa::new("first-serving-again-ca");
    let dir = scratch("first-serving-again");
    let store = open(dir.path());
    let key = HostKey::new();
    enroll(&store, &key, "m-1", "web-1", false);
    let web = host_id("web-1");
    store.note_serving(&web, NOW + 60).expect("noted");

    store.issue(&ca.ca, &key.key_hash()).expect("issued");
    let renewing = store
        .post(renewal(&key, "m-1", "web-1"), NOW + 100)
        .expect("a renewal");
    assert_eq!(times(&store, "web-1").1, Some(NOW + 60), "a repost");
    store
        .approve(
            &renewing.request_id,
            &key.key_hash(),
            &decision("web-1", false),
        )
        .expect("the renewal approved");
    assert_eq!(times(&store, "web-1").1, None, "after a renewal");
    store.note_serving(&web, NOW + 200).expect("noted");
    assert_eq!(times(&store, "web-1").1, Some(NOW + 200));

    enroll(&store, &HostKey::new(), "m-9", "web-1", true);
    assert_eq!(times(&store, "web-1").1, None, "after a new key");
    store.note_serving(&web, NOW + 300).expect("noted");
    assert_eq!(times(&store, "web-1").1, Some(NOW + 300));
}

/// Catches: a report of a host id never approved refused, or making a host file for it.
#[test]
fn a_host_never_approved_records_no_serving_time() {
    let dir = scratch("first-serving-unknown");
    let store = open(dir.path());
    store
        .post(request(&HostKey::new(), "m-1"), NOW)
        .expect("posted");
    store
        .note_serving(&host_id("web-1"), NOW + 60)
        .expect("nothing to record");
    assert!(!dir.path().join("hosts/web-1.pb").exists());
}

/// Catches: a host file that does not decode read as a host that never served, and noted over.
#[test]
fn a_host_file_that_cannot_be_read_is_refused_its_serving_time() {
    let dir = scratch("first-serving-corrupt");
    let store = open(dir.path());
    enroll(&store, &HostKey::new(), "m-1", "web-1", false);
    std::fs::write(dir.path().join("hosts/web-1.pb"), b"\xff").expect("write");
    let refused = store
        .note_serving(&host_id("web-1"), NOW + 60)
        .expect_err("refused");
    assert!(matches!(refused, StoreError::Corrupt { .. }), "{refused}");
    assert_eq!(
        std::fs::read(dir.path().join("hosts/web-1.pb")).expect("read"),
        b"\xff"
    );
}

/// Catches: every report after the first taking the store's lock and reading the host file, and
/// a host remembered as serving past a write that failed (never written at all) or past an
/// approval (its next serving time never written).
#[test]
fn a_report_after_the_first_touches_no_file_until_the_host_is_approved_again() {
    let dir = scratch("first-serving-memory");
    let fs = Arc::new(FaultFs::new());
    let store = EnrollmentStore::open(fs.clone(), dir.path()).expect("open");
    enroll(&store, &HostKey::new(), "m-1", "web-1", false);
    let web = host_id("web-1");

    // Noting takes the lock (1), reads the host file (2), then writes it anew (3): that fails.
    fs.fail_at(fs.count() + 3);
    store.note_serving(&web, NOW + 50).expect_err("not written");
    assert_eq!(times(&store, "web-1").1, None);
    store.note_serving(&web, NOW + 60).expect("noted");
    let before = fs.count();
    store.note_serving(&web, NOW + 75).expect("noted");
    assert_eq!(fs.count(), before, "answered from memory");
    assert_eq!(times(&store, "web-1").1, Some(NOW + 60));

    enroll(&store, &HostKey::new(), "m-9", "web-1", true);
    store.note_serving(&web, NOW + 300).expect("noted");
    assert_eq!(times(&store, "web-1").1, Some(NOW + 300));
}
