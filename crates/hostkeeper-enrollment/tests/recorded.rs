//! The pending cap, the recorded approval (who, when, why; a duplicate claim needs `replace`)
//! and Mac nonces consumed in the approval's own write, over a scratch directory, with the
//! fault-injecting file system where a write must fail.

mod support;

use std::path::Path;
use std::sync::Arc;

use hostkeeper_enrollment::enroll::{
    ApprovalRecord, ApproveError, Claims, DEFAULT_MAX_PENDING, EnrollmentStore,
    MAC_NONCE_VALID_SECS, MacNonceHash, MacNonceRefusal, NewRequest, PostError, RecordedApproval,
    RequestState,
};
use hostkeeper_enrollment::fs::{FaultFs, Fs, Op, RealFs};
use hostkeeper_enrollment::names::{KeyHash, Role};
use support::{HostKey, NOW, claims, decision, host_id, renewal, request, scratch};

/// The serial number of the Mac in these tests.
const SERIAL: &str = "C02XK1ABJG5J";

fn open(fs: Arc<dyn Fs>, dir: &Path) -> EnrollmentStore {
    EnrollmentStore::open(fs, dir).expect("open")
}

/// A Mac's request: its claims carry `serial`, and it presented a nonce.
fn mac_request(key: &HostKey, machine_id: &str, serial: &str) -> NewRequest {
    NewRequest {
        claims: Claims {
            mac_serial: serial.to_owned(),
            ..claims(machine_id)
        },
        mac_nonce_presented: true,
        ..request(key, machine_id)
    }
}

/// The record of an approval by `who` at `at`, consuming `nonce` if any.
fn record(who: &str, at: u64, nonce: Option<MacNonceHash>) -> ApprovalRecord {
    ApprovalRecord {
        approver: who.to_owned(),
        approved_unix: at,
        reason: "new rack".to_owned(),
        mac_nonce: nonce,
        mac_enrolled_since_unix: None,
    }
}

/// Posts `key`'s request and approves it as `host`, recorded by `who` with `nonce`.
fn approve_mac(
    store: &EnrollmentStore,
    key: &HostKey,
    host: &str,
    nonce: MacNonceHash,
    at: u64,
) -> Result<KeyHash, ApproveError> {
    let id = store
        .post(mac_request(key, host, SERIAL), NOW)
        .expect("post")
        .request_id;
    store
        .approve_recorded(
            &id,
            &id,
            &decision(host, false),
            &record("mdm-nonce", at, Some(nonce)),
        )
        .map(|_| id)
}

/// Catches the newest request evicting an older one, the cap counting approved requests, a
/// repost of a waiting request being refused as new, and the refusal writing anything.
#[test]
fn a_full_pending_queue_keeps_existing_requests() {
    let dir = scratch("cap");
    let store = open(Arc::new(RealFs), dir.path()).with_max_pending(3);
    assert_eq!(store.max_pending(), 3);
    assert_eq!(
        open(Arc::new(RealFs), dir.path()).max_pending(),
        DEFAULT_MAX_PENDING
    );
    assert_eq!(DEFAULT_MAX_PENDING, 256);
    let keys: Vec<HostKey> = (0..5).map(|_| HostKey::new()).collect();
    for (n, key) in keys.iter().take(3).enumerate() {
        store
            .post(request(key, &format!("m-{n}")), NOW + n as u64)
            .expect("post");
    }
    let before = store.pending().expect("pending");
    let refused = store
        .post(request(&keys[3], "m-3"), NOW + 10)
        .expect_err("full");
    assert!(matches!(refused, PostError::PendingFull), "{refused}");
    assert_eq!(refused.token(), Some("pending_full"));
    assert_eq!(store.pending().expect("pending"), before, "nothing evicted");
    assert!(store.request(&keys[3].key_hash()).expect("read").is_none());

    // A repost of a waiting request is the same request, full or not.
    let again = store
        .post(request(&keys[0], "m-0"), NOW + 20)
        .expect("repost");
    assert!(again.reposted);
    assert_eq!(store.pending().expect("pending"), before);

    // An approved request no longer counts.
    let first = keys[0].key_hash();
    store
        .approve(&first, &first, &decision("web-1", false))
        .expect("approve");
    store
        .post(request(&keys[3], "m-3"), NOW + 30)
        .expect("room after an approval");
    let refused = store
        .post(request(&keys[4], "m-4"), NOW + 40)
        .expect_err("full again");
    assert!(matches!(refused, PostError::PendingFull), "{refused}");
}

