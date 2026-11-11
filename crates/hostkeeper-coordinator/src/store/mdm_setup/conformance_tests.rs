//! The contract of the [`SetupStore`], as one suite run against both stores: the files and
//! Postgres (`docs/design/coordinator-mdm.md`, 4.9 and 10). Each clause is a function that answers
//! how the store fared, so that the suite's own test can run it against a store wrong in one way
//! each and see the clauses that name the fault fail, and only those.
//!
//! The clauses:
//! - `versions_are_numbered_by_the_store`: a row is the next version of its kind or it is refused
//!   and changes nothing, kinds number apart, the version replaced is marked superseded at the
//!   time of its replacement, and the rows come back by kind and version, bytes exactly as given.
//! - `prune_deletes_only_what_was_superseded_by_the_time`: the version in use is never deleted.
//! - `settings_are_set_and_replaced`.
//! - `a_failed_change_is_not_made`.
//! - `what_was_written_is_there_after_a_restart`.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hostkeeper_testkit::Scratch;

use super::files::FileSetup;
use super::{Sealed, SecretRow, SetupError, SetupStore};
use crate::store::postgres::Postgres;
use crate::store::postgres::mdm_setup::PgSetup;
use crate::store::postgres::tests::TestDatabase;
use crate::testutil::scratch;

/// A store under the suite: the live one, one started again on what it holds, and a way to make
/// its next secret writes fail.
pub(crate) trait Subject {
    /// The store that changes.
    fn live(&self) -> Box<dyn SetupStore>;
    /// A store started again on what the live one made durable.
    fn restarted(&self) -> Box<dyn SetupStore>;
    /// From now on every write of a secret fails (`true`), or none does.
    fn failing(&self, on: bool);
}

/// How one clause fared: `Err` names what it found.
type Outcome = Result<(), String>;

type Clause = fn(&dyn Subject) -> Outcome;

fn check(holds: bool, what: impl FnOnce() -> String) -> Outcome {
    if holds { Ok(()) } else { Err(what()) }
}

fn store<T>(r: Result<T, SetupError>) -> Result<T, String> {
    r.map_err(|e| e.to_string())
}

/// The row of `kind` at `version` made at `at`, sealed to bytes that are not text.
fn row(kind: &str, version: u32, at: i64) -> SecretRow {
    SecretRow {
        kind: kind.to_owned(),
        version,
        sealed: Sealed(
            [0_u8, 255, 1, 254, version as u8]
                .into_iter()
                .cycle()
                .take(40)
                .collect(),
        ),
        fingerprint: format!("{version:064x}"),
        not_after_unix: Some(at + 1000),
        created_unix: at,
        superseded_unix: None,
    }
}

fn versions(rows: &[SecretRow]) -> Vec<(String, u32, Option<i64>)> {
    rows.iter()
        .map(|r| (r.kind.clone(), r.version, r.superseded_unix))
        .collect()
}

fn versions_are_numbered_by_the_store(subject: &dyn Subject) -> Outcome {
    let s = subject.live();
    store(s.put_secret(&row("tls_key", 1, 10)))?;
    let refused = s.put_secret(&row("tls_key", 1, 11));
    check(
        refused
            == Err(SetupError::Version {
                kind: "tls_key".to_owned(),
                expected: 2,
            }),
        || format!("a second version 1 gave {refused:?}"),
    )?;
    let skipped = s.put_secret(&row("tls_key", 3, 11));
    check(
        matches!(skipped, Err(SetupError::Version { expected: 2, .. })),
        || format!("a version 3 after 1 gave {skipped:?}"),
    )?;
    let after_refusals = store(s.secrets())?;
    check(
        versions(&after_refusals) == [("tls_key".to_owned(), 1, None)],
        || format!("a refusal changed the rows: {after_refusals:?}"),
    )?;

    store(s.put_secret(&row("push_key", 1, 12)))?;
    store(s.put_secret(&row("tls_key", 2, 20)))?;
    let rows = store(s.secrets())?;
    check(
        versions(&rows)
            == [
                ("push_key".to_owned(), 1, None),
                ("tls_key".to_owned(), 1, Some(20)),
                ("tls_key".to_owned(), 2, None),
            ],
        || format!("the rows are {:?}", versions(&rows)),
    )?;
    let mut sent = row("tls_key", 2, 20);
    sent.superseded_unix = None;
    check(rows[2] == sent, || {
        format!("the bytes or times changed: {:?} for {sent:?}", rows[2])
    })?;
    check(rows[1].sealed == row("tls_key", 1, 10).sealed, || {
        "the sealed bytes of a replaced version changed".to_owned()
    })?;
    // A row that says it is superseded is stored as the one in use.
    let mut lies = row("abm_key", 1, 30);
    lies.superseded_unix = Some(5);
    store(s.put_secret(&lies))?;
    let held = store(s.secrets())?;
    check(held[0].superseded_unix.is_none(), || {
        format!("a new row came back superseded: {:?}", held[0])
    })
}

