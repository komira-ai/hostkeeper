//! A host's approval for service identity: the assurance read from who approved the host's key,
//! the operator's approval of an auto host's key, what is allowed, the hosts that are denied, and
//! what each of those does over the files and over the fake.

mod support;

use std::sync::Arc;

use hostkeeper_enrollment::enroll::records::{FileRecords, MemoryRecords, Record, Records, pb};
use hostkeeper_enrollment::enroll::{
    ApprovalRecord, Assurance, EnrollmentStore, HostIdentityState, INVENTORY_APPROVER,
    IdentityApproval, IdentityApproved, IdentityError, StoreError,
};
use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_testkit::Scratch;
use support::{HostKey, NOW, TestCa, decision, host_id, renewal, request, scratch};

/// A store, its records to read what was written beside it, and the scratch directory the files
/// are in, which is removed when the last of them is dropped.
type Kept = (
    &'static str,
    EnrollmentStore,
    Arc<dyn Records>,
    Arc<Scratch>,
);

/// One store over the files of a scratch directory and one over the fake.
fn stores(name: &str) -> Vec<Kept> {
    let dir = Arc::new(scratch(name));
    let path = dir.path().to_path_buf();
    let files: Arc<dyn Records> =
        Arc::new(FileRecords::open(Arc::new(RealFs), &path).expect("open the files"));
    let memory: Arc<dyn Records> = Arc::new(MemoryRecords::new());
    vec![
        (
            "files",
            EnrollmentStore::new(files.clone()),
            files,
            dir.clone(),
        ),
        ("memory", EnrollmentStore::new(memory.clone()), memory, dir),
    ]
}

fn record(approver: &str) -> ApprovalRecord {
    ApprovalRecord {
        approver: approver.to_owned(),
        approved_unix: NOW,
        reason: "racked".to_owned(),
        mac_nonce: None,
        mac_enrolled_since_unix: None,
    }
}

/// Posts `key` from `machine` and approves it as `host` by `approver` (`None`: the plain
/// approval, which records no approver), taking the id over when `replace`.
fn enroll(
    store: &EnrollmentStore,
    key: &HostKey,
    machine: &str,
    host: &str,
    approver: Option<&str>,
    replace: bool,
) {
    let id = store
        .post(request(key, machine), NOW)
        .expect("posted")
        .request_id;
    let decided = decision(host, replace);
    match approver {
        Some(approver) => store
            .approve_recorded(&id, &key.key_hash(), &decided, &record(approver))
            .map(|_| ()),
        None => store.approve(&id, &key.key_hash(), &decided).map(|_| ()),
    }
    .expect("approved");
}

fn approval(key: &HostKey, approver: &str, at: u64) -> IdentityApproval {
    IdentityApproval {
        key_hash: key.key_hash(),
        approver: approver.to_owned(),
        approved_unix: at,
    }
}

/// Proves: the assurance of a host is `auto` when the approval of its key names the inventory
/// approver, and `operator` for an operator's name and for an approval recorded with no approver;
/// a host id never approved has no state.
///
/// Catches: a host approved by an inventory record counted as approved by hand, which would let
/// any machine an inventory record matched get service certificates.
#[test]
fn the_assurance_is_auto_for_an_inventory_approval_and_operator_for_any_other() {
    for (label, store, _, _dir) in stores("identity-assurance") {
        let (auto, by_hand, plain) = (HostKey::new(), HostKey::new(), HostKey::new());
        enroll(
            &store,
            &auto,
            "m-1",
            "auto-1",
            Some(INVENTORY_APPROVER),
            false,
        );
        enroll(&store, &by_hand, "m-2", "hand-1", Some("ops"), false);
        enroll(&store, &plain, "m-3", "plain-1", None, false);
        let state = |host: &str| store.identity_state(&host_id(host)).expect("read");
        assert_eq!(
            state("auto-1"),
            Some(HostIdentityState {
                assurance: Assurance::Auto,
                key_hash: auto.key_hash(),
                approval: None,
                denied: false,
            }),
            "{label}"
        );
        assert_eq!(
            state("hand-1").map(|s| s.assurance),
            Some(Assurance::Operator),
            "{label}"
        );
        assert_eq!(
            state("plain-1").map(|s| s.assurance),
            Some(Assurance::Operator),
            "{label}"
        );
        assert_eq!(state("nobody-1"), None, "{label}");
        assert_eq!(INVENTORY_APPROVER, "inventory");
    }
}