/// Catches a renewal round that starts while the queue is full: it would wait like any new
/// request.
#[test]
fn a_renewal_round_counts_against_the_cap() {
    let dir = scratch("cap-renewal");
    let ca = support::TestCa::new("cap-renewal-ca");
    let store = open(Arc::new(RealFs), dir.path()).with_max_pending(1);
    let key = HostKey::new();
    let id = store
        .post(request(&key, "m-1"), NOW)
        .expect("post")
        .request_id;
    store
        .approve(&id, &id, &decision("web-1", false))
        .expect("approve");
    store.issue(&ca.ca, &id).expect("issue");
    store
        .post(request(&HostKey::new(), "m-2"), NOW)
        .expect("one waits");
    let refused = store
        .post(renewal(&key, "m-1", "web-1"), NOW)
        .expect_err("full");
    assert!(matches!(refused, PostError::PendingFull), "{refused}");
}

/// Catches a recorded approval admitting a duplicate claim without `replace`, writing anything
/// when it refuses, and leaving out who approved, when and why.
#[test]
fn a_recorded_approval_needs_replace_for_a_duplicate_claim_and_keeps_its_record() {
    let dir = scratch("recorded");
    let store = open(Arc::new(RealFs), dir.path());
    let (first, second) = (HostKey::new(), HostKey::new());
    let one = store.post(request(&first, "m-1"), NOW).expect("post");
    let two = store.post(request(&second, "m-1"), NOW).expect("post");
    assert_eq!(two.state, RequestState::DuplicateClaim);
    let id = two.request_id;
    let refused = store
        .approve_recorded(
            &id,
            &id,
            &decision("web-2", false),
            &record("alice", NOW, None),
        )
        .expect_err("duplicate claim");
    assert!(matches!(refused, ApproveError::DuplicateClaim), "{refused}");
    assert_eq!(refused.token(), Some("duplicate_claim"));
    assert_eq!(
        store.request(&id).expect("read").expect("held").state,
        RequestState::DuplicateClaim
    );
    assert_eq!(store.host_role(&host_id("web-2")).expect("read"), None);
    assert!(store.recorded_approvals().expect("read").is_empty());

    store
        .approve_recorded(
            &id,
            &id,
            &decision("web-2", true),
            &record("alice", NOW + 5, None),
        )
        .expect("replace");
    // An approval without a record is not one to audit.
    store
        .approve(&one.request_id, &one.request_id, &decision("web-1", false))
        .expect("approve");
    assert_eq!(
        store.recorded_approvals().expect("read"),
        [RecordedApproval {
            request_id: id,
            host_id: "web-2".to_owned(),
            role: "web".to_owned(),
            services: vec!["api".to_owned()],
            replace: true,
            approver: "alice".to_owned(),
            approved_unix: NOW + 5,
            reason: "new rack".to_owned(),
            claims: claims("m-1"),
            mac_nonce: false,
        }]
    );
    // Its key hash is compared whole.
    let mut near = *one.request_id.as_bytes();
    near[31] ^= 1;
    let near = KeyHash::from_bytes(&near).expect("32 bytes");
    let third = store
        .post(request(&HostKey::new(), "m-3"), NOW)
        .expect("post");
    let refused = store
        .approve_recorded(
            &third.request_id,
            &near,
            &decision("web-3", false),
            &record("alice", NOW, None),
        )
        .expect_err("another key");
    assert!(matches!(refused, ApproveError::KeyMismatch), "{refused}");
}

/// Catches a certificate of the key a host id was taken from still naming the host.
#[test]
fn a_host_role_is_read_only_for_the_key_the_host_holds() {
    let dir = scratch("role-for");
    let store = open(Arc::new(RealFs), dir.path());
    let (old, new) = (HostKey::new(), HostKey::new());
    for (key, machine) in [(&old, "m-1"), (&new, "m-2")] {
        store.post(request(key, machine), NOW).expect("post");
    }
    let web = host_id("web-1");
    let role = Some(Role::parse("web").expect("a role"));
    store
        .approve(&old.key_hash(), &old.key_hash(), &decision("web-1", false))
        .expect("approve");
    assert_eq!(
        store.host_role_for(&web, &old.key_hash()).expect("read"),
        role
    );
    assert_eq!(
        store.host_role_for(&web, &new.key_hash()).expect("read"),
        None
    );
    store
        .approve(&new.key_hash(), &new.key_hash(), &decision("web-1", true))
        .expect("replace");
    assert_eq!(
        store.host_role_for(&web, &old.key_hash()).expect("read"),
        None
    );
    assert_eq!(
        store.host_role_for(&web, &new.key_hash()).expect("read"),
        role
    );
    assert_eq!(
        store
            .host_role_for(&host_id("web-9"), &new.key_hash())
            .expect("read"),
        None
    );
}

