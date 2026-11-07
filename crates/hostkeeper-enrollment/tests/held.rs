//! What the store holds for a host id ([`EnrollmentStore::held`]): nothing, an approved host, a
//! denied (retiring) one or a retired one, read from its files alone, so the same after the
//! store is opened again and whether or not the host was ever heard from; and a refusal, not
//! "nothing", when a file cannot be read.

mod support;

use std::path::Path;
use std::sync::Arc;

use hostkeeper_enrollment::enroll::{EnrollmentStore, HostHeld, StoreError};
use hostkeeper_enrollment::fs::RealFs;
use support::{HostKey, decision, host_id, request, scratch};

fn open(dir: &Path) -> EnrollmentStore {
    EnrollmentStore::open(Arc::new(RealFs), dir).expect("open")
}

/// Posts a new key from machine `machine` and approves it as `host` (`replace` as given),
/// without a heartbeat.
fn approve(store: &EnrollmentStore, machine: &str, host: &str, replace: bool) {
    let key = HostKey::new();
    let id = store
        .post(request(&key, machine), 1)
        .expect("posted")
        .request_id;
    store
        .approve(&id, &key.key_hash(), &decision(host, replace))
        .expect("approved");
}

/// What `store` holds for each of `hosts`.
fn held(store: &EnrollmentStore, hosts: &[&str]) -> Vec<HostHeld> {
    hosts
        .iter()
        .map(|host| store.held(&host_id(host)).expect("read"))
        .collect()
}

/// Proves: an id never approved holds nothing; an approved id never heard from is approved; a
/// denied id is denied until its retirement is final, then retired, also for an id denied
/// before it was ever approved; an id denied and then approved again with `replace` is
/// approved; and every answer is the same from the store opened again over the same files.
///
/// Catches: `held` read from what the store saw since it opened (a heartbeat, a cache), so that
/// a reopened store says "nothing" for a retired or a silent approved host; a cleared deny row
/// taken as still denied; and a retiring row taken as retired or as nothing.
#[test]
fn held_says_none_approved_denied_or_retired_and_survives_reopening() {
    let dir = scratch("held-states");
    let store = open(dir.path());
    approve(&store, "m-1", "db-1", false);
    approve(&store, "m-2", "db-2", false);
    approve(&store, "m-3", "db-3", false);
    approve(&store, "m-4", "db-4", false);
    store.deny(&host_id("db-2")).expect("denied");
    store.deny(&host_id("db-3")).expect("denied");
    store.retire(&host_id("db-3")).expect("retired");
    store.deny(&host_id("db-4")).expect("denied");
    approve(&store, "m-5", "db-4", true);
    store
        .deny(&host_id("ahead-1"))
        .expect("denied ahead of time");
    store
        .retire(&host_id("ahead-2"))
        .expect("retired ahead of time");

    let hosts = ["db-0", "db-1", "db-2", "db-3", "db-4", "ahead-1", "ahead-2"];
    let want = [
        HostHeld::None,
        HostHeld::Approved,
        HostHeld::Denied,
        HostHeld::Retired,
        HostHeld::Approved,
        HostHeld::Denied,
        HostHeld::Retired,
    ];
    assert_eq!(held(&store, &hosts), want);
    drop(store);
    assert_eq!(held(&open(dir.path()), &hosts), want, "reopened");
}

/// Proves: a deny row cleared by an approval whose host file is then missing (an approval cut
/// short between its writes) holds nothing.
///
/// Catches: a cleared row read as an approved host with no host file, or as denied.
#[test]
fn a_cleared_deny_row_without_a_host_file_holds_nothing() {
    let dir = scratch("held-cleared");
    let store = open(dir.path());
    approve(&store, "m-1", "db-1", false);
    store.deny(&host_id("db-1")).expect("denied");
    approve(&store, "m-2", "db-1", true);
    std::fs::remove_file(dir.path().join("hosts/db-1.pb")).expect("remove");
    assert_eq!(held(&store, &["db-1"]), [HostHeld::None]);
}

/// Proves: a deny file or a host file that does not decode is refused as corrupt.
///
/// Catches: an unreadable file answered as "nothing held", which would let a retired host's
/// record look free.
#[test]
fn an_unreadable_deny_or_host_file_is_refused() {
    for file in ["deny/db-1.pb", "hosts/db-1.pb"] {
        let dir = scratch("held-unreadable");
        let store = open(dir.path());
        approve(&store, "m-1", "db-1", false);
        store.deny(&host_id("db-1")).expect("denied");
        if file.starts_with("hosts") {
            approve(&store, "m-2", "db-1", true);
        }
        std::fs::write(dir.path().join(file), b"\xff").expect("write");
        let refused = store.held(&host_id("db-1")).expect_err("refused");
        assert!(
            matches!(refused, StoreError::Corrupt { .. }),
            "{file}: {refused}"
        );
    }
}
