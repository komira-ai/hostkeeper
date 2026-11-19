//! Edits as row transactions (`docs/design/coordinator-multi-instance.md`, section 6): the locks an
//! edit takes and their one order, an edit made by a coordinator whose copy is older than the rows,
//! the versions an edit moves on, and an edit dropped before its commit.
//!
//! Two coordinators are two stores opened on one database. A lock an edit holds is seen by a
//! session of the test's asking for the same row with `NOWAIT`; an edit is held open by its own
//! closure, which waits for the test on a channel. Nothing sleeps and no time is measured.

use std::convert::Infallible;
use std::sync::Arc;

use super::{AT, TestDatabase};
use crate::accounts::AccountStore;
use crate::desired::DesiredStore;
use crate::layout::LayoutStore;
use crate::ops::{AuditEntry, OpsState, OpsStore, Write};
use crate::store::conformance_tests::rich_accounts;
use crate::store::postgres::Postgres;
use crate::store::{Backend, Lock, Part, Scope, Shared};
use crate::verdict_file::VerdictFile;

/// One coordinator on a database: its store and its stores of the parts.
pub(super) struct Instance {
    pub(super) store: Postgres,
    pub(super) desired: DesiredStore,
    pub(super) ops: OpsStore,
    pub(super) accounts: AccountStore,
    pub(super) layout: LayoutStore,
    pub(super) verdicts: VerdictFile,
}

/// A coordinator started on `db` as `name`.
pub(super) fn instance(db: &TestDatabase, name: &str) -> Instance {
    let (store, loaded) = db.open(name).expect("the store opens");
    let backend: Shared = Arc::new(store.clone());
    Instance {
        desired: DesiredStore::over(Arc::clone(&backend), loaded.desired),
        ops: OpsStore::over(Arc::clone(&backend), loaded.ops)
            .with_facts(loaded.facts)
            .with_unix(Arc::new(|| AT)),
        accounts: AccountStore::over(Arc::clone(&backend), loaded.accounts),
        layout: LayoutStore::over(Arc::clone(&backend), loaded.layout),
        verdicts: VerdictFile::over(backend, loaded.verdicts),
        store,
    }
}

/// Labels `host` with `key` = `value` on `ops`, audited as an operator's `set_labels`.
pub(super) fn label(ops: &OpsStore, host: &str, key: &str, value: &str) -> AuditEntry {
    let mut entry = AuditEntry::new("alice", "set_labels");
    entry.node_id = Some(host.to_owned());
    ops.change(Write::BeforeMemory, entry, |state| {
        state
            .hosts
            .entry(host.to_owned())
            .or_default()
            .labels
            .insert(key.to_owned(), value.to_owned());
    })
    .expect("the labels are set")
}

/// A change refused, for whatever reason: what a test that only asks whether it was made takes a
/// change's own error as.
#[derive(Debug)]
pub(super) struct Refused;

impl From<crate::ops::OpsError> for Refused {
    fn from(_: crate::ops::OpsError) -> Refused {
        Refused
    }
}

/// The labels `state` holds, as (host, key, value).
pub(super) fn labels(state: &OpsState) -> Vec<(String, String, String)> {
    state
        .hosts
        .iter()
        .flat_map(|(host, ops)| {
            ops.labels
                .iter()
                .map(|(key, value)| (host.clone(), key.clone(), value.clone()))
        })
        .collect()
}

/// The labels the database holds, as (host, key, value).
pub(super) fn rows_of_labels(db: &TestDatabase) -> Vec<(String, String, String)> {
    db.rows("SELECT node_id, key, value FROM host_labels ORDER BY node_id, key")
        .into_iter()
        .map(|row| {
            let text = |i: usize| row[i].clone().unwrap_or_default();
            (text(0), text(1), text(2))
        })
        .collect()
}

/// The version of `part` the database holds.
pub(super) fn version(db: &TestDatabase, part: Part) -> i64 {
    db.rows(&format!(
        "SELECT version FROM state_versions WHERE part = '{}'",
        part.name()
    ))[0][0]
        .as_deref()
        .and_then(|n| n.parse().ok())
        .expect("a version")
}

