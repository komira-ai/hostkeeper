//! The store module's own parts: a commit across two backends, the errors as each store says
//! them, the files with no part, and CI's spent ids over a backend.

use std::sync::Arc;

use hostkeeper_oidc::SpentJtis;

use super::conformance_tests::{desired_at, lines, rich_verdicts};
use super::fake::store_failure;
use super::{Backend, BackendError, Commit, Fake, Files, Loaded, Shared, Spent, commit_across};
use crate::audit_file::{DEFAULT_ROTATION, Query};
use crate::config::StartError;
use crate::desired::{self, DesiredStore, StoreError};
use crate::jti_file::{JtiError, jti_path};
use crate::ops::{OpsError, OpsState, OpsStore};
use crate::testutil::{LEGACY, scratch, version};
use crate::verdict_file::{self, VerdictFile, VerdictsError};

/// An I/O error of a file operation at `path`.
fn io_at(path: &str) -> StoreError {
    StoreError::Io {
        path: path.into(),
        source: std::io::Error::other("the disk is full"),
    }
}

/// Proves: a commit of the desired state and the operations state over two backends commits
/// each to its own, the desired state first; when the second refuses, the first is put back as
/// it was; over one backend it is one commit.
///
/// Catches: a desired state left in place by a change refused after it, or a change split in
/// two over a backend that takes it whole.
#[test]
fn a_commit_across_two_backends_puts_the_desired_state_back_when_the_rest_fails() {
    let (before, after) = (desired::Held::default(), desired_at("2.0", 'a'));
    let (none, ops) = (OpsState::default(), OpsState::default());
    let audit = lines(1, 1);
    let change = Commit {
        desired: Some((&before, &after)),
        ops: Some((&none, &ops)),
        audit: &audit,
        ..Commit::default()
    };
    let (first, rest) = (Fake::new(), Fake::new());
    let (a, b): (Shared, Shared) = (Arc::new(first.clone()), Arc::new(rest.clone()));
    commit_across(&a, &b, &change).expect("committed");
    assert_eq!(first.desired(), after);
    assert_eq!(rest.audit(), audit);
    assert_eq!(rest.desired(), desired::Held::default(), "not to the rest");

    let (other_first, other_rest) = (Fake::new(), Fake::new());
    other_rest.fail();
    let (a, b): (Shared, Shared) = (Arc::new(other_first.clone()), Arc::new(other_rest));
    assert!(commit_across(&a, &b, &change).is_err());
    assert_eq!(other_first.desired(), before, "put back");
    assert_eq!(other_first.commits(), 2, "written, then put back");

    let (refusing_first, untouched) = (Fake::new(), Fake::new());
    refusing_first.fail();
    let (a, b): (Shared, Shared) = (Arc::new(refusing_first), Arc::new(untouched.clone()));
    assert!(commit_across(&a, &b, &change).is_err());
    assert_eq!(
        untouched.commits(),
        0,
        "nothing of a refused desired state goes on"
    );

    let one = Fake::new();
    let shared: Shared = Arc::new(one.clone());
    commit_across(&shared, &shared, &change).expect("committed");
    assert_eq!((one.commits(), one.desired()), (1, after));
}

/// Proves: over the files, a commit of the desired state and more whose desired file is renamed
/// in but not synced is refused, and the desired file is put back as it was.
///
/// Catches: a desired state left on disk, unsynced, by a change that was refused.
#[test]
fn an_unsynced_desired_file_of_a_wider_commit_is_put_back() {
    let dir = scratch("store-unsynced");
    let path = dir.path().join("desired.json");
    let store = DesiredStore::open(&path, Some(LEGACY)).expect("a store");
    store
        .set(LEGACY, desired_at("1.0", 'a').desired[LEGACY_KEY].clone())
        .expect("set");
    let written = std::fs::read(&path).expect("the file");
    let files = Files::default()
        .with_desired(&path, Some(LEGACY))
        .with_sync_dir(|_| Err(std::io::Error::other("no sync")))
        .with_ops(&dir.path().join("desired.json.ops"), DEFAULT_ROTATION)
        .expect("the ops files");
    let before = files.load_desired().expect("loaded");
    let mut after = before.clone();
    after.desired.insert(
        LEGACY.to_owned(),
        desired_at("2.0", 'b').desired[LEGACY_KEY].clone(),
    );
    let ops = OpsState::default();
    let refused = files.commit(&Commit {
        desired: Some((&before, &after)),
        ops: Some((&ops, &ops)),
        ..Commit::default()
    });
    assert!(
        matches!(
            refused,
            Err(BackendError::Desired(StoreError::NotSynced { .. }))
        ),
        "{refused:?}"
    );
    assert_eq!(std::fs::read(&path).expect("the file"), written);
    assert!(!dir.path().join("desired.json.ops").exists(), "no ops file");
}