fn prune_deletes_only_what_was_superseded_by_the_time(subject: &dyn Subject) -> Outcome {
    let s = subject.live();
    store(s.put_secret(&row("tls_key", 1, 10)))?;
    store(s.put_secret(&row("tls_key", 2, 20)))?;
    store(s.put_secret(&row("tls_key", 3, 30)))?;
    store(s.put_secret(&row("push_key", 1, 5)))?;
    let none = store(s.prune_superseded(19))?;
    check(none == 0, || {
        format!("a prune before every replacement deleted {none}")
    })?;
    let one = store(s.prune_superseded(20))?;
    check(one == 1, || {
        format!("a prune at a replacement's time deleted {one}")
    })?;
    let left = versions(&store(s.secrets())?);
    check(
        left == [
            ("push_key".to_owned(), 1, None),
            ("tls_key".to_owned(), 2, Some(30)),
            ("tls_key".to_owned(), 3, None),
        ],
        || format!("after the first prune: {left:?}"),
    )?;
    let rest = store(s.prune_superseded(i64::MAX))?;
    check(rest == 1, || {
        format!("a prune of everything deleted {rest}")
    })?;
    let kept = versions(&store(s.secrets())?);
    check(
        kept == [
            ("push_key".to_owned(), 1, None),
            ("tls_key".to_owned(), 3, None),
        ],
        || format!("the versions in use are not all that is left: {kept:?}"),
    )?;
    // A version numbered after the deleted ones continues from the newest.
    store(s.put_secret(&row("tls_key", 4, 40)))
}