/// The version of every part the database holds, in [`Part`]'s order.
pub(super) fn versions(db: &TestDatabase) -> Vec<i64> {
    Part::ALL
        .into_iter()
        .map(|part| version(db, part))
        .collect()
}

/// The audit counter's next number.
pub(super) fn counter(db: &TestDatabase) -> i64 {
    db.rows("SELECT next FROM audit_counter")[0][0]
        .as_deref()
        .and_then(|n| n.parse().ok())
        .expect("the counter")
}

/// The numbers of the audit's lines, in order.
pub(super) fn audit_numbers(db: &TestDatabase) -> Vec<u64> {
    db.rows("SELECT seq FROM audit ORDER BY seq")
        .into_iter()
        .map(|row| {
            row[0]
                .as_deref()
                .and_then(|n| n.parse().ok())
                .expect("a number")
        })
        .collect()
}

/// The statement that asks for the row of `lock` without waiting.
fn probe(lock: Lock) -> String {
    match lock {
        Lock::Gate => "SELECT 1 FROM rollout_gate WHERE id = 1 FOR UPDATE NOWAIT".to_owned(),
        Lock::Part(part) => format!(
            "SELECT 1 FROM state_versions WHERE part = '{}' FOR UPDATE NOWAIT",
            part.name()
        ),
        Lock::AuditCounter => {
            "SELECT 1 FROM audit_counter WHERE id = 1 FOR UPDATE NOWAIT".to_owned()
        }
    }
}

/// The rows of the gate, the parts and the counter that another session holds now.
pub(super) fn held(db: &TestDatabase) -> Vec<Lock> {
    let every = [Lock::Gate]
        .into_iter()
        .chain(Part::ALL.into_iter().map(Lock::Part))
        .chain([Lock::AuditCounter]);
    every
        .filter(|lock| {
            db.run(&format!("BEGIN; {}; ROLLBACK", probe(*lock)))
                .is_err()
        })
        .collect()
}

/// The first two locks of `plan` that are not in the one global order, if any.
fn out_of_order(plan: &[Lock]) -> Option<(Lock, Lock)> {
    plan.windows(2)
        .find(|pair| pair[0] >= pair[1])
        .map(|pair| (pair[0], pair[1]))
}

/// Proves: every scope an edit is begun with takes its locks in the one global order (the gate,
/// then the parts' rows in the order desired, ops, accounts, layout, verdicts, then the audit
/// counter), each at most once; the scopes of a part a decision reads take the gate first and the
/// accounts' does not; an edit that writes audit lines takes the counter last and one that writes
/// none takes no counter. Every transaction takes its locks in one total order, so none waits for
/// one held by a transaction that waits for one it holds: no cycle. The check finds a plan that
/// takes a part's row before the gate, or the counter before a part's row.
///
/// Catches: an edit that takes the gate after its part's row, or the counter before a part's row,
/// either of which two coordinators can deadlock on (the model's `edit_takes_the_gate_after_its_part_row`
/// and `audit_counter_taken_before_a_part_row`), and a heartbeat's or an operator's edit of a
/// decision's input that does not take the gate.
#[test]
fn lock_order_has_no_cycle() {
    for scope in Scope::ALL {
        let plan = scope.locks();
        assert_eq!(out_of_order(&plan), None, "{scope:?}");
        assert_eq!(
            plan.first() == Some(&Lock::Gate),
            scope.gated(),
            "{scope:?}"
        );
        assert_eq!(
            plan.last() == Some(&Lock::AuditCounter),
            scope.audited,
            "{scope:?}"
        );
    }
    assert_eq!(
        Scope::DESIRED_AND_OPS.locks(),
        [
            Lock::Gate,
            Lock::Part(Part::Desired),
            Lock::Part(Part::Ops),
            Lock::AuditCounter
        ]
    );
    assert_eq!(Scope::ACCOUNTS.locks(), [Lock::Part(Part::Accounts)]);
    assert_eq!(Scope::AUDIT.locks(), [Lock::AuditCounter]);
    assert!(Scope::VERDICTS.gated() && Scope::LAYOUT.gated() && !Scope::ACCOUNTS.gated());
    assert_eq!(
        out_of_order(&[Lock::Part(Part::Ops), Lock::Gate]),
        Some((Lock::Part(Part::Ops), Lock::Gate))
    );
    assert_eq!(
        out_of_order(&[Lock::AuditCounter, Lock::Part(Part::Desired)]),
        Some((Lock::AuditCounter, Lock::Part(Part::Desired)))
    );
}