/// Catches a request's Mac nonce flag lost, or set on a request no longer waiting.
#[test]
fn a_request_says_whether_it_presented_a_mac_nonce() {
    let dir = scratch("presented");
    let store = open(Arc::new(RealFs), dir.path());
    let key = HostKey::new();
    let id = store
        .post(request(&key, "m-1"), NOW)
        .expect("post")
        .request_id;
    let flag = |store: &EnrollmentStore| {
        store
            .request(&id)
            .expect("read")
            .expect("held")
            .mac_nonce_presented
    };
    assert!(!flag(&store));
    store
        .post(mac_request(&key, "m-1", SERIAL), NOW)
        .expect("repost");
    assert!(flag(&store), "a repost with a nonce marks it");
    store
        .post(mac_request(&key, "m-1", SERIAL), NOW)
        .expect("repost");
    assert!(flag(&store), "a second one leaves it marked");
    store.post(request(&key, "m-1"), NOW).expect("repost");
    assert!(flag(&store), "a repost without one does not unmark it");

    let other = HostKey::new();
    let late = store
        .post(request(&other, "m-2"), NOW)
        .expect("post")
        .request_id;
    store
        .approve(&late, &late, &decision("web-2", false))
        .expect("approve");
    store
        .post(mac_request(&other, "m-2", SERIAL), NOW)
        .expect("repost");
    assert!(
        !store
            .request(&late)
            .expect("read")
            .expect("held")
            .mac_nonce_presented,
        "an approved request is not marked"
    );
}

/// Catches a nonce approving when unknown, expired, used or issued for another serial, being
/// accepted past one hour or refused within it, a nonce issued twice, and a hash shown by
/// `Debug`.
#[test]
fn a_mac_nonce_approves_once_within_an_hour_for_its_serial() {
    let dir = scratch("nonce");
    let store = open(Arc::new(RealFs), dir.path());
    let nonce = MacNonceHash::of(b"nonce-one-0123456789abcdef012345");
    // The key the nonce is issued for: the one that approves at the last second of the hour.
    let bound = HostKey::new();
    let for_bound = bound.key_hash();
    assert!(
        store
            .issue_mac_nonce(SERIAL, &for_bound, &nonce, NOW)
            .expect("issue")
    );
    assert!(
        !store
            .issue_mac_nonce(SERIAL, &for_bound, &nonce, NOW)
            .expect("again")
    );
    assert_eq!(format!("{nonce:?}"), "MacNonceHash(..)");

    let refusal = |result: Result<KeyHash, ApproveError>| match result {
        Err(ApproveError::MacNonce(refusal)) => refusal,
        other => panic!("not a nonce refusal: {other:?}"),
    };
    let unknown = MacNonceHash::of(b"never issued");
    assert_eq!(
        refusal(approve_mac(&store, &HostKey::new(), "mac-1", unknown, NOW)),
        MacNonceRefusal::Unknown
    );
    let late = NOW + MAC_NONCE_VALID_SECS + 1;
    assert_eq!(
        refusal(approve_mac(&store, &HostKey::new(), "mac-2", nonce, late)),
        MacNonceRefusal::Expired
    );
    let key = HostKey::new();
    let id = store
        .post(mac_request(&key, "m-x", "OTHERSERIAL1"), NOW)
        .expect("post")
        .request_id;
    let wrong = store
        .approve_recorded(
            &id,
            &id,
            &decision("mac-3", false),
            &record("mdm-nonce", NOW, Some(nonce)),
        )
        .expect_err("another serial");
    assert_eq!(wrong.token(), Some("nonce_wrong_serial"));

    // The last second of the hour approves; the nonce is then used.
    let at = NOW + MAC_NONCE_VALID_SECS;
    let approved = approve_mac(&store, &bound, "mac-4", nonce, at).expect("approved");
    assert_eq!(
        store.request(&approved).expect("read").expect("held").state,
        RequestState::Approved
    );
    assert_eq!(
        refusal(approve_mac(&store, &HostKey::new(), "mac-5", nonce, at)),
        MacNonceRefusal::Used
    );
    let tokens = [
        MacNonceRefusal::Unknown,
        MacNonceRefusal::Expired,
        MacNonceRefusal::Used,
        MacNonceRefusal::WrongSerial,
        MacNonceRefusal::WrongRequest,
    ]
    .map(|refusal| {
        (
            refusal.token(),
            refusal.to_string().starts_with(refusal.token()),
        )
    });
    assert_eq!(
        tokens,
        [
            ("nonce_unknown", true),
            ("nonce_expired", true),
            ("nonce_used", true),
            ("nonce_wrong_serial", true),
            ("nonce_wrong_request", true)
        ]
    );
    let recorded = store.recorded_approvals().expect("read");
    assert_eq!(recorded.len(), 1);
    assert!(recorded[0].mac_nonce);
    // The files hold the hash, never the nonce.
    for entry in walk(dir.path()) {
        let bytes = std::fs::read(&entry).expect("read");
        assert!(
            !bytes.windows(16).any(|w| w == b"nonce-one-012345"),
            "{}",
            entry.display()
        );
    }
}

