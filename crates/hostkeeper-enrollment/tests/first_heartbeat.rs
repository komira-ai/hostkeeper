//! The first heartbeat the coordinator accepted from an approved host
//! ([`EnrollmentStore::note_heartbeat`]): written once into the host file, kept by later
//! heartbeats, a reopened store, a renewal's repost and a duplicate mark, cleared by every
//! approval of the host id (a new key or a renewal), and never written for a host id the store
//! never approved.

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

/// The first heartbeat time `store` answers for `host`.
fn first(store: &EnrollmentStore, host: &str) -> Option<u64> {
    store
        .enrolled_host(&host_id(host))
        .expect("read")
        .expect("enrolled")
        .first_heartbeat_unix
}

/// Catches: no time before the first heartbeat, the time of a later heartbeat replacing the
/// first, a time noted for one host given to another, and a time kept in memory only (lost when
/// the store is opened again).
#[test]
fn the_first_heartbeat_is_written_once_and_read_after_a_reopen() {
    let dir = scratch("first-beat-once");
    let store = open(dir.path());
    enroll(&store, &HostKey::new(), "m-1", "web-1", false);
    enroll(&store, &HostKey::new(), "m-2", "web-2", false);
    assert_eq!(first(&store, "web-1"), None, "not heard yet");

    store
        .note_heartbeat(&host_id("web-1"), NOW + 60)
        .expect("noted");
    store
        .note_heartbeat(&host_id("web-1"), NOW + 75)
        .expect("noted");
    assert_eq!(first(&store, "web-1"), Some(NOW + 60));
    assert_eq!(first(&store, "web-2"), None, "another host");

    let reopened = open(dir.path());
    reopened
        .note_heartbeat(&host_id("web-1"), NOW + 90)
        .expect("noted");
    assert_eq!(first(&reopened, "web-1"), Some(NOW + 60));
}

/// Catches: a re-enrollment, by a new key that takes the id over or by a renewal of the same
/// host, keeping the first heartbeat of the approval before.
#[test]
fn every_approval_of_the_host_clears_the_first_heartbeat() {
    let ca = TestCa::new("first-beat-again-ca");
    let dir = scratch("first-beat-again");
    let store = open(dir.path());
    let key = HostKey::new();
    enroll(&store, &key, "m-1", "web-1", false);
    let web = host_id("web-1");
    store.note_heartbeat(&web, NOW + 60).expect("noted");

    store.issue(&ca.ca, &key.key_hash()).expect("issued");
    let renewing = store
        .post(renewal(&key, "m-1", "web-1"), NOW + 100)
        .expect("a renewal");
    assert_eq!(
        first(&store, "web-1"),
        Some(NOW + 60),
        "a repost changes nothing"
    );
    store
        .approve(
            &renewing.request_id,
            &key.key_hash(),
            &decision("web-1", false),
        )
        .expect("the renewal approved");
    assert_eq!(first(&store, "web-1"), None, "after a renewal");
    store.note_heartbeat(&web, NOW + 200).expect("noted");
    assert_eq!(first(&store, "web-1"), Some(NOW + 200));

    enroll(&store, &HostKey::new(), "m-9", "web-1", true);
    assert_eq!(first(&store, "web-1"), None, "after a new key");
    store.note_heartbeat(&web, NOW + 300).expect("noted");
    assert_eq!(first(&store, "web-1"), Some(NOW + 300));
}

/// Catches: a heartbeat of a host id never approved (an updater of the farm CA, or a pending
/// request) refused, or making a host file for it.
#[test]
fn a_host_never_approved_records_nothing() {
    let dir = scratch("first-beat-unknown");
    let store = open(dir.path());
    store
        .post(request(&HostKey::new(), "m-1"), NOW)
        .expect("posted");
    store
        .note_heartbeat(&host_id("web-1"), NOW + 60)
        .expect("nothing to record");
    assert!(!dir.path().join("hosts/web-1.pb").exists());
    assert_eq!(store.enrolled_host(&host_id("web-1")).expect("read"), None);
}