/// Proves: against a server, an edit of the operations state holds the gate, the ops part's row
/// and the counter while its change is made, and nothing else; a change of the desired state staged
/// with the operations state holds no lock of the database while it is staged, and the gate, both
/// parts' rows and the counter while the ops store makes its change; a change of the accounts
/// holds the accounts' row alone; a change of the layout the gate and its row. Each frees all of
/// them when it ends, committed or dropped.
///
/// Catches: an edit whose transaction takes other locks than its scope's plan says, so the plan the
/// order is checked over is not what runs, and one that keeps a lock past its end.
#[test]
fn an_edit_holds_the_locks_of_its_scope_until_it_ends() {
    let db = TestDatabase::new();
    let one = instance(&db, "one");
    let mut seen = Vec::new();
    one.ops
        .change(
            Write::BeforeMemory,
            AuditEntry::new("alice", "exclude"),
            |_| seen = held(&db),
        )
        .expect("committed");
    assert_eq!(seen, Scope::OPS.locks());
    assert_eq!(held(&db), []);

    let staged = one
        .desired
        .stage_repositories(|registry| Ok::<_, Infallible>(registry.clone()))
        .expect("staged");
    assert_eq!(held(&db), [], "a change staged on memory holds nothing");
    one.ops
        .decide_changes_with(staged, |_| {
            seen = held(&db);
            vec![AuditEntry::new("alice", "repository_put")]
        })
        .expect("committed");
    assert_eq!(seen, Scope::DESIRED_AND_OPS.locks());
    assert_eq!(held(&db), []);

    let accounts = one.accounts.change(|_| {
        assert_eq!(held(&db), Scope::ACCOUNTS.locks());
        Ok(rich_accounts())
    });
    accounts.expect("the accounts are changed");
    let layout = one.layout.change(|held_now| {
        assert_eq!(held(&db), Scope::LAYOUT.locks());
        Ok((held_now.clone(), ()))
    });
    layout.expect("the layout is read");
    let dropped = one.layout.change(|_| {
        Err::<(crate::layout::Layout, ()), _>(crate::layout::LayoutError::Io(
            crate::desired::StoreError::Io {
                path: "layout".into(),
                source: std::io::Error::other("the change refuses"),
            },
        ))
    });
    assert!(dropped.is_err());
    assert_eq!(held(&db), []);
}

/// Proves: while a change of the desired state is staged to commit with the operations state (the
/// desired store's writers' lock held), another thread's change of the operations state is made
/// and committed: the staged change holds no lock of the database, so the ops store's change does
/// not wait for it; the staged change then commits after it, with both in the rows.
///
/// Catches: a staged change that takes the gate before it has the ops store's lock, which a change
/// of the operations state that holds that lock and waits for the gate would wait on until its wait
/// ran out, and the staged change behind it.
#[test]
fn a_change_staged_with_the_ops_state_holds_no_database_lock_until_the_ops_store_has_it() {
    let db = TestDatabase::new();
    let one = instance(&db, "one");
    let staged = one
        .desired
        .stage_repositories(|registry| Ok::<_, Infallible>(registry.clone()))
        .expect("staged");
    std::thread::scope(|scope| {
        let ops = &one.ops;
        scope
            .spawn(move || label(ops, "host-1", "rack", "r1"))
            .join()
            .expect("the other change ends");
    });
    one.ops
        .decide_changes_with(staged, |_| vec![AuditEntry::new("alice", "repository_put")])
        .expect("committed after it");
    assert_eq!(
        db.rows("SELECT action FROM audit ORDER BY seq"),
        [
            [Some("set_labels".to_owned())],
            [Some("repository_put".to_owned())]
        ]
    );
}

