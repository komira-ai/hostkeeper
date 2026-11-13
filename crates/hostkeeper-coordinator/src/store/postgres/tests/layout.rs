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

/// Proves: a commit of the layout writes one row to each table for each row, rack and placement,
/// keyed by id (a placement by its subject's key), a load reads the same layout, and a commit that
/// removes a placement and renames a row changes those rows only.
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
    assert_eq!(
        names(&db, "lab_placements", "subject"),
        ["host:node-1", "inventory:rec-7"]
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
    assert_eq!(names(&db, "lab_placements", "subject"), ["inventory:rec-7"]);
    assert_eq!(
        db.rows("SELECT record #>> '{}' FROM lab_cluster"),
        [vec![Some("Renamed".to_owned())]]
    );
    store.stop();
}

/// Proves: a database whose rows break a rule (a placement in a rack that is not there) is
/// refused at load, naming the table, and the table's own checks refuse a key that is no id.
///
/// Catches: a database loaded more loosely than a file.
#[test]
fn a_load_refuses_rows_that_break_a_rule_and_the_tables_refuse_a_bad_key() {
    let db = TestDatabase::new();
    let (store, _) = db.open("coordinator").expect("the store opens");
    store.stop();
    db.run(
        "INSERT INTO lab_placements (subject, record) VALUES \
         ('host:n1', '{\"rack\":\"rack-9\",\"u\":1,\"height_u\":1}')",
    )
    .expect("a row");
    let refused = db.open("again").err().expect("refused");
    assert!(refused.to_string().starts_with("database:"), "{refused}");
    assert!(refused.to_string().contains("rack-9"), "{refused}");
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
