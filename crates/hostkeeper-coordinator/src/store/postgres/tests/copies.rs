//! Each store's copy of its part over Postgres (`docs/design/coordinator-multi-instance.md`,
//! section 6): an edit of any part by a coordinator whose copy is older than the rows is made on
//! the rows; a copy read again when nothing moved stays as it is; a copy that cannot be read again
//! keeps serving; rows this build cannot read refuse the edit; and a stopped store begins no edit.
//!
//! Two coordinators are two stores opened on one database. Nothing sleeps and no time is measured.

use hostkeeper_wire::wire::{OrderId, Outcome, UpdateResult};

use super::TestDatabase;
use super::edits::{Instance, instance, label, labels, version};
use crate::store::conformance_tests::rich_accounts;
use crate::store::{Backend, Health, Part};
use crate::testutil::{desired, id, version as release};
use crate::verdict_file::VerdictsError;

/// A heartbeat of `host` that reports a failed update of `web`, as `instance` acknowledges it.
fn beat(instance: &Instance, host: &str) -> Result<(), VerdictsError> {
    let report = UpdateResult {
        order_id: OrderId::parse("order-1"),
        order_try: Some(1),
        ..UpdateResult::new(release("2.0"), Outcome::Failed)
    };
    instance
        .verdicts
        .record(&id(host), &[("web".to_owned(), Some(report))], &[])
}

/// The hosts whose verdict about `web` stands in `instance`'s copy.
fn standing(instance: &Instance) -> Vec<String> {
    ["host-1", "host-2"]
        .into_iter()
        .filter(|host| instance.verdicts.standing("web", &id(host)).is_some())
        .map(str::to_owned)
        .collect()
}

/// What `instance`'s copies hold of the parts the tests change: web's desired version, the labels,
/// whether the accounts are the rich ones, the layout's name and the hosts whose verdict stands.
fn copies(instance: &Instance) -> (Option<String>, usize, bool, String, Vec<String>) {
    (
        instance
            .desired
            .get("web")
            .map(|d| d.version.as_str().to_owned()),
        labels(&instance.ops.state()).len(),
        instance.accounts.snapshot() == rich_accounts(),
        instance.layout.snapshot().name().to_owned(),
        standing(instance),
    )
}

/// `other` changes every part: web's desired version, a label, the accounts, the layout's name,
/// and a verdict of `host-1`.
fn change_every_part(other: &Instance) {
    other
        .desired
        .set("web", desired("2.0", 1, 1))
        .expect("the desired state is set");
    label(&other.ops, "host-1", "rack", "r1");
    other
        .accounts
        .change(|_| Ok(rich_accounts()))
        .expect("the accounts are set");
    other
        .layout
        .change(|held| Ok((held.with_name("lab-b").expect("a name"), ())))
        .expect("the layout is named");
    beat(other, "host-1").expect("the verdict is acknowledged");
}

/// Proves: a coordinator whose copies of every part are older than the rows (another coordinator
/// changed each) makes its next edit of each on the rows: its desired set keeps the other's
/// version of web, its edit of the accounts and of the layout sees the other's, and its heartbeat's
/// verdict is acknowledged beside the other's. Its copies then carry the versions they were read
/// at: no part is found behind after.
///
/// Catches: an edit of a part made on a stale copy, whose diff would put back what the other
/// coordinator changed (a desired version set back, the accounts or a verdict lost).
#[test]
fn an_edit_of_any_stale_part_is_made_on_the_rows() {
    let db = TestDatabase::new();
    let (a, b) = (instance(&db, "a"), instance(&db, "b"));
    change_every_part(&b);
    let mut stale = a.store.stale();
    stale.sort();
    assert_eq!(stale, Part::ALL);

    a.desired
        .set("db", desired("1.0", 1, 1))
        .expect("a sets db on the rows");
    assert_eq!(
        a.desired.get("web").map(|d| d.version.as_str().to_owned()),
        Some("2.0".to_owned())
    );
    let mut seen = (false, String::new());
    a.accounts
        .change(|held| {
            seen.0 = *held == rich_accounts();
            Ok(held.clone())
        })
        .expect("a's edit of the accounts");
    a.layout
        .change(|held| {
            seen.1 = held.name().to_owned();
            Ok((held.clone(), ()))
        })
        .expect("a's edit of the layout");
    assert_eq!(seen, (true, "lab-b".to_owned()));
    beat(&a, "host-2").expect("a acknowledges host-2");
    assert_eq!(standing(&a), ["host-1", "host-2"]);
    assert_eq!(
        db.rows("SELECT node_id FROM verdict_standing ORDER BY node_id")
            .len(),
        2
    );
    label(&a.ops, "host-2", "rack", "r2");
    assert_eq!(labels(&a.ops.state()).len(), 2);
    assert_eq!(a.store.stale(), []);
}

