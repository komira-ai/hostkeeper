//! The lab's layout over the Postgres store (migration `0015`): the tables `lab_rows`,
//! `lab_racks` and `lab_placements`, what a commit writes to them, how a load reads them, and what
//! a commit of unknown outcome gives back to memory.

use std::sync::Arc;

use super::TestDatabase;
use crate::layout::{Layout, LayoutStore};
use crate::store::conformance_tests::rich_layout;
use crate::store::postgres::{Landing, Owned};
use crate::store::{Backend, Commit, Shared};

fn names(db: &TestDatabase, table: &str, column: &str) -> Vec<String> {
    db.rows(&format!("SELECT {column} FROM {table} ORDER BY {column}"))
        .into_iter()
        .map(|row| row[0].clone().unwrap_or_default())
        .collect()
}

/// Proves: a commit of the layout writes one row to each table for each row, rack, sled and
/// placed subject, keyed by id (a subject on a sled by its key) and none to `lab_placements`
/// (which the release before reads), a load reads the same layout, and a commit that unplaces a
/// subject (and so removes its one-slot sled) and renames a row changes those rows only.
///
/// Catches: a layout that is committed to memory and not to the tables, which a restart loses.
#[test]
fn a_commit_writes_a_row_for_each_item_and_a_load_reads_them_back() {
    let db = TestDatabase::new();
    let (store, loaded) = db.open("coordinator").expect("the store opens");
    assert!(loaded.layout.is_empty());
    let held = rich_layout();
    store
        .commit(&Commit {
            layout: Some((&Layout::default(), &held)),
            ..Commit::default()
        })
        .expect("committed");
    assert_eq!(names(&db, "lab_rows", "id"), ["row-1"]);
    assert_eq!(names(&db, "lab_racks", "id"), ["rack-1", "rack-2"]);
    assert!(names(&db, "lab_placements", "subject").is_empty());
    assert_eq!(names(&db, "lab_devices", "id"), ["net.1"]);
    assert_eq!(
        names(&db, "lab_sleds", "id"),
        ["sled-1", "sled-2", "sled-3", "sled-4"]
    );
    assert_eq!(
        names(&db, "lab_sled_items", "subject"),
        [
            "host:node-1",
            "host:node-2",
            "inventory:net.1",
            "inventory:rec-7",
            "inventory:rec-8"
        ]
    );
    store.stop();
    let (store, loaded) = db.open("again").expect("the store opens again");
    assert_eq!(loaded.layout, held);

    let subject = crate::layout::Subject {
        kind: crate::layout::Kind::Host,
        id: "node-1".into(),
    };
    let after = held
        .without_placement(&subject)
        .and_then(|next| next.with_name("Renamed"))
        .expect("an edit");
    store
        .commit(&Commit {
            layout: Some((&held, &after)),
            ..Commit::default()
        })
        .expect("committed");
    assert_eq!(
        names(&db, "lab_sled_items", "subject"),
        [
            "host:node-2",
            "inventory:net.1",
            "inventory:rec-7",
            "inventory:rec-8"
        ]
    );
    assert_eq!(
        names(&db, "lab_sleds", "id"),
        ["sled-2", "sled-3", "sled-4"],
        "the one-slot sled of the unplaced host went with it"
    );
    assert_eq!(
        db.rows("SELECT record #>> '{}' FROM lab_cluster"),
        [vec![Some("Renamed".to_owned())]]
    );
    store.stop();
}

/// Proves: migration `0017` turns the rows of `lab_placements` (rack, U, height) of a database of
/// the release before into one one-slot implicit sled and one subject on its slot 0 for each,
/// numbered in the order of rack, U and key; a load reads them as sleds, `lab_placements` is left
/// as it was for a rollback and is never read again (a commit does not write it), and a restart
/// reads the same layout.
///
/// Catches: servers that vanish from the lab view after an update, or rows read twice (as old
/// placements and as the sleds they became).
#[test]
fn the_placements_of_the_release_before_become_sleds_when_migration_0017_runs() {
    let db = TestDatabase::new();
    let (store, _) = db.open("coordinator").expect("the store opens");
    store.stop();
    // The database as the release before left it: no sled table, the placements in their table.
    db.run("DROP TABLE lab_sled_items; DROP TABLE lab_sleds")
        .expect("the tables go");
    db.run("INSERT INTO lab_rows (id, record) VALUES ('row-1', '{\"number\":1}')")
        .expect("a row");
    db.run(
        "INSERT INTO lab_racks (id, record) VALUES \
         ('rack-1', '{\"row\":\"row-1\",\"number\":1,\"height_u\":20}')",
    )
    .expect("a rack");
    db.run(
        "INSERT INTO lab_placements (subject, record) VALUES \
         ('host:n2', '{\"rack\":\"rack-1\",\"u\":8,\"height_u\":4}'), \
         ('host:n1', '{\"rack\":\"rack-1\",\"u\":2,\"height_u\":1}')",
    )
    .expect("the old rows");
    let (_, _, migration) = crate::store::postgres::migrate::SHIPPED[16];
    db.run(migration).expect("the migration runs on it");
    let (store, loaded) = db.open("again").expect("the store opens");
    assert_eq!(
        loaded
            .layout
            .sleds()
            .iter()
            .map(|s| (s.id.as_str(), s.u, s.height_u, s.implicit))
            .collect::<Vec<_>>(),
        [("sled-1", 2, 1, true), ("sled-2", 8, 4, true)]
    );
    assert_eq!(
        names(&db, "lab_placements", "subject"),
        ["host:n1", "host:n2"]
    );
    let renamed = loaded.layout.with_name("Lab").expect("an edit");
    store
        .commit(&Commit {
            layout: Some((&loaded.layout, &renamed)),
            ..Commit::default()
        })
        .expect("committed");
    assert_eq!(
        names(&db, "lab_placements", "subject"),
        ["host:n1", "host:n2"],
        "the table of the release before is left as it was"
    );
    assert_eq!(names(&db, "lab_sleds", "id"), ["sled-1", "sled-2"]);
    assert_eq!(
        names(&db, "lab_sled_items", "subject"),
        ["host:n1", "host:n2"]
    );
    store.stop();
    let (store, again) = db.open("third").expect("the store opens");
    assert_eq!(again.layout, renamed);
    store.stop();
}