/// The service [`desired_at`] names.
const LEGACY_KEY: &str = "kbf";

/// Proves: the files with no part keep nothing and answer as empty: a load of nothing, every
/// commit taken, no audit, no spent id kept.
///
/// Catches: a store built without a file that fails, or that keeps what it was given.
#[test]
fn files_with_no_part_keep_nothing() {
    let files = Files::default();
    assert_eq!(files.load().expect("loaded"), Loaded::default());
    let (held, ops, verdicts) = (desired_at("2.0", 'a'), OpsState::default(), rich_verdicts());
    let audit = lines(1, 2);
    files
        .commit(&Commit {
            ops: Some((&ops, &ops)),
            verdicts: Some((&verdict_file::Held::default(), &verdicts)),
            audit: &audit,
            ..Commit::default()
        })
        .expect("taken");
    files
        .commit(&Commit {
            desired: Some((&desired::Held::default(), &held)),
            ..Commit::default()
        })
        .expect("taken");
    let every = |_: &crate::ops::AuditEntry| true;
    let query = Query {
        since_seq: 0,
        limit: 10,
        newest: false,
        keep: &every,
    };
    assert_eq!(files.audit_page(&query).expect("a page"), []);
    let mut seen = 0;
    files.audit_each(&mut |_| seen += 1).expect("read");
    assert_eq!((seen, files.last_seq()), (0, 0));
    assert_eq!(files.audit_pruned_before().expect("read"), None);
    files.spend("j", 10, 1).expect("taken");
    files
        .spend("j", 10, 1)
        .expect("kept nowhere, so taken again");
    assert_eq!(files.load().expect("loaded"), Loaded::default());
}

/// Proves: each store says a backend's failure as its own error: the error of its own part as it
/// is, a file operation's of another part as that file operation's, anything else as an I/O
/// error that says it; and a start says a load's failure as the refusal of the part that failed.
///
/// Catches: a refusal that loses its cause, or a store failure answered as another refusal.
#[test]
fn each_store_says_a_backend_failure_as_its_own_error() {
    let spent = "store: the token id is spent";
    assert_eq!(BackendError::Spent.into_store().to_string(), spent);
    for why in [
        BackendError::Desired(io_at("d")),
        BackendError::Ops(OpsError::Io(io_at("d"))),
        BackendError::Verdicts(VerdictsError::Io(io_at("d"))),
    ] {
        assert_eq!(why.into_store().to_string(), "d: the disk is full");
    }
    assert!(matches!(
        OpsError::from(BackendError::Desired(io_at("d"))),
        OpsError::Io(StoreError::Io { .. })
    ));

    let fake = Fake::new();
    fake.refuse(|_| Some(BackendError::Spent));
    let store = DesiredStore::over(Arc::new(fake.clone()), desired::Held::default());
    let refused = store
        .set(LEGACY, desired_at("1.0", 'a').desired[LEGACY_KEY].clone())
        .expect_err("refused");
    assert_eq!(refused.to_string(), spent);
    assert_eq!(store.get(LEGACY), None, "nothing changed");

    let verdicts = VerdictFile::over(Arc::new(fake.clone()), verdict_file::Held::default());
    let host = "s1".parse().expect("a host");
    let report = serde_json::from_value(serde_json::json!({
        "target": "2.0", "outcome": "failed", "phase": "boot", "reason": "a",
        "order_id": "update-1", "order_try": 1
    }))
    .expect("a report");
    let heard = [(LEGACY.to_owned(), Some(report))];
    let refused = verdicts.record(&host, &heard, &[]).expect_err("refused");
    assert!(matches!(refused, VerdictsError::Io(_)), "{refused:?}");
    let invalid = |path: &str| VerdictsError::Invalid {
        path: path.into(),
        why: "bad".into(),
    };
    fake.refuse(move |_| Some(BackendError::Verdicts(invalid("v"))));
    let refused = verdicts.record(&host, &heard, &[]).expect_err("refused");
    assert!(
        matches!(refused, VerdictsError::Invalid { .. }),
        "{refused:?}"
    );
    fake.heal();
    verdicts.record(&host, &heard, &[]).expect("recorded");
    assert_eq!(verdicts.standing(LEGACY, &host), heard[0].1);
    let reopened = VerdictFile::over(Arc::new(fake.clone()), fake.verdicts());
    assert_eq!(
        reopened.standing(LEGACY, &host),
        heard[0].1,
        "committed to the fake"
    );

    let jti = JtiError::Malformed {
        path: "j".into(),
        line: 2,
    };
    let starts = [
        (BackendError::Desired(io_at("d")), "d: the disk is full"),
        (
            BackendError::Ops(OpsError::Io(io_at("o"))),
            "--ops-file: o: the disk is full",
        ),
        (
            BackendError::Verdicts(VerdictsError::Io(io_at("v"))),
            "v: the disk is full",
        ),
        (BackendError::Spent, "the token id is spent"),
    ];
    for (why, said) in starts {
        assert_eq!(StartError::from(why).to_string(), said);
    }
    assert!(matches!(
        StartError::from(BackendError::Jti(jti)),
        StartError::Jti(JtiError::Malformed { line: 2, .. })
    ));
}

