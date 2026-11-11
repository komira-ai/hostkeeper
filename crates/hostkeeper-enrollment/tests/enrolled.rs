//! What the store answers of an enrolled host: the claims of the request its key was approved
//! from and when that approval was made, for one host id and for every host; nothing for a host
//! id never approved or denied; and a refusal, not a silent gap, when its files cannot be read.

mod support;

use std::path::Path;
use std::sync::Arc;

use hostkeeper_enrollment::enroll::{
    ApprovalRecord, Claims, EnrolledHost, EnrollmentStore, StoreError,
};
use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_enrollment::names::{KeyHash, Role};
use support::{HostKey, decision, host_id, request, scratch};

/// What the host of machine `machine` (`m-<n>`) claims: two data MACs and a BMC MAC, each its
/// own.
fn claimed(machine: &str) -> Claims {
    let n = machine.trim_start_matches("m-");
    Claims {
        machine_id: machine.to_owned(),
        addresses: vec![
            format!("0c:c4:7a:00:0{n}:02"),
            format!("0c:c4:7a:00:0{n}:03"),
        ],
        bmc_mac: format!("0c:c4:7a:00:0{n}:01"),
        ..Claims::default()
    }
}

/// Posts `key`'s request from machine `machine`, claiming [`claimed`]; its request id.
fn post(store: &EnrollmentStore, key: &HostKey, machine: &str) -> KeyHash {
    let mut new = request(key, machine);
    new.claims = claimed(machine);
    store.post(new, 1).expect("posted").request_id
}

/// Posts and approves `key`'s request as `host`, recorded by `alice` at `at`.
fn enroll(store: &EnrollmentStore, key: &HostKey, machine: &str, host: &str, at: u64) {
    let id = post(store, key, machine);
    store
        .approve_recorded(
            &id,
            &key.key_hash(),
            &decision(host, false),
            &ApprovalRecord {
                approver: "alice".to_owned(),
                approved_unix: at,
                reason: "new rack".to_owned(),
                mac_nonce: None,
                mac_enrolled_since_unix: None,
            },
        )
        .expect("approved");
}

fn enrolled(host: &str, machine: &str, at: u64) -> EnrolledHost {
    EnrolledHost {
        host_id: host_id(host),
        claims: claimed(machine),
        approved_unix: at,
        first_heartbeat_unix: None,
        first_serving_unix: None,
        role: Role::parse("web").expect("a role"),
        host_group: String::new(),
        moving_from: None,
        running: None,
        running_unix: None,
    }
}

/// Catches: an approved host answered without its claims or with another time than its
/// approval's, a pending request or a host id never approved answered as enrolled, a denied
/// host still answered, an approval recorded without an approver answered with a time, and the
/// list of every host out of host-id order or broken by a file in `hosts/` that names no host.
#[test]
fn an_approved_host_is_enrolled_with_its_claims_and_approval_time() {
    let dir = scratch("enrolled-hosts");
    let store = EnrollmentStore::open(Arc::new(RealFs), dir.path()).expect("open");
    enroll(&store, &HostKey::new(), "m-2", "db-2", 1_800_000_200);
    enroll(&store, &HostKey::new(), "m-1", "db-1", 1_800_000_100);
    post(&store, &HostKey::new(), "m-3");
    let plain = HostKey::new();
    let id = post(&store, &plain, "m-4");
    store
        .approve(&id, &plain.key_hash(), &decision("db-4", false))
        .expect("approved without a record");
    let gone = HostKey::new();
    enroll(&store, &gone, "m-5", "db-5", 1_800_000_500);
    store.deny(&host_id("db-5")).expect("denied");
    std::fs::write(dir.path().join("hosts/README"), b"not a host file").expect("write");
    std::fs::write(dir.path().join("hosts/Not_A_Host.pb"), b"").expect("write");

    assert_eq!(
        store.enrolled_host(&host_id("db-1")).expect("read"),
        Some(enrolled("db-1", "m-1", 1_800_000_100))
    );
    assert_eq!(
        store.enrolled_host(&host_id("db-4")).expect("read"),
        Some(enrolled("db-4", "m-4", 0))
    );
    assert_eq!(store.enrolled_host(&host_id("db-3")).expect("read"), None);
    assert_eq!(store.enrolled_host(&host_id("db-5")).expect("read"), None);
    assert_eq!(
        store.enrolled_hosts().expect("read"),
        [
            enrolled("db-1", "m-1", 1_800_000_100),
            enrolled("db-2", "m-2", 1_800_000_200),
            enrolled("db-4", "m-4", 0),
        ]
    );
}

/// `store` over `dir` with `db-1` enrolled; the request id of its key.
fn one_host(dir: &Path) -> (EnrollmentStore, KeyHash) {
    let store = EnrollmentStore::open(Arc::new(RealFs), dir).expect("open");
    let key = HostKey::new();
    enroll(&store, &key, "m-1", "db-1", 1_800_000_100);
    (store, key.key_hash())
}

fn is_corrupt(error: &StoreError) -> bool {
    matches!(error, StoreError::Corrupt { .. })
}

/// Catches: a host whose request file is gone, whose request, host or deny file does not
/// decode, or a `hosts/` directory that cannot be listed, answered as not enrolled (or the
/// list answered without it) instead of refused.
#[test]
fn a_host_whose_files_cannot_be_read_is_refused() {
    let dir = scratch("enrolled-missing-request");
    let (store, id) = one_host(dir.path());
    std::fs::remove_file(dir.path().join(format!("requests/{id}.pb"))).expect("remove");
    assert!(is_corrupt(
        &store.enrolled_host(&host_id("db-1")).expect_err("refused")
    ));
    assert!(is_corrupt(&store.enrolled_hosts().expect_err("refused")));

    let dir = scratch("enrolled-bad-request");
    let (store, id) = one_host(dir.path());
    std::fs::write(dir.path().join(format!("requests/{id}.pb")), b"\xff").expect("write");
    store.enrolled_host(&host_id("db-1")).expect_err("refused");

    let dir = scratch("enrolled-bad-host");
    let (store, _) = one_host(dir.path());
    std::fs::write(dir.path().join("hosts/db-1.pb"), b"\xff").expect("write");
    store.enrolled_host(&host_id("db-1")).expect_err("refused");

    let dir = scratch("enrolled-bad-deny");
    let (store, _) = one_host(dir.path());
    std::fs::write(dir.path().join("deny/db-1.pb"), b"\xff").expect("write");
    store.enrolled_host(&host_id("db-1")).expect_err("refused");

    let dir = scratch("enrolled-no-hosts-dir");
    let (store, _) = one_host(dir.path());
    std::fs::remove_dir_all(dir.path().join("hosts")).expect("remove");
    std::fs::write(dir.path().join("hosts"), b"a file").expect("write");
    let refused = store.enrolled_hosts().expect_err("refused");
    assert!(matches!(refused, StoreError::Io { .. }), "{refused}");
}
