//! Crashes and the order of writes, through the fault-injecting file system: a store stopped
//! between any two operations, then reopened, never hands out a second certificate for one
//! request; and every file is written by temporary file, fsync, rename, directory fsync.

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_enrollment::ca::{Ca, CaError};
use hostkeeper_enrollment::enroll::{
    EnrollmentStore, IssueError, IssuedCertificate, RequestState, StoreError,
};
use hostkeeper_enrollment::fs::{FaultFs, Fs, Op, RealFs};
use hostkeeper_enrollment::names::KeyHash;
use support::{FixedClock, HostKey, NOW, TestCa, decision, files, host_id, request, scratch};

/// A store over a fault-injecting file system in its own scratch directory, holding one
/// approved request.
struct Approved {
    dir: hostkeeper_testkit::Scratch,
    fs: Arc<FaultFs>,
    store: EnrollmentStore,
    id: KeyHash,
}

impl Approved {
    fn new(name: &str) -> Approved {
        let dir = scratch(name);
        let fs = Arc::new(FaultFs::new());
        let store = EnrollmentStore::open(fs.clone(), dir.path()).expect("open");
        let key = HostKey::new();
        let id = store
            .post(request(&key, "m-1"), NOW)
            .expect("post")
            .request_id;
        store
            .approve(&id, &id, &decision("web-1", false))
            .expect("approve");
        Approved { dir, fs, store, id }
    }
}

/// The number (counted from 1, from `base`) of the first operation in `ops` after `base` that
/// `wanted` picks.
fn find(ops: &[Op], base: u64, wanted: impl Fn(&Op) -> bool) -> u64 {
    let skip = usize::try_from(base).expect("small");
    let at = ops[skip..]
        .iter()
        .position(wanted)
        .expect("the operation happens");
    base + 1 + u64::try_from(at).expect("small")
}

fn under(op: &Op, sub: &str) -> bool {
    match op {
        Op::WriteNew(path) => path.parent().is_some_and(|dir| dir.ends_with(sub)),
        _ => false,
    }
}

/// Catches a second serial (and so a second certificate) being minted for one request when
/// issuance stops after the certificate file is written and before the request is marked
/// issued, or before the file is written; a retry that does not return the same bytes on
/// every call; and a first issue told more than once, or not at all: the call that signs, or
/// that records a certificate signed before the crash, is the first, and no later call is.
#[test]
fn a_crash_between_approval_and_issue_yields_one_certificate() {
    let ca = TestCa::new("crash-ca");
    // A run without a fault shows where each write falls; its first call is the first issue.
    let probe = Approved::new("crash-probe");
    let base = probe.fs.count();
    assert!(probe.store.issue(&ca.ca, &probe.id).expect("issue").first);
    let ops = probe.fs.ops();
    let issued_write = find(&ops, base, |op| under(op, "issued"));
    let marked_write = find(&ops, issued_write, |op| under(op, "requests"));
    let reserve_write = find(&ops, base, |op| under(op, "requests"));
    assert!(reserve_write < issued_write && issued_write < marked_write);

    // After the file, before the request is marked; and before the file.
    for (name, offset) in [
        ("crash-after-file", marked_write - base),
        ("crash-before-file", issued_write - base),
    ] {
        let run = Approved::new(name);
        run.fs.fail_at(run.fs.count() + offset);
        let failed = run.store.issue(&ca.ca, &run.id).expect_err("stopped");
        assert!(
            matches!(failed, IssueError::Store(StoreError::Io { .. })),
            "{name}: {failed}"
        );
        let reopened = EnrollmentStore::open(Arc::new(RealFs), run.dir.path()).expect("reopen");
        let first = reopened.issue(&ca.ca, &run.id).expect("retry");
        let second = reopened.issue(&ca.ca, &run.id).expect("again");
        assert!(first.first, "{name}: the retry is the first issue");
        assert!(!second.first, "{name}: a later call is not");
        assert_eq!(
            IssuedCertificate {
                first: true,
                ..second
            },
            first,
            "{name}: the same certificate"
        );
        assert_eq!(first.serial, 2, "{name}: the reserved serial");
        assert_eq!(reopened.allocate_serial().expect("serial"), 3, "{name}");
        let view = reopened.request(&run.id).expect("read").expect("held");
        assert_eq!(view.state, RequestState::Issued, "{name}");
        let issued = std::fs::read_dir(run.dir.path().join("issued")).expect("list");
        let names: Vec<String> = issued
            .map(|e| e.expect("entry").file_name().into_string().expect("UTF-8"))
            .collect();
        assert_eq!(
            names,
            [format!("{}.pb", run.id)],
            "{name}: one file, no stray"
        );
    }
}