/// Proves: a host whose key has no request, or whose request holds no approval, is `auto`: a
/// missing record never allows more.
///
/// Catches: a host read as approved by hand because the record that says otherwise is gone.
#[test]
fn a_host_whose_approval_is_not_found_counts_as_auto() {
    for (label, store, records, _dir) in stores("identity-missing") {
        let orphan = HostKey::new();
        records
            .commit(&[Record::Host(pb::Host {
                schema: 1,
                host_id: "orphan-1".to_owned(),
                key_hash: orphan.key_hash().as_bytes().to_vec(),
                machine_id: "m-1".to_owned(),
                role: "web".to_owned(),
                ..pb::Host::default()
            })])
            .expect("a host with no request");
        let waiting = HostKey::new();
        store
            .post(request(&waiting, "m-2"), NOW)
            .expect("a request that waits");
        records
            .commit(&[Record::Host(pb::Host {
                schema: 1,
                host_id: "waiting-1".to_owned(),
                key_hash: waiting.key_hash().as_bytes().to_vec(),
                machine_id: "m-2".to_owned(),
                role: "web".to_owned(),
                ..pb::Host::default()
            })])
            .expect("a host whose request is not approved");
        for host in ["orphan-1", "waiting-1"] {
            let state = store
                .identity_state(&host_id(host))
                .expect("read")
                .expect("a host");
            assert_eq!(state.assurance, Assurance::Auto, "{label} {host}");
            assert!(!store.identity_allowed(&host_id(host)).expect("read"));
        }
    }
}

/// Proves: an operator's approval of an auto host's current key is written (naming who and when),
/// shown by the host's state, allows the host, is kept by a store made again over the same
/// records, and a second approval of the same key writes nothing and answers the first.
///
/// Catches: an approval that is lost, that names the later caller, or that is written again on
/// each call.
#[test]
fn an_operator_approves_an_auto_hosts_key_once() {
    for (label, store, records, _dir) in stores("identity-approve") {
        let key = HostKey::new();
        enroll(
            &store,
            &key,
            "m-1",
            "auto-1",
            Some(INVENTORY_APPROVER),
            false,
        );
        let host = host_id("auto-1");
        assert!(!store.identity_allowed(&host).expect("read"), "{label}");

        let first = store
            .approve_identity(&host, &key.key_hash(), "alice", NOW + 5)
            .expect("approved");
        assert_eq!(
            first,
            IdentityApproved::Approved(approval(&key, "alice", NOW + 5)),
            "{label}"
        );
        let held = records.host(&host).expect("read").expect("the host");
        assert_eq!(
            held.identity_approval,
            Some(pb::IdentityApproval {
                key_hash: key.key_hash().as_bytes().to_vec(),
                approver: "alice".to_owned(),
                approved_unix: NOW + 5,
            }),
            "{label}"
        );
        assert!(store.identity_allowed(&host).expect("read"), "{label}");
        let state = store.identity_state(&host).expect("read").expect("a host");
        assert_eq!(
            (state.assurance, state.approval, state.denied),
            (
                Assurance::Auto,
                Some(approval(&key, "alice", NOW + 5)),
                false
            ),
            "{label}"
        );

        let again = store
            .approve_identity(&host, &key.key_hash(), "bob", NOW + 9)
            .expect("approved again");
        assert_eq!(
            again,
            IdentityApproved::AlreadyApproved(approval(&key, "alice", NOW + 5)),
            "{label}"
        );
        assert_eq!(
            records.host(&host).expect("read").expect("the host"),
            held,
            "{label}: nothing was written again"
        );
        let reopened = EnrollmentStore::new(records.clone());
        assert!(reopened.identity_allowed(&host).expect("read"), "{label}");
    }
}

/// Proves: a host approved by an operator is told so and nothing is written, and its host
/// record holds no identity approval; it is allowed with none.
///
/// Catches: an approval written for a host that needs none, which a later change to how the host
/// was approved would read as an approval of the key.
#[test]
fn a_host_an_operator_approved_needs_no_identity_approval() {
    for (label, store, records, _dir) in stores("identity-operator") {
        let key = HostKey::new();
        enroll(&store, &key, "m-1", "hand-1", Some("ops"), false);
        let host = host_id("hand-1");
        assert!(store.identity_allowed(&host).expect("read"), "{label}");
        let answer = store
            .approve_identity(&host, &key.key_hash(), "alice", NOW + 5)
            .expect("answered");
        assert_eq!(answer, IdentityApproved::OperatorAssured, "{label}");
        assert_eq!(
            records
                .host(&host)
                .expect("read")
                .expect("the host")
                .identity_approval,
            None,
            "{label}"
        );
    }
}