/// Proves: a change staged to commit with the operations state on a copy of the desired state
/// older than the rows (another coordinator set a desired version since) is refused when the ops
/// store's edit finds the rows newer, nothing written, and memory takes the rows: the same change
/// made again is staged on them and committed.
///
/// Catches: a staged change committed over a desired state it was not made on, whose diff would
/// write the copy's rows back over the other coordinator's change.
#[test]
fn a_change_staged_on_a_stale_desired_copy_is_refused_and_memory_takes_the_rows() {
    let db = TestDatabase::new();
    let (a, b) = (instance(&db, "a"), instance(&db, "b"));
    b.desired
        .set("web", crate::testutil::desired("2.0", 1, 1))
        .expect("b sets web");
    let desired_at = version(&db, Part::Desired);
    let put = |instance: &Instance| {
        let staged = instance
            .desired
            .stage_repositories(|registry| Ok::<_, Infallible>(registry.clone()))
            .expect("staged");
        instance
            .ops
            .decide_changes_with(staged, |_| vec![AuditEntry::new("alice", "repository_put")])
    };
    let refused = put(&a).expect_err("a's copy is older than the rows");
    assert!(refused.to_string().contains("make it again"), "{refused}");
    assert!(audit_numbers(&db).is_empty());
    assert_eq!(
        a.desired.get("web").map(|d| d.version.as_str().to_owned()),
        Some("2.0".to_owned())
    );
    put(&a).expect("made again on the rows");
    assert_eq!(audit_numbers(&db), [1]);
    assert_eq!(version(&db, Part::Desired), desired_at);
}

/// Proves: a coordinator whose copy of the operations state is older than the rows (another
/// coordinator labelled a host since it read them) makes its next change on the rows: its edit
/// sees the other's label, the rows and its memory hold both labels after it, and each change moved
/// the ops part's version on by one. The other coordinator reads the part again once its version
/// is behind, and holds both too.
///
/// Catches: an edit made on a stale copy, whose diff would be computed against what the other
/// coordinator already changed and whose checks would read a state that no longer is, and a copy
/// that is never read again.
#[test]
fn an_edit_of_a_stale_copy_is_made_on_the_rows_the_other_instance_committed() {
    let db = TestDatabase::new();
    let (a, b) = (instance(&db, "a"), instance(&db, "b"));
    let start = version(&db, Part::Ops);
    label(&a.ops, "host-1", "rack", "r1");
    assert_eq!(version(&db, Part::Ops), start + 1);
    assert_eq!(labels(&b.ops.state()), [], "b's copy is the one it loaded");

    let mut seen = Vec::new();
    let mut entry = AuditEntry::new("bob", "set_labels");
    entry.node_id = Some("host-2".to_owned());
    b.ops
        .change(Write::BeforeMemory, entry, |state| {
            seen = labels(state);
            state
                .hosts
                .entry("host-2".to_owned())
                .or_default()
                .labels
                .insert("rack".to_owned(), "r2".to_owned());
        })
        .expect("b labels its host");
    let both = vec![
        ("host-1".to_owned(), "rack".to_owned(), "r1".to_owned()),
        ("host-2".to_owned(), "rack".to_owned(), "r2".to_owned()),
    ];
    assert_eq!(seen, both[..1], "b's edit was made on the rows a committed");
    assert_eq!(rows_of_labels(&db), both);
    assert_eq!(labels(&b.ops.state()), both);
    assert_eq!(version(&db, Part::Ops), start + 2);

    assert_eq!(a.store.stale(), [Part::Ops]);
    a.ops.refresh();
    assert_eq!(labels(&a.ops.state()), both);
    assert_eq!(a.store.stale(), []);
    assert_eq!(b.store.stale(), [], "b's copy carries the version it wrote");
}