/// Proves: a request reads again the parts another coordinator changed, and every store's copy
/// then holds them; a part read again when its version did not move is left as it is (the copy
/// is not replaced, and nothing is found behind after).
///
/// Catches: a read of a part again that replaces a copy that was not behind, which would drop
/// what memory holds beside the rows, and one that never takes the rows.
#[test]
fn a_copy_read_again_holds_the_rows_and_one_not_behind_stays_as_it_is() {
    let db = TestDatabase::new();
    let (a, b) = (instance(&db, "a"), instance(&db, "b"));
    change_every_part(&b);
    let before = copies(&a);
    assert_eq!(before, (None, 0, false, String::new(), Vec::new()));
    let refresh = |instance: &Instance| {
        instance.desired.refresh();
        instance.ops.refresh();
        instance.accounts.refresh();
        instance.layout.refresh();
        instance.verdicts.refresh();
    };
    refresh(&a);
    let read = copies(&a);
    assert_eq!(
        read,
        (
            Some("2.0".to_owned()),
            1,
            true,
            "lab-b".to_owned(),
            vec!["host-1".to_owned()]
        )
    );
    assert_eq!(a.store.stale(), []);
    refresh(&a);
    assert_eq!(copies(&a), read);
    assert_eq!(a.store.stale(), []);
}

/// Proves: while the parts' versions cannot be read (their table is not there), a coordinator
/// finds no part behind and every store keeps serving its copy when asked to read its part again;
/// once the versions can be read, the parts another coordinator changed meanwhile are found behind
/// and read again.
///
/// Catches: a request that fails, or a copy that is emptied, because a read of the versions or of
/// a part failed, and a failure that is taken as "nothing changed" for good.
#[test]
fn a_part_that_cannot_be_read_again_keeps_its_copy() {
    let db = TestDatabase::new();
    let (a, b) = (instance(&db, "a"), instance(&db, "b"));
    change_every_part(&b);
    db.run("ALTER TABLE state_versions RENAME TO state_versions_away")
        .expect("the versions are taken away");
    assert_eq!(a.store.stale(), []);
    a.desired.refresh();
    a.ops.refresh();
    a.accounts.refresh();
    a.layout.refresh();
    a.verdicts.refresh();
    assert_eq!(copies(&a), (None, 0, false, String::new(), Vec::new()));
    db.run("ALTER TABLE state_versions_away RENAME TO state_versions")
        .expect("the versions are back");
    let mut stale = a.store.stale();
    stale.sort();
    assert_eq!(stale, Part::ALL);
}

/// Proves: an edit of a part whose rows this build cannot read (a row another writer left that
/// does not convert) is refused, naming the row, with nothing written, and the store stays up:
/// the edit read the part afresh because its copy was behind, and the rows were not taken into
/// memory.
///
/// Catches: an edit made on a copy whose rows could not be read again, and a refusal that loses
/// the session.
#[test]
fn an_edit_of_rows_this_build_cannot_read_is_refused_and_writes_nothing() {
    let db = TestDatabase::new();
    let one = instance(&db, "one");
    let ops_at = version(&db, Part::Ops);
    db.run(
        "INSERT INTO services (service, last_good) VALUES ('web', 'nope'); \
         UPDATE state_versions SET version = version + 1 WHERE part = 'ops'",
    )
    .expect("a row this build cannot read");
    let mut entry = crate::ops::AuditEntry::new("alice", "set_labels");
    entry.node_id = Some("host-1".to_owned());
    let refused = one
        .ops
        .change(crate::ops::Write::BeforeMemory, entry, |_| {})
        .expect_err("the rows do not read");
    assert!(refused.to_string().contains("web"), "{refused}");
    assert_eq!(one.store.health(), Health::Up);
    assert_eq!(version(&db, Part::Ops), ops_at + 1);
    assert!(db.rows("SELECT seq FROM audit").is_empty());
}