/// Proves: the tables refuse a key that is no id of theirs.
///
/// Catches: a row no reader could name.
#[test]
fn the_tables_refuse_a_bad_key() {
    let db = TestDatabase::new();
    let (store, _) = db.open("coordinator").expect("the store opens");
    store.stop();
    assert!(
        db.run("INSERT INTO lab_rows (id, record) VALUES ('x', '{}')")
            .is_err()
    );
    assert!(
        db.run("INSERT INTO lab_racks (id, record) VALUES ('x', '{}')")
            .is_err()
    );
    assert!(
        db.run("INSERT INTO lab_placements (subject, record) VALUES ('vm:x', '{}')")
            .is_err()
    );
    assert!(
        db.run("INSERT INTO lab_devices (id, record) VALUES ('switch-1', '{}')")
            .is_err()
    );
    assert!(
        db.run("INSERT INTO lab_sleds (id, record) VALUES ('rack-1', '{}')")
            .is_err()
    );
    assert!(
        db.run("INSERT INTO lab_sled_items (subject, record) VALUES ('vm:x', '{}')")
            .is_err()
    );
}

/// Proves: a database whose sled rows break a rule (a server on a sled that is not there, a slot
/// past the sled's slots) is refused at load, naming the table, as the file is.
///
/// Catches: an occupied server whose sled is gone, loaded as if it were fine.
#[test]
fn a_load_refuses_sled_rows_that_break_a_rule() {
    let db = TestDatabase::new();
    let (store, _) = db.open("coordinator").expect("the store opens");
    store.stop();
    db.run(
        "INSERT INTO lab_sled_items (subject, record) VALUES \
         ('host:n1', '{\"sled\":\"sled-4\",\"slot\":1}')",
    )
    .expect("a row");
    let refused = db.open("again").err().expect("refused");
    assert!(refused.to_string().starts_with("database:"), "{refused}");
    assert!(refused.to_string().contains("sled-4"), "{refused}");
}

/// Proves: a commit of the layout whose outcome was not known and that landed is laid over memory
/// by a layout store, row by row: a row it made is taken, one memory changed since stays as memory
/// has it; a landing that did not land, or had no layout, gives nothing.
///
/// Catches: a store that keeps memory as it was before a change the database holds, so a placement
/// made in a commit that landed would not show until a restart.
#[test]
fn a_layout_store_takes_a_landed_change_and_leaves_what_it_had_no_part_in() {
    let held = rich_layout();
    let none = Layout::default();
    let landed = |commit: &Commit<'_>, landed: bool| Landing {
        number: 1,
        landed,
        change: Some(Owned::of(commit)),
    };
    let change = Commit {
        layout: Some((&none, &held)),
        ..Commit::default()
    };
    let backend: Shared = Arc::new(crate::store::Fake::new());

    let store = LayoutStore::over(Arc::clone(&backend), none.clone());
    store.take_landed(&landed(&Commit::default(), true));
    store.take_landed(&landed(&change, false));
    assert_eq!(store.snapshot(), none, "nothing landed, nothing taken");
    store.take_landed(&landed(&change, true));
    assert_eq!(store.snapshot(), held);

    let store = LayoutStore::over(Arc::clone(&backend), none.clone());
    store
        .change(|memory| memory.with_name("Mine").map(|next| (next, ())))
        .expect("memory changed since");
    store.take_landed(&landed(&change, true));
    let taken = store.snapshot();
    assert!(taken.name() == "Mine", "what memory made since stays");
    assert_eq!(
        taken.racks().len(),
        2,
        "the racks of the landed change are taken"
    );
}