/// Proves: an approval is refused for a host never approved, for a key that is not the host's
/// current key, and for a denied host, in that order of the checks (a denied host named by
/// another key is a key mismatch), and none of them writes.
///
/// Catches: an approval of a key the host does not hold, which would allow the key it holds
/// later, and an approval of a retired host.
#[test]
fn an_approval_for_an_unknown_host_another_key_or_a_denied_host_is_refused() {
    for (label, store, records, _dir) in stores("identity-refused") {
        let key = HostKey::new();
        enroll(
            &store,
            &key,
            "m-1",
            "auto-1",
            Some(INVENTORY_APPROVER),
            false,
        );
        let host = host_id("auto-1");
        let other = HostKey::new().key_hash();

        let unknown = store
            .approve_identity(&host_id("nobody-1"), &key.key_hash(), "alice", NOW)
            .expect_err("unknown");
        assert!(
            matches!(&unknown, IdentityError::UnknownHost(id) if *id == host_id("nobody-1")),
            "{label}"
        );
        assert_eq!(unknown.to_string(), "no host nobody-1 was approved");
        let mismatch = store
            .approve_identity(&host, &other, "alice", NOW)
            .expect_err("another key");
        assert!(matches!(mismatch, IdentityError::KeyMismatch), "{label}");
        assert_eq!(
            mismatch.to_string(),
            "the key named is not the host's current key"
        );

        assert!(store.deny(&host).expect("denied"));
        let denied = store
            .approve_identity(&host, &key.key_hash(), "alice", NOW)
            .expect_err("denied");
        assert!(matches!(denied, IdentityError::Denied), "{label}");
        assert_eq!(denied.to_string(), "the host is denied");
        let order = store
            .approve_identity(&host, &other, "alice", NOW)
            .expect_err("denied and another key");
        assert!(matches!(order, IdentityError::KeyMismatch), "{label}");
        store.retire(&host).expect("retired");
        let retired = store
            .approve_identity(&host, &key.key_hash(), "alice", NOW)
            .expect_err("retired");
        assert!(matches!(retired, IdentityError::Denied), "{label}");
        assert_eq!(
            records
                .host(&host)
                .expect("read")
                .expect("the host")
                .identity_approval,
            None,
            "{label}: nothing was written"
        );
    }
}

/// Proves: a denied host is not allowed identity, whether an operator approved it, the key was
/// approved for identity, or neither; its state says denied, whether retiring or retired.
///
/// Catches: a retired host that keeps getting certificates because its approval stands.
#[test]
fn a_denied_host_is_not_allowed_identity() {
    for (label, store, _, _dir) in stores("identity-denied") {
        let (auto, by_hand) = (HostKey::new(), HostKey::new());
        enroll(
            &store,
            &auto,
            "m-1",
            "auto-1",
            Some(INVENTORY_APPROVER),
            false,
        );
        enroll(&store, &by_hand, "m-2", "hand-1", Some("ops"), false);
        store
            .approve_identity(&host_id("auto-1"), &auto.key_hash(), "alice", NOW)
            .expect("approved");
        for host in ["auto-1", "hand-1"] {
            assert!(store.identity_allowed(&host_id(host)).expect("read"));
            store.deny(&host_id(host)).expect("denied");
            assert!(
                !store.identity_allowed(&host_id(host)).expect("read"),
                "{label} {host} retiring"
            );
            assert!(
                store
                    .identity_state(&host_id(host))
                    .expect("read")
                    .expect("a host")
                    .denied
            );
            store.retire(&host_id(host)).expect("retired");
            assert!(
                !store.identity_allowed(&host_id(host)).expect("read"),
                "{label} {host} retired"
            );
        }
        assert!(
            !store.identity_allowed(&host_id("nobody-1")).expect("read"),
            "{label}: a host never approved"
        );
    }
}