/// Catches: a duplicate mark (written into the same host file by `report_live`) losing the
/// first heartbeat, and the first heartbeat losing the duplicate mark.
#[test]
fn the_duplicate_mark_and_the_first_heartbeat_keep_each_other() {
    let dir = scratch("first-beat-duplicate");
    let store = open(dir.path());
    enroll(&store, &HostKey::new(), "m-1", "web-1", false);
    let web = host_id("web-1");
    assert!(!store.report_live(&web, "m-1", NOW + 10).expect("live"));
    store.note_heartbeat(&web, NOW + 10).expect("noted");
    assert!(store.report_live(&web, "m-2", NOW + 20).expect("live"));
    assert_eq!(first(&store, "web-1"), Some(NOW + 10));

    enroll(&store, &HostKey::new(), "m-3", "web-3", false);
    let other = host_id("web-3");
    store.report_live(&other, "m-3", NOW + 10).expect("live");
    assert!(store.report_live(&other, "m-4", NOW + 20).expect("live"));
    store.note_heartbeat(&other, NOW + 30).expect("noted");
    assert!(store.is_duplicate(&other).expect("read"));
    assert_eq!(first(&store, "web-3"), Some(NOW + 30));
}

/// Catches: a host file that does not decode read as a host never heard, and noted over.
#[test]
fn a_host_file_that_cannot_be_read_is_refused() {
    let dir = scratch("first-beat-corrupt");
    let store = open(dir.path());
    enroll(&store, &HostKey::new(), "m-1", "web-1", false);
    std::fs::write(dir.path().join("hosts/web-1.pb"), b"\xff").expect("write");
    let refused = store
        .note_heartbeat(&host_id("web-1"), NOW + 60)
        .expect_err("refused");
    assert!(matches!(refused, StoreError::Corrupt { .. }), "{refused}");
    assert_eq!(
        std::fs::read(dir.path().join("hosts/web-1.pb")).expect("read"),
        b"\xff"
    );
}

/// Catches: every heartbeat after the first taking the store's lock and reading the host file,
/// and a host remembered as heard past an approval (its next first heartbeat never written) or
/// past a write that failed (never written at all).
#[test]
fn a_heartbeat_after_the_first_touches_no_file_until_the_host_is_approved_again() {
    let dir = scratch("first-beat-memory");
    let fs = Arc::new(FaultFs::new());
    let store = EnrollmentStore::open(fs.clone(), dir.path()).expect("open");
    enroll(&store, &HostKey::new(), "m-1", "web-1", false);
    let web = host_id("web-1");

    // Noting takes the lock (1), reads the host file (2), then writes it anew (3): that fails.
    fs.fail_at(fs.count() + 3);
    store
        .note_heartbeat(&web, NOW + 50)
        .expect_err("not written");
    assert_eq!(first(&store, "web-1"), None);
    store.note_heartbeat(&web, NOW + 60).expect("noted");
    let before = fs.count();
    store.note_heartbeat(&web, NOW + 75).expect("noted");
    assert_eq!(fs.count(), before, "answered from memory");
    assert_eq!(first(&store, "web-1"), Some(NOW + 60));

    enroll(&store, &HostKey::new(), "m-9", "web-1", true);
    store.note_heartbeat(&web, NOW + 300).expect("noted");
    assert_eq!(first(&store, "web-1"), Some(NOW + 300));

    let reopened = EnrollmentStore::open(fs.clone(), dir.path()).expect("open");
    reopened.note_heartbeat(&web, NOW + 400).expect("read once");
    let before = fs.count();
    reopened.note_heartbeat(&web, NOW + 415).expect("noted");
    assert_eq!(fs.count(), before, "remembered after one read");
    assert_eq!(first(&reopened, "web-1"), Some(NOW + 300));
}