/// Every file under `dir`.
fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).expect("list") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}

/// The number, counted from 1 since `fs` was made, of the first operation after `base` that
/// writes under the subdirectory `sub`.
fn write_under(fs: &FaultFs, base: u64, sub: &str) -> u64 {
    let ops = fs.ops();
    let skip = usize::try_from(base).expect("small");
    let at = ops[skip..]
        .iter()
        .position(|op| match op {
            Op::WriteNew(path) => path.parent().is_some_and(|dir| dir.ends_with(sub)),
            _ => false,
        })
        .expect("the write happens");
    base + 1 + u64::try_from(at).expect("small")
}

/// A store over a fault-injecting file system with a nonce issued for [`SERIAL`] and the Mac's
/// request posted: the directory, the file system, the store, the request and the nonce.
struct MacRun {
    dir: hostkeeper_testkit::Scratch,
    fs: Arc<FaultFs>,
    store: EnrollmentStore,
    id: KeyHash,
    nonce: MacNonceHash,
}

impl MacRun {
    fn new(name: &str) -> MacRun {
        let dir = scratch(name);
        let fs = Arc::new(FaultFs::new());
        let store = open(fs.clone(), dir.path());
        let nonce = MacNonceHash::of(name.as_bytes());
        let key = HostKey::new();
        assert!(
            store
                .issue_mac_nonce(SERIAL, &key.key_hash(), &nonce, NOW)
                .expect("issue")
        );
        let id = store
            .post(mac_request(&key, "m-1", SERIAL), NOW)
            .expect("post")
            .request_id;
        MacRun {
            dir,
            fs,
            store,
            id,
            nonce,
        }
    }

    fn approve(&self) -> Result<(), ApproveError> {
        self.store
            .approve_recorded(
                &self.id,
                &self.id,
                &decision("mac-1", false),
                &record("mdm-nonce", NOW, Some(self.nonce)),
            )
            .map(|_| ())
    }

    /// The store opened again over the real file system: what a restart reads.
    fn reopen(&self) -> EnrollmentStore {
        open(Arc::new(RealFs), self.dir.path())
    }
}

/// Whether `store` holds the request `id` approved, and whether the nonce is still unused now:
/// presented by another Mac request, it is refused only for being another request's.
fn after(store: &EnrollmentStore, id: &KeyHash, nonce: MacNonceHash) -> (bool, bool) {
    let approved = store.request(id).expect("read").expect("held").state == RequestState::Approved;
    let free = matches!(
        approve_mac(store, &HostKey::new(), "mac-9", nonce, NOW),
        Err(ApproveError::MacNonce(MacNonceRefusal::WrongRequest))
    );
    (approved, free)
}