/// Proves: an approval for another key than the host's counts as absent, and approving the
/// host's key then writes over it; and an approval of the host (a new key taking the id over)
/// writes the host without the identity approval, so the new key needs its own.
///
/// Catches: an approval of one key allowing the next key of the host id.
#[test]
fn an_approval_of_another_key_never_allows_the_host() {
    for (label, store, records, _dir) in stores("identity-key") {
        let first = HostKey::new();
        enroll(
            &store,
            &first,
            "m-1",
            "auto-1",
            Some(INVENTORY_APPROVER),
            false,
        );
        let host = host_id("auto-1");
        let mut stale = records.host(&host).expect("read").expect("the host");
        stale.identity_approval = Some(pb::IdentityApproval {
            key_hash: HostKey::new().key_hash().as_bytes().to_vec(),
            approver: "old".to_owned(),
            approved_unix: NOW - 1,
        });
        records.commit(&[Record::Host(stale)]).expect("stale row");
        let state = store.identity_state(&host).expect("read").expect("a host");
        assert_eq!(state.approval, None, "{label}");
        assert!(!store.identity_allowed(&host).expect("read"), "{label}");
        assert_eq!(
            store
                .approve_identity(&host, &first.key_hash(), "alice", NOW + 1)
                .expect("approved over the stale one"),
            IdentityApproved::Approved(approval(&first, "alice", NOW + 1)),
            "{label}"
        );
        assert!(store.identity_allowed(&host).expect("read"), "{label}");

        let second = HostKey::new();
        enroll(
            &store,
            &second,
            "m-1",
            "auto-1",
            Some(INVENTORY_APPROVER),
            true,
        );
        assert_eq!(
            records
                .host(&host)
                .expect("read")
                .expect("the host")
                .identity_approval,
            None,
            "{label}: the approval is written anew without it"
        );
        let state = store.identity_state(&host).expect("read").expect("a host");
        assert_eq!(
            (state.key_hash, state.approval),
            (second.key_hash(), None),
            "{label}"
        );
        assert!(!store.identity_allowed(&host).expect("read"), "{label}");
    }
}

/// Proves: an identity approval whose key hash is not 32 bytes is a corrupt record, refused when
/// the state is read and when an approval is asked for, never read as no approval.
///
/// Catches: a damaged approval read as absent, which would let the next call write over it.
#[test]
fn an_identity_approval_that_is_not_a_key_hash_is_corrupt() {
    for (label, store, records, _dir) in stores("identity-corrupt") {
        let key = HostKey::new();
        enroll(
            &store,
            &key,
            "m-1",
            "auto-1",
            Some(INVENTORY_APPROVER),
            false,
        );
        let host = host_id("auto-1");
        let mut damaged = records.host(&host).expect("read").expect("the host");
        damaged.identity_approval = Some(pb::IdentityApproval {
            key_hash: vec![1, 2, 3],
            approver: "alice".to_owned(),
            approved_unix: NOW,
        });
        records.commit(&[Record::Host(damaged)]).expect("damaged");
        let read = store.identity_state(&host).expect_err("refused");
        assert!(matches!(read, StoreError::Corrupt { .. }), "{label}");
        assert!(
            read.to_string()
                .ends_with("the identity approval's key is not 32 bytes"),
            "{label}: {read}"
        );
        assert!(store.identity_allowed(&host).is_err(), "{label}");
        let approve = store
            .approve_identity(&host, &key.key_hash(), "alice", NOW)
            .expect_err("refused");
        assert!(
            matches!(approve, IdentityError::Store(StoreError::Corrupt { .. })),
            "{label}"
        );
    }
}

/// Proves: the denied hosts are those whose deny row is retiring or retired, sorted, a deny row
/// of a host id never approved among them, and not a host whose deny row a new approval with
/// `replace` cleared; none while no host is denied.
///
/// Catches: a list that leaves a retired host out, includes a host the operator took back, or
/// lists in the order the rows were written.
#[test]
fn the_denied_hosts_are_the_retiring_and_the_retired_sorted() {
    for (label, store, _, _dir) in stores("identity-deny-list") {
        assert_eq!(store.denied_hosts().expect("listed"), vec![], "{label}");
        let keys: Vec<HostKey> = (0..3).map(|_| HostKey::new()).collect();
        for (key, host) in keys.iter().zip(["web-2", "web-1", "web-3"]) {
            enroll(&store, key, host, host, Some("ops"), false);
        }
        store.deny(&host_id("web-2")).expect("retiring");
        store.deny(&host_id("web-1")).expect("retiring");
        store.retire(&host_id("web-1")).expect("retired");
        store
            .retire(&host_id("zed-1"))
            .expect("a host never approved");
        assert_eq!(
            store.denied_hosts().expect("listed"),
            [host_id("web-1"), host_id("web-2"), host_id("zed-1")],
            "{label}"
        );
        let again = HostKey::new();
        enroll(&store, &again, "m-9", "web-2", Some("ops"), true);
        assert_eq!(
            store.denied_hosts().expect("listed"),
            [host_id("web-1"), host_id("zed-1")],
            "{label}: web-2 was taken back"
        );
    }
}