/// Proves: CI's spent ids over a backend give back what was loaded and commit each spend by the
/// clock, a replay refused as an I/O error that says it; over the files, a spend the file cannot
/// take is the file's error, naming it.
///
/// Catches: a spend that does not reach the backend, or a replay or a failed write taken.
#[test]
fn spent_ids_over_a_backend_commit_each_spend() {
    let fake = Fake::new();
    let spent = Spent::new(
        Arc::new(fake.clone()),
        vec![("old".to_owned(), 90)],
        Arc::new(|| 50),
    );
    assert_eq!(spent.load(), [("old".to_owned(), 90)]);
    spent.record("j1", 100).expect("spent");
    let replay = spent.record("j1", 100).expect_err("a replay");
    assert_eq!(replay.to_string(), "the token id is spent");
    assert_eq!(fake.load().expect("loaded").spent, [("j1".to_owned(), 100)]);

    let dir = scratch("store-spend-fails");
    let path = jti_path(&dir.path().join("desired.json"));
    let files = Files::default()
        .with_jti(&path, Arc::new(|| 50))
        .expect("the spent ids");
    assert!(matches!(
        files.spend("not one word", 100, 50),
        Err(BackendError::Jti(JtiError::Io { .. }))
    ));
    std::fs::remove_file(&path).expect("removed");
    std::fs::create_dir(&path).expect("a directory in its place");
    let why = files.spend("j2", 100, 50).expect_err("not written");
    assert!(
        matches!(&why, BackendError::Jti(JtiError::Io { path: p, .. }) if *p == path),
        "{why:?}"
    );
}

/// Proves: a raise whose audit line alone cannot be kept stands and is answered as made, as it
/// always was, and stays pending until the line is kept with it.
///
/// Catches: a raise refused for its audit, or one taken as written without its line.
#[test]
fn a_raise_whose_audit_cannot_be_kept_stands_pending() {
    let fake = Fake::new();
    fake.refuse(|_| Some(super::fake::audit_failure()));
    let store = OpsStore::on_fake(&fake, OpsState::default());
    let entry = store
        .change(
            crate::ops::Write::AfterMemory,
            crate::ops::AuditEntry::new("coordinator", "mark_bad"),
            |state| state.services.entry("web".to_owned()).or_default().bad = vec![version("2.0")],
        )
        .expect("a raise stands");
    assert_eq!(entry.seq, 1);
    assert!(store.halt_unwritten());
    assert_eq!(fake.audit(), []);
    fake.heal();
    store.retry_unwritten(std::time::Duration::from_secs(1));
    assert!(!store.halt_unwritten());
    assert_eq!(fake.audit().len(), 1);
    assert_eq!(fake.ops().services["web"].bad, [version("2.0")]);
}

/// Proves: the abort of a store whose commits fail is made all the same and stands pending.
///
/// Catches: an abort lost, or refused, because storage fails.
#[test]
fn an_abort_stands_whatever_its_commit_does() {
    let fake = Fake::new();
    fake.refuse(|_| Some(store_failure()));
    let store = OpsStore::on_fake(&fake, OpsState::default());
    let entry = store.escape(crate::ops::AuditEntry::new("operator", "abort"));
    assert_eq!((entry.seq, entry.action.as_str()), (1, "abort"));
    assert!(store.halt_unwritten());
    assert_eq!(store.state().audit, [entry]);
}