/// Catches a deny that stopped before the host's request was marked denied letting the
/// request be issued: issuance checks the deny row itself.
#[test]
fn a_deny_cut_short_still_stops_issuance() {
    let ca = TestCa::new("deny-cut-ca");
    let run = Approved::new("deny-cut");
    let base = run.fs.count();
    let probe = Approved::new("deny-cut-probe");
    let probe_base = probe.fs.count();
    probe.store.deny(&host_id("web-1")).expect("deny");
    let mark = find(&probe.fs.ops(), probe_base, |op| under(op, "requests"));
    run.fs.fail_at(base + (mark - probe_base));
    let failed = run.store.deny(&host_id("web-1")).expect_err("stopped");
    assert!(matches!(failed, StoreError::Io { .. }), "{failed}");
    assert!(run.store.is_denied(&host_id("web-1")).expect("read"));
    let view = run.store.request(&run.id).expect("read").expect("held");
    assert_eq!(view.state, RequestState::Approved);
    let refused = run.store.issue(&ca.ca, &run.id).expect_err("denied");
    assert!(matches!(refused, IssueError::Denied), "{refused}");
}

/// The operations that follow each `WriteNew` in `ops`, checked to be the temporary file's
/// fsync, its rename over the name without `.tmp`, and the fsync of that directory. Returns the
/// final paths written.
fn written(ops: &[Op]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for (at, op) in ops.iter().enumerate() {
        if let Op::WriteNew(tmp) = op {
            let name = tmp.file_name().expect("a name").to_str().expect("UTF-8");
            let final_name = name.strip_suffix(".tmp").expect("a temporary file");
            let target = tmp.with_file_name(final_name);
            let dir = tmp.parent().expect("a directory").to_path_buf();
            assert_eq!(
                ops[at + 1..at + 4],
                [
                    Op::SyncFile(tmp.clone()),
                    Op::Rename(tmp.clone(), target.clone()),
                    Op::SyncDir(dir),
                ],
                "after writing {}",
                tmp.display()
            );
            out.push(target);
        }
    }
    let renames = ops.iter().filter(|op| matches!(op, Op::Rename(..))).count();
    assert_eq!(renames, out.len(), "every rename is the end of a write");
    out
}

/// Catches a store or CA file written in place, renamed before its fsync, or left without the
/// directory fsync that makes the rename durable.
#[test]
fn every_store_write_is_temp_fsync_rename() {
    let dir = scratch("write-order");
    let fs = Arc::new(FaultFs::new());
    let clock = FixedClock::at(NOW);
    let ca = Ca::init(&*fs, files(dir.path()), clock).expect("make the CA");
    let store_dir = dir.path().join("store");
    let store = EnrollmentStore::open(fs.clone(), &store_dir).expect("open");
    let key = HostKey::new();
    let id = store
        .post(request(&key, "m-1"), NOW)
        .expect("post")
        .request_id;
    store
        .post(request(&HostKey::new(), "m-1"), NOW)
        .expect("a duplicate claim");
    store
        .approve(&id, &id, &decision("web-1", false))
        .expect("approve");
    store.issue(&ca, &id).expect("issue");
    store.allocate_serial().expect("serial");
    store
        .report_live(&host_id("web-1"), "m-1", NOW)
        .expect("live");
    store
        .report_live(&host_id("web-1"), "m-2", NOW)
        .expect("duplicate");
    store.deny(&host_id("web-1")).expect("deny");
    store.retire(&host_id("web-1")).expect("retire");
    let paths = written(&fs.ops());
    let in_dir = |sub: &Path| paths.iter().filter(|path| path.starts_with(sub)).count();
    assert_eq!(in_dir(&store_dir.join("requests")), 7);
    assert_eq!(in_dir(&store_dir.join("issued")), 1);
    assert_eq!(in_dir(&store_dir.join("hosts")), 2);
    assert_eq!(in_dir(&store_dir.join("deny")), 2);
    assert_eq!(
        paths
            .iter()
            .filter(|path| path.ends_with("serial.pb"))
            .count(),
        2
    );
    assert!(paths.contains(&dir.path().join("ca.key")));
    assert!(paths.contains(&dir.path().join("ca.cert")));
    for mode_of in [&dir.path().join("ca.key"), &store_dir.join("serial.pb")] {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(mode_of)
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "{}", mode_of.display());
    }
    // Every mutation took the lock file.
    let locks = fs
        .ops()
        .iter()
        .filter(|op| matches!(op, Op::Lock(_)))
        .count();
    assert_eq!(locks, 9);
}