/// Proves: a store whose writing sessions are stopped begins no edit: a change is refused, the
/// store says it takes no writes, the next change is refused at once too, and no part is read
/// again for a request.
///
/// Catches: an edit that waits for a writer that is gone, or that reads the versions over a store
/// that is stopped.
#[test]
fn a_stopped_store_begins_no_edit() {
    let db = TestDatabase::new();
    let (one, other) = (instance(&db, "one"), instance(&db, "other"));
    one.store.stop();
    let mut entry = crate::ops::AuditEntry::new("alice", "exclude");
    entry.node_id = Some("host-1".to_owned());
    let first = one
        .ops
        .change(crate::ops::Write::BeforeMemory, entry.clone(), |_| {});
    assert!(first.is_err());
    assert_eq!(one.store.health(), Health::Lost);
    let again = one
        .ops
        .change(crate::ops::Write::BeforeMemory, entry, |_| {});
    assert!(again.is_err());
    label(&other.ops, "host-1", "rack", "r1");
    assert_eq!(one.store.stale(), []);
}

/// Proves: a store asked to read its part again while one of its changes holds its writers' lock
/// (the change may be waiting for the gate) does not wait for it: the read is skipped at once, the
/// copy is as it was, and the change goes on; once the change ended, a read takes the rows. Asked
/// on the thread of the change itself, a wait would never end.
///
/// Catches: a request (each heartbeat among them) that waits on a runtime thread for a store's
/// lock held across a wait for the gate, so a few such requests stall every request of the
/// coordinator.
#[test]
fn a_copy_being_changed_is_not_waited_for_by_a_request() {
    let db = TestDatabase::new();
    let (a, b) = (instance(&db, "a"), instance(&db, "b"));
    change_every_part(&b);
    let staged = a
        .desired
        .stage_repositories(|registry| Ok::<_, std::convert::Infallible>(registry.clone()))
        .expect("staged");
    a.desired.refresh();
    assert_eq!(a.desired.get("web"), None, "the read was skipped");
    drop(staged);
    let mut entry = crate::ops::AuditEntry::new("alice", "exclude");
    entry.node_id = Some("host-1".to_owned());
    a.ops
        .change(crate::ops::Write::BeforeMemory, entry, |_| a.ops.refresh())
        .expect("the change goes on");
    a.accounts
        .change(|held| {
            a.accounts.refresh();
            Ok(held.clone())
        })
        .expect("the accounts' change goes on");
    a.layout
        .change(|held| {
            a.layout.refresh();
            Ok((held.clone(), ()))
        })
        .expect("the layout's change goes on");
    let holder = super::coordinator::Holding::start(
        &db,
        "BEGIN; SELECT 1 FROM hostkeeper.state_versions WHERE part = 'verdicts' FOR UPDATE",
        "COMMIT",
    );
    std::thread::scope(|scope| {
        let writing = scope.spawn(|| beat(&a, "host-2"));
        super::migrate_ddl::seen_waiting(&db, "state_versions");
        // The write holds the verdicts' lock while it waits: a read that waited for it would
        // never return before the holder is released below.
        a.verdicts.refresh();
        holder.release();
        writing
            .join()
            .expect("the write ends")
            .expect("acknowledged");
    });
    a.desired.refresh();
    a.verdicts.refresh();
    assert_eq!(standing(&a), ["host-1", "host-2"]);
    assert_eq!(
        copies(&a),
        (
            Some("2.0".to_owned()),
            1,
            true,
            "lab-b".to_owned(),
            vec!["host-1".to_owned(), "host-2".to_owned()]
        )
    );
}

/// Proves: an edit whose lock cannot be taken for a reason other than a wait (the counter's table
/// is not there) is refused with the server's words, nothing written, and the store keeps its
/// session: once the table is back, the same change is made.
///
/// Catches: a failure of the lock phase taken as a wait that ran out (and its flag named), and one
/// that loses the session.
#[test]
fn an_edit_whose_lock_cannot_be_taken_is_refused_and_keeps_the_session() {
    let db = TestDatabase::new();
    let one = instance(&db, "one");
    db.run("ALTER TABLE audit_counter RENAME TO audit_counter_away")
        .expect("the counter is taken away");
    let mut entry = crate::ops::AuditEntry::new("alice", "exclude");
    entry.node_id = Some("host-1".to_owned());
    let refused = one
        .ops
        .change(crate::ops::Write::BeforeMemory, entry, |_| {})
        .expect_err("no counter");
    let why = refused.to_string();
    assert!(why.contains("audit_counter"), "{why}");
    assert!(!why.contains("--operator-gate-wait-ms"), "{why}");
    assert_eq!(one.store.health(), Health::Up);
    db.run("ALTER TABLE audit_counter_away RENAME TO audit_counter")
        .expect("the counter is back");
    assert_eq!(label(&one.ops, "host-1", "rack", "r1").seq, 1);
}