/// Issues the certificate of the approved request of `key`, which is what a renewal follows.
fn issue(store: &EnrollmentStore, ca: &TestCa, key: &HostKey) {
    store.issue(&ca.ca, &key.key_hash()).expect("issued");
}

/// Posts a renewal of `host` by `key` and approves it as the same host by `approver`.
fn renew(store: &EnrollmentStore, key: &HostKey, machine: &str, host: &str, approver: &str) {
    let id = store
        .post(renewal(key, machine, host), NOW + 10)
        .expect("renewal posted")
        .request_id;
    store
        .approve_recorded(
            &id,
            &key.key_hash(),
            &decision(host, false),
            &record(approver),
        )
        .expect("renewal approved");
}

/// Proves: a renewal of an auto host by the same key keeps the operator's approval of that key
/// (the host stays allowed, the approval is unchanged, and a store opened again reads it), and
/// the assurance after the renewal is that of the renewal's approver: `auto` when an inventory
/// record approved it, `operator` when an operator did.
///
/// Catches: an auto host that re-enrols with its own key losing its identity approval, so its
/// services get no certificates until an operator approves the same key again.
#[test]
fn a_renewal_with_the_same_key_keeps_the_identity_approval() {
    let ca = TestCa::new("identity-renewal-ca");
    for (label, store, records, _dir) in stores("identity-renewal") {
        let key = HostKey::new();
        enroll(
            &store,
            &key,
            "m-1",
            "auto-1",
            Some(INVENTORY_APPROVER),
            false,
        );
        let host = host_id("auto-1");
        store
            .approve_identity(&host, &key.key_hash(), "alice", NOW + 5)
            .expect("approved");

        issue(&store, &ca, &key);
        renew(&store, &key, "m-1", "auto-1", INVENTORY_APPROVER);
        let state = store.identity_state(&host).expect("read").expect("a host");
        assert_eq!(state.assurance, Assurance::Auto, "{label}");
        assert_eq!(
            state.approval,
            Some(approval(&key, "alice", NOW + 5)),
            "{label}"
        );
        assert!(store.identity_allowed(&host).expect("read"), "{label}");
        let reopened = EnrollmentStore::new(records.clone());
        assert!(reopened.identity_allowed(&host).expect("read"), "{label}");

        issue(&store, &ca, &key);
        renew(&store, &key, "m-1", "auto-1", "ops");
        let state = store.identity_state(&host).expect("read").expect("a host");
        assert_eq!(state.assurance, Assurance::Operator, "{label}");
        assert_eq!(
            state.approval,
            Some(approval(&key, "alice", NOW + 5)),
            "{label}"
        );
        assert!(store.identity_allowed(&host).expect("read"), "{label}");
    }
}

/// Proves: a renewal of a host that holds no identity approval leaves it without one, and a
/// replace by a new key of a host that holds one writes it without.
///
/// Catches: a renewal inventing an approval, and a new key inheriting the old key's.
#[test]
fn a_renewal_invents_no_identity_approval_and_a_replace_clears_it() {
    let ca = TestCa::new("identity-renewal-none-ca");
    for (label, store, records, _dir) in stores("identity-renewal-none") {
        let key = HostKey::new();
        enroll(
            &store,
            &key,
            "m-1",
            "auto-1",
            Some(INVENTORY_APPROVER),
            false,
        );
        let host = host_id("auto-1");
        issue(&store, &ca, &key);
        renew(&store, &key, "m-1", "auto-1", INVENTORY_APPROVER);
        assert!(!store.identity_allowed(&host).expect("read"), "{label}");

        store
            .approve_identity(&host, &key.key_hash(), "alice", NOW + 5)
            .expect("approved");
        let next = HostKey::new();
        enroll(
            &store,
            &next,
            "m-1",
            "auto-1",
            Some(INVENTORY_APPROVER),
            true,
        );
        let held = records.host(&host).expect("read").expect("the host");
        assert_eq!(held.identity_approval, None, "{label}");
        assert!(!store.identity_allowed(&host).expect("read"), "{label}");
    }
}