/// Catches a failure of the file system being swallowed: each operation of each step fails in
/// turn, and every step reports it as an error naming the path.
#[test]
fn every_file_system_failure_is_an_error() {
    let ca = TestCa::new("failures-ca");
    // Opening the store: each directory.
    for n in 1..=4 {
        let dir = scratch("fail-open");
        let fs = Arc::new(FaultFs::new());
        fs.fail_at(n);
        let err = EnrollmentStore::open(fs, dir.path()).expect_err("fails");
        assert!(matches!(err, StoreError::Io { .. }), "{err}");
    }
    // Each operation of a post (a duplicate claim lists the requests), an approval, an issue,
    // a deny, a live report and a serial: every one fails as an error, never a panic or a
    // silent success.
    let probe = Approved::new("fail-probe");
    let base = probe.fs.count();
    probe.store.issue(&ca.ca, &probe.id).expect("issue");
    probe
        .store
        .report_live(&host_id("web-1"), "m-1", NOW)
        .expect("live");
    probe
        .store
        .report_live(&host_id("web-1"), "m-2", NOW)
        .expect("live");
    probe.store.allocate_serial().expect("serial");
    probe
        .store
        .post(request(&HostKey::new(), "m-1"), NOW)
        .expect("post");
    probe.store.deny(&host_id("web-1")).expect("deny");
    let total = probe.fs.count() - base;
    for n in 1..=total {
        let run = Approved::new("fail-run");
        let start = run.fs.count();
        run.fs.fail_at(start + n);
        let web = host_id("web-1");
        let results = [
            run.store
                .issue(&ca.ca, &run.id)
                .err()
                .map(|e| e.to_string()),
            run.store
                .report_live(&web, "m-1", NOW)
                .err()
                .map(|e| e.to_string()),
            run.store
                .report_live(&web, "m-2", NOW)
                .err()
                .map(|e| e.to_string()),
            run.store.allocate_serial().err().map(|e| e.to_string()),
            run.store
                .post(request(&HostKey::new(), "m-1"), NOW)
                .err()
                .map(|e| e.to_string()),
            run.store.deny(&web).err().map(|e| e.to_string()),
        ];
        let failed: Vec<&String> = results.iter().flatten().collect();
        assert_eq!(failed.len(), 1, "operation {n}: {results:?}");
        assert!(
            failed[0].contains("injected failure at operation"),
            "operation {n}: {}",
            failed[0]
        );
    }
    // The reads of approval and of the poll check.
    let run = Approved::new("fail-reads");
    let other = KeyHash::of_spki(b"another");
    run.fs.fail_at(run.fs.count() + 1);
    let err = run
        .store
        .approve(&other, &other, &decision("web-2", false))
        .expect_err("lock");
    assert!(err.token().is_none(), "{err}");
    run.fs.fail_at(run.fs.count() + 1);
    let err = run
        .store
        .verify_poll(&run.id, &[0; 16], b"x")
        .expect_err("read");
    assert!(err.token().is_none(), "{err}");
    run.fs.fail_at(run.fs.count() + 1);
    assert!(run.store.is_denied(&host_id("web-1")).is_err());
    run.fs.fail_at(run.fs.count() + 1);
    assert!(run.store.pending().is_err());
    run.fs.fail_at(run.fs.count() + 1);
    assert!(run.store.request(&run.id).is_err());
}

/// Catches the CA reporting success when one of its files was not written or read.
#[test]
fn a_ca_file_failure_is_an_error() {
    let clock = FixedClock::at(NOW);
    let probe_dir = scratch("ca-fail-probe");
    let probe = FaultFs::new();
    Ca::init(&probe, files(probe_dir.path()), clock.clone()).expect("init");
    let init_ops = probe.count();
    for n in 1..=init_ops {
        let dir = scratch("ca-fail");
        let fs = FaultFs::new();
        fs.fail_at(n);
        let err = Ca::init(&fs, files(dir.path()), clock.clone()).expect_err("fails");
        assert!(matches!(err, CaError::Io { .. }), "operation {n}: {err}");
    }
    let before = probe.count();
    Ca::open(&probe, files(probe_dir.path()), clock.clone()).expect("open");
    for n in 1..=probe.count() - before {
        let fs = FaultFs::new();
        fs.fail_at(n);
        let err = Ca::open(&fs, files(probe_dir.path()), clock.clone()).expect_err("fails");
        assert!(matches!(err, CaError::Io { .. }), "operation {n}: {err}");
    }
    // A key path in a directory that does not exist fails as an error of the write.
    let dir = scratch("ca-fail-dir");
    let nowhere = hostkeeper_enrollment::ca::CaFiles {
        key: &dir.path().join("missing/ca.key"),
        certificate: &dir.path().join("ca.cert"),
    };
    let err = Ca::init(&RealFs, nowhere, clock).expect_err("no directory");
    assert!(err.to_string().starts_with("cannot use "), "{err}");
    let fs: &dyn Fs = &RealFs;
    assert!(fs.read(&dir.path().join("absent")).expect("read").is_none());
}