/// Catches a nonce consumed in a write of its own: a crash at the approval's write leaves
/// neither the approval nor the consumption, a crash at the nonce's own mark leaves both
/// (the approval holds the consumption, and the reopen marks the file), and a restart after
/// either reads the same.
#[test]
fn a_nonce_is_consumed_with_the_approval_across_a_restart() {
    let probe = MacRun::new("crash-probe");
    let base = probe.fs.count();
    probe.approve().expect("approve");
    let commit = write_under(&probe.fs, base, "requests") - base;
    let mark = write_under(&probe.fs, base, "mac-nonces") - base;
    assert!(
        commit < mark,
        "the approval is written before the nonce is marked"
    );
    assert_eq!(
        after(&probe.reopen(), &probe.id, probe.nonce),
        (true, false)
    );

    // The approval's write fails: nothing is approved and the nonce is free.
    let run = MacRun::new("crash-commit");
    run.fs.fail_at(run.fs.count() + commit);
    let failed = run.approve().expect_err("the write fails");
    assert!(failed.token().is_none(), "{failed}");
    assert_eq!(after(&run.reopen(), &run.id, run.nonce), (false, true));

    // The nonce's mark fails: the approval stands and the nonce is used, before and after a
    // restart, which marks the file.
    let run = MacRun::new("crash-mark");
    run.fs.fail_at(run.fs.count() + mark);
    run.approve().expect("the approval stands");
    let used = approve_mac(&run.store, &HostKey::new(), "mac-8", run.nonce, NOW)
        .expect_err("used in memory");
    assert_eq!(used.token(), Some("nonce_used"));
    let reopened = run.reopen();
    let marked = walk(&run.dir.path().join("mac-nonces"));
    assert_eq!(marked.len(), 1);
    assert_eq!(after(&reopened, &run.id, run.nonce), (true, false));
    assert!(
        std::fs::read(&marked[0])
            .expect("read")
            .windows(32)
            .any(|w| w == run.id.as_bytes()),
        "the file names the request that consumed it"
    );

    // Every operation of a reopen that marks a file fails as an error, never as a store that
    // forgot the consumption.
    let run = MacRun::new("crash-reopen");
    run.fs.fail_at(run.fs.count() + mark);
    run.approve().expect("the approval stands");
    let probe_fs = Arc::new(FaultFs::new());
    drop(open(probe_fs.clone(), run.dir.path()));
    let reopen_ops = probe_fs.count();
    assert!(reopen_ops > 5, "the reopen marked the file");
    for n in 1..=reopen_ops {
        let again = MacRun::new("crash-reopen-each");
        again.fs.fail_at(again.fs.count() + mark);
        again.approve().expect("the approval stands");
        let fs = Arc::new(FaultFs::new());
        fs.fail_at(n);
        assert!(
            EnrollmentStore::open(fs, again.dir.path()).is_err(),
            "operation {n}"
        );
        assert_eq!(
            after(&again.reopen(), &again.id, again.nonce),
            (true, false)
        );
    }

    // A consumed nonce whose file was removed by hand is unknown after a reopen.
    for file in walk(&run.dir.path().join("mac-nonces")) {
        std::fs::remove_file(file).expect("remove");
    }
    let removed = run.reopen();
    let refused =
        approve_mac(&removed, &HostKey::new(), "mac-7", run.nonce, NOW).expect_err("unknown");
    assert_eq!(refused.token(), Some("nonce_unknown"));
}

/// Catches a nonce whose file says it was consumed being taken as unused when no approval in
/// the store names it (its request file removed by hand): the file alone makes it used.
#[test]
fn a_nonce_file_marked_consumed_is_used() {
    use std::io::Write as _;

    let dir = scratch("nonce-marked");
    let store = open(Arc::new(RealFs), dir.path());
    let nonce = MacNonceHash::of(b"nonce-marked-0123456789abcdef012");
    let key = HostKey::new();
    assert!(
        store
            .issue_mac_nonce(SERIAL, &key.key_hash(), &nonce, NOW)
            .expect("issue")
    );
    let path = dir
        .path()
        .join("mac-nonces")
        .join(format!("{}.pb", hex::encode(nonce.as_bytes())));
    // Field 5, `consumed_by`, one byte long.
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .and_then(|mut file| file.write_all(&[0x2a, 0x01, 0x01]))
        .expect("appended");
    let refused = approve_mac(&store, &key, "mac-1", nonce, NOW).expect_err("used");
    assert_eq!(refused.token(), Some("nonce_used"));
}