/// Proves: an edit moves on the version of each part whose rows it wrote and of no other: a change
/// of the accounts the accounts' alone; an audit-only line none; a change staged with the desired
/// state that changes only the operations state (a repository put that changes no registry, with
/// its audit line) the ops part's alone; a change that changes nothing in the rows of a part
/// (a change of the accounts to what they are) none.
///
/// Catches: a version moved on for a part that was not written, which makes every other
/// coordinator read it again for nothing, and one not moved on for a part that was, which leaves
/// their copies stale for good.
#[test]
fn an_edit_moves_on_the_version_of_each_part_it_wrote_and_of_no_other() {
    let db = TestDatabase::new();
    let one = instance(&db, "one");
    let before = versions(&db);
    let moved = |was: &[i64], now: &[i64]| -> Vec<Part> {
        Part::ALL
            .into_iter()
            .zip(was.iter().zip(now))
            .filter(|(_, (was, now))| was != now)
            .map(|(part, _)| part)
            .collect()
    };

    one.accounts
        .change(|_| Ok(rich_accounts()))
        .expect("accounts set");
    let after_accounts = versions(&db);
    assert_eq!(moved(&before, &after_accounts), [Part::Accounts]);

    one.accounts
        .change(|held| Ok(held.clone()))
        .expect("nothing changes");
    assert_eq!(versions(&db), after_accounts);

    one.ops
        .append_audit_only(
            crate::audit_actions::AuditOnly::ConsoleOpen,
            "alice",
            None,
            None,
        )
        .expect("appended");
    assert_eq!(versions(&db), after_accounts);

    let staged = one
        .desired
        .stage_repositories(|registry| Ok::<_, Infallible>(registry.clone()))
        .expect("staged");
    one.ops
        .decide_changes_with(staged, |_| vec![AuditEntry::new("alice", "repository_put")])
        .expect("committed");
    assert_eq!(moved(&after_accounts, &versions(&db)), [Part::Ops]);
}

/// Proves: a change whose edit refuses (its closure answers an error) writes nothing: no row, no
/// audit line, no version moved on and the counter where it was, and it holds nothing after: the
/// next change takes the number the refused one would have.
///
/// Catches: a refused change whose transaction is left open, holding the gate, or committed, and
/// one that takes an audit number, which would leave a gap.
#[test]
fn an_edit_refused_by_its_change_writes_nothing_and_holds_nothing() {
    let db = TestDatabase::new();
    let one = instance(&db, "one");
    let first = label(&one.ops, "host-1", "rack", "r1");
    let (versions_then, counter_then) = (versions(&db), counter(&db));
    let refused = one.ops.try_change(
        Write::BeforeMemory,
        AuditEntry::new("alice", "exclude"),
        |state| {
            state.hosts.clear();
            Err(crate::ops::OpsError::AuditUnavailable(
                std::io::Error::other("the change refuses"),
            ))
        },
    );
    assert!(refused.is_err());
    assert_eq!(held(&db), []);
    assert_eq!(versions(&db), versions_then);
    assert_eq!(counter(&db), counter_then);
    assert_eq!(rows_of_labels(&db).len(), 1);
    let next = label(&one.ops, "host-2", "rack", "r2");
    assert_eq!(next.seq, first.seq + 1);
    assert_eq!(audit_numbers(&db), [first.seq, next.seq]);
}

/// Proves: a change that writes a part its edit does not hold is refused before anything is sent to
/// the database, naming the part: a line of the ring of changes is a write of the ops part, and an
/// edit of the desired state alone does not hold it. The same change in an edit that holds both is
/// planned, with each part it writes to be moved on.
///
/// Catches: a commit that takes a part's row after the counter (outside the one order) because its
/// edit did not lock it first, and a version not moved on for a part that was written.
#[test]
fn a_change_of_a_part_its_edit_does_not_hold_is_refused() {
    let held = crate::store::conformance_tests::lines(1, 1);
    let change = crate::store::Commit {
        audit: &held,
        ..crate::store::Commit::default()
    };
    let refused = crate::store::postgres::edit::plan(&change, Scope::DESIRED)
        .expect_err("the ops part is not held");
    assert!(refused.to_string().contains("ops"), "{refused}");
    let (statements, bumps) =
        crate::store::postgres::edit::plan(&change, Scope::DESIRED_AND_OPS).expect("planned");
    assert_eq!(statements.len(), 1);
    assert_eq!(bumps.into_iter().collect::<Vec<_>>(), [Part::Ops]);
}