fn settings_are_set_and_replaced(subject: &dyn Subject) -> Outcome {
    let s = subject.live();
    check(store(s.settings())?.is_empty(), || {
        "a new store holds a setting".to_owned()
    })?;
    store(s.set_setting("tls_renew_days", "30"))?;
    store(s.set_setting("push_alert_days", "60,14"))?;
    store(s.set_setting("tls_renew_days", "10"))?;
    // A name this release does not know is kept as it was written.
    store(s.set_setting("from_the_future", "x y"))?;
    let expected: BTreeMap<String, String> = [
        ("from_the_future", "x y"),
        ("push_alert_days", "60,14"),
        ("tls_renew_days", "10"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    let found = store(s.settings())?;
    check(found == expected, || format!("the settings are {found:?}"))
}

fn a_failed_change_is_not_made(subject: &dyn Subject) -> Outcome {
    let s = subject.live();
    store(s.put_secret(&row("tls_key", 1, 10)))?;
    subject.failing(true);
    let refused = s.put_secret(&row("tls_key", 2, 20));
    check(matches!(refused, Err(SetupError::Store(_))), || {
        format!("a write that cannot be made answered {refused:?}")
    })?;
    subject.failing(false);
    let held = versions(&store(s.secrets())?);
    check(held == [("tls_key".to_owned(), 1, None)], || {
        format!("a failed write left {held:?}")
    })?;
    store(s.put_secret(&row("tls_key", 2, 20)))
}

fn what_was_written_is_there_after_a_restart(subject: &dyn Subject) -> Outcome {
    let s = subject.live();
    store(s.put_secret(&row("tls_key", 1, 10)))?;
    store(s.put_secret(&row("tls_key", 2, 20)))?;
    store(s.set_setting("tls_renew_days", "9"))?;
    store(s.prune_superseded(0))?;
    let again = subject.restarted();
    check(store(again.secrets())? == store(s.secrets())?, || {
        "the secrets differ after a restart".to_owned()
    })?;
    check(store(again.settings())? == store(s.settings())?, || {
        "the settings differ after a restart".to_owned()
    })?;
    check(store(again.secrets())?.len() == 2, || {
        "the secrets are not both there".to_owned()
    })?;
    // The restarted store goes on numbering where the first stopped, and prunes what it read.
    store(again.put_secret(&row("tls_key", 3, 30)))?;
    check(store(again.prune_superseded(20))? == 1, || {
        "the restarted store did not prune the version it read".to_owned()
    })
}

/// Every clause, by name.
const CLAUSES: [(&str, Clause); 5] = [
    (
        "versions_are_numbered_by_the_store",
        versions_are_numbered_by_the_store,
    ),
    (
        "prune_deletes_only_what_was_superseded_by_the_time",
        prune_deletes_only_what_was_superseded_by_the_time,
    ),
    (
        "settings_are_set_and_replaced",
        settings_are_set_and_replaced,
    ),
    ("a_failed_change_is_not_made", a_failed_change_is_not_made),
    (
        "what_was_written_is_there_after_a_restart",
        what_was_written_is_there_after_a_restart,
    ),
];

/// Runs every clause on a subject of its own, made by `make` with the clause's name: the names of
/// the clauses that failed, each with what it found.
fn run(make: &dyn Fn(&str) -> Box<dyn Subject>) -> Vec<(&'static str, String)> {
    CLAUSES
        .iter()
        .filter_map(|(name, clause)| clause(&*make(name)).err().map(|why| (*name, why)))
        .collect()
}

// ---- the subjects --------------------------------------------------------------------------

/// The same files store handed out more than once: the live store, and what a call borrows.
struct Handle(Arc<FileSetup>);

impl SetupStore for Handle {
    fn put_secret(&self, row: &SecretRow) -> Result<(), SetupError> {
        self.0.put_secret(row)
    }
    fn secrets(&self) -> Result<Vec<SecretRow>, SetupError> {
        self.0.secrets()
    }
    fn prune_superseded(&self, at: i64) -> Result<u64, SetupError> {
        self.0.prune_superseded(at)
    }
    fn settings(&self) -> Result<BTreeMap<String, String>, SetupError> {
        self.0.settings()
    }
    fn set_setting(&self, name: &str, value: &str) -> Result<(), SetupError> {
        self.0.set_setting(name, value)
    }
}

/// The files: a directory of the suite's own.
struct OnFiles {
    dir: Scratch,
    live: Arc<FileSetup>,
}

impl OnFiles {
    fn new(name: &str) -> Self {
        let dir = scratch(&format!("mdm-setup-conformance-{name}"));
        let live = Arc::new(FileSetup::open(&dir.path().join("setup")).expect("the files open"));
        Self { dir, live }
    }

    fn blocker(&self) -> PathBuf {
        let mut name = std::ffi::OsString::from("secrets.json");
        name.push(".new");
        self.dir.path().join("setup").join(name)
    }
}

impl Subject for OnFiles {
    fn live(&self) -> Box<dyn SetupStore> {
        Box::new(Handle(Arc::clone(&self.live)))
    }

    fn restarted(&self) -> Box<dyn SetupStore> {
        Box::new(FileSetup::open(&self.dir.path().join("setup")).expect("the files open again"))
    }

    // A directory where the new file is written makes the write fail.
    fn failing(&self, on: bool) {
        if on {
            fs::create_dir(self.blocker()).expect("the blocker is made");
        } else {
            fs::remove_dir(self.blocker()).expect("the blocker is removed");
        }
    }
}

/// Postgres: a database of the suite's own.
struct OnPostgres {
    database: TestDatabase,
    store: Postgres,
}

impl OnPostgres {
    fn new() -> Self {
        let database = TestDatabase::new();
        let (store, _) = database
            .open("mdm-setup-conformance")
            .expect("the store opens");
        Self { database, store }
    }
}

impl Subject for OnPostgres {
    fn live(&self) -> Box<dyn SetupStore> {
        Box::new(PgSetup::over(&self.store))
    }

    fn restarted(&self) -> Box<dyn SetupStore> {
        // Every row is committed when a call returns, so what a started store reads is what this
        // one reads.
        Box::new(PgSetup::over(&self.store))
    }

    fn failing(&self, on: bool) {
        let sql = if on {
            "CREATE FUNCTION test_refuse() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN RAISE EXCEPTION 'the test refuses this change'; END $$; \
             CREATE TRIGGER test_refuse BEFORE INSERT ON mdm_secrets FOR EACH ROW \
             EXECUTE FUNCTION test_refuse()"
        } else {
            "DROP TRIGGER test_refuse ON mdm_secrets; DROP FUNCTION test_refuse()"
        };
        self.database.run(sql).expect("the refusal is set");
    }
}

/// How a store is wrong: the clauses that name it must fail, and no other.
#[derive(Clone, Copy, Debug)]
enum Flaw {
    /// A row is stored whatever its version, numbered as the next one.
    RenumbersRows,
    /// A prune deletes nothing.
    NeverPrunes,
    /// Nothing written is there after a restart.
    ForgetsOnRestart,
}

/// A files store with one flaw.
struct Flawed {
    inner: Handle,
    flaw: Flaw,
}

impl SetupStore for Flawed {
    fn put_secret(&self, row: &SecretRow) -> Result<(), SetupError> {
        if matches!(self.flaw, Flaw::RenumbersRows) {
            let next = self
                .inner
                .secrets()?
                .iter()
                .filter(|held| held.kind == row.kind)
                .map(|held| held.version)
                .max()
                .map_or(1, |newest| newest + 1);
            return self.inner.put_secret(&SecretRow {
                version: next,
                ..row.clone()
            });
        }
        self.inner.put_secret(row)
    }

    fn prune_superseded(&self, at: i64) -> Result<u64, SetupError> {
        if matches!(self.flaw, Flaw::NeverPrunes) {
            return Ok(0);
        }
        self.inner.prune_superseded(at)
    }

    fn secrets(&self) -> Result<Vec<SecretRow>, SetupError> {
        self.inner.secrets()
    }
    fn settings(&self) -> Result<BTreeMap<String, String>, SetupError> {
        self.inner.settings()
    }
    fn set_setting(&self, name: &str, value: &str) -> Result<(), SetupError> {
        self.inner.set_setting(name, value)
    }
}

struct OnFlawed {
    flaw: Flaw,
    files: OnFiles,
}

impl Subject for OnFlawed {
    fn live(&self) -> Box<dyn SetupStore> {
        Box::new(Flawed {
            inner: Handle(Arc::clone(&self.files.live)),
            flaw: self.flaw,
        })
    }

    fn restarted(&self) -> Box<dyn SetupStore> {
        let path = if matches!(self.flaw, Flaw::ForgetsOnRestart) {
            self.files.dir.path().join("another")
        } else {
            self.files.dir.path().join("setup")
        };
        Box::new(Flawed {
            inner: Handle(Arc::new(
                FileSetup::open(&path).expect("the files open again"),
            )),
            flaw: self.flaw,
        })
    }

    fn failing(&self, on: bool) {
        self.files.failing(on);
    }
}

/// Proves: the files and Postgres each keep every clause of the setup store's contract: versions
/// numbered by the store with a refusal that changes nothing, the replaced version marked
/// superseded, a prune that never touches the version in use, settings set and replaced, a failed
/// change that is not made, and everything written there after a restart.
///
/// Catches: a store that takes a second version 1 of a kind, deletes the version in use, forgets
/// a setting across a restart or keeps a row of a failed write.
#[test]
fn the_conformance_suite_passes_on_the_files_and_postgres() {
    assert_eq!(run(&|name| Box::new(OnFiles::new(name))), []);
    assert_eq!(run(&|_| Box::new(OnPostgres::new())), []);
}

/// Proves: the suite catches what it claims. Against a store wrong in one way each, the clauses
/// that name the fault fail, and every other clause passes.
///
/// Catches: a clause that passes whatever the store does, or one that fails for a fault that is
/// another clause's.
#[test]
fn the_suite_fails_against_a_store_that_breaks_each_clause() {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let cases: [(Flaw, &[&str]); 3] = [
        (Flaw::RenumbersRows, &["versions_are_numbered_by_the_store"]),
        (
            Flaw::NeverPrunes,
            &[
                "prune_deletes_only_what_was_superseded_by_the_time",
                "what_was_written_is_there_after_a_restart",
            ],
        ),
        (
            Flaw::ForgetsOnRestart,
            &["what_was_written_is_there_after_a_restart"],
        ),
    ];
    for (flaw, clauses) in cases {
        let run_id = NEXT.fetch_add(1, Ordering::Relaxed);
        let failed: Vec<&str> = run(&|name| {
            Box::new(OnFlawed {
                flaw,
                files: OnFiles::new(&format!("flawed-{run_id}-{name}")),
            })
        })
        .into_iter()
        .map(|(name, _)| name)
        .collect();
        assert_eq!(failed, clauses, "{flaw:?}");
    }
}
