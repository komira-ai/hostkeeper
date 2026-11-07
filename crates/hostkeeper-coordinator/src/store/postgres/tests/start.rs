//! The start's load (what it refuses of a database it cannot read), the rows of every column
//! type through a real server, and what a commit of unknown outcome gives back to memory.

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use super::{AT, SPENT_NOW, TestDatabase, off_runtime, unix};
use crate::audit_file::UnixClock;
use crate::ops::{AUDIT_RING, OpsState};
use crate::store::conformance_tests::{desired_at, lines, normal, rich_ops, rich_verdicts};
use crate::store::postgres::rows::{KeyTy, KeyVal, Rows, Stmt, Table, Ty, Val};
use crate::store::postgres::session::Session;
use crate::store::postgres::{DatabaseError, Landing, Owned, Postgres, TIMEOUTS};
use crate::store::{Backend, Commit};

/// The start's error on a database made by one start, then changed by `sql`.
fn refused_after(sql: &str) -> String {
    let db = TestDatabase::new();
    let (store, _) = db.open("first").expect("the store opens");
    store.stop();
    db.run(sql).expect("the database is changed");
    db.open("second")
        .err()
        .unwrap_or_else(|| panic!("the start is refused after {sql}"))
        .to_string()
}

/// Proves: a start on a database whose rows this build cannot read is refused with the text of the
/// table and key it cannot read (a desired state with a version that does not parse, an operations
/// row, a verdict), and a table or column that is not what the load expects is refused with the
/// server's words; nothing is started on it.
///
/// Catches: a start that guesses at a row it cannot read, or that loads part of the state.
#[test]
fn a_database_with_rows_this_build_cannot_read_refuses_the_start() {
    let version_error = hostkeeper_wire::version::Version::parse("nope")
        .expect_err("not a version")
        .to_string();
    let desired = refused_after(
        "INSERT INTO desired (service, version, max_updating, min_up) VALUES ('kbf', 'nope', '{}', '{}')",
    );
    assert!(desired.starts_with("database:desired: "), "{desired}");
    assert_eq!(
        refused_after("INSERT INTO services (service, last_good) VALUES ('kbf', 'nope')"),
        format!("database:services: the row kbf: {version_error}")
    );
    let verdict = refused_after(
        "INSERT INTO verdict_standing (service, node_id, report) VALUES ('kbf', 's1', '{\"x\": 1}')",
    );
    assert!(verdict.starts_with("database:verdicts: "), "{verdict}");

    for table in ["audit", "spent_tokens", "host_labels"] {
        let renamed = refused_after(&format!("ALTER TABLE {table} RENAME TO {table}_away"));
        assert_eq!(
            renamed,
            format!(
                "database:database: ERROR: relation \"{table}\" does not exist (SQLSTATE 42P01)"
            ),
            "{table}"
        );
    }
    let retyped = refused_after(
        "INSERT INTO host_labels VALUES ('s1', 'k', 'v'); \
         ALTER TABLE host_labels ALTER COLUMN value TYPE bytea USING value::bytea",
    );
    assert!(
        retyped.starts_with("database:database: error deserializing column 2"),
        "{retyped}"
    );
}

/// Proves: a start whose clock reads above the largest counter the database keeps is refused with
/// the text that names it, and a start that is not given a bound of its own is the one the
/// coordinator makes, which loads the ids of CI's tokens still standing.
///
/// Catches: a lease time written wrapped, and a start that loads ids already past.
#[test]
fn a_start_reads_its_clock_and_loads_the_spent_ids_still_standing() {
    let db = TestDatabase::new();
    let far: UnixClock = Arc::new(|| u64::MAX);
    let error = Postgres::open_with(&db.dsn(), TIMEOUTS, "far", &far, Arc::new(|| SPENT_NOW))
        .err()
        .expect("the clock is refused");
    assert!(matches!(error, DatabaseError::Load(_)));
    assert_eq!(
        error.to_string(),
        "database:counter: 18446744073709551615 is above the largest counter the database keeps"
    );

    let (store, _) = Postgres::open(&db.dsn(), "near", &unix(), Arc::new(|| SPENT_NOW))
        .expect("the store opens");
    store.spend("past", SPENT_NOW, 1).expect("an id");
    store.spend("standing", SPENT_NOW + 1, 1).expect("an id");
    store.stop();
    let (again, loaded) = db.open("again").expect("the store opens again");
    assert_eq!(loaded.spent, vec![("standing".to_owned(), SPENT_NOW + 1)]);
    assert_eq!(
        db.rows("SELECT holder, since_unix FROM lease"),
        vec![vec![Some("again".to_owned()), Some(AT.to_string())]]
    );
    again.stop();
}

const EVERYTHING: Table = Table {
    name: "everything",
    keys: &[("n", KeyTy::Int), ("t", KeyTy::Text)],
    values: &[
        ("flag", Ty::Bool),
        ("doc", Ty::Json),
        ("names", Ty::Texts),
        ("word", Ty::Text),
        ("big", Ty::Int),
    ],
};

/// Proves: a row of every column type a table reads (text, bigint, boolean, jsonb, text[]),
/// with nulls, is written by the statement a table makes, read back by its select as the same
/// values, overwritten by a second upsert and removed by the delete; and a column of another type
/// than the table names for it is an error rather than a guess.
///
/// Catches: a column type that is written as one thing and read as another.
#[test]
fn every_column_type_goes_to_the_server_and_back() {
    let db = TestDatabase::new();
    let (store, _) = db.open("types").expect("the store opens");
    store.stop();
    db.run(
        "CREATE TABLE everything (n bigint, t text, flag boolean, doc jsonb, names text[], \
         word text, big bigint, PRIMARY KEY (n, t))",
    )
    .expect("the table is made");
    let key = vec![KeyVal::Int(-5), KeyVal::Text("k".to_owned())];
    let full = vec![
        Val::Bool(true),
        Val::Json(serde_json::json!({"a": [1, 2]})),
        Val::Texts(vec!["x".to_owned(), "y z".to_owned()]),
        Val::Text("w".to_owned()),
        Val::Int(i64::MAX),
    ];
    let nulls = vec![Val::Null; 5];
    let config = db.dsn().config(TIMEOUTS);
    let (written, overwritten, gone, wrong) = {
        let (key, full, nulls) = (key.clone(), full.clone(), nulls.clone());
        off_runtime(move || {
            let mut session =
                Session::connect(&config, Arc::new(AtomicU64::new(0)), false).expect("a session");
            let first = {
                run(&mut session, EVERYTHING.upsert(&key, &full));
                read(&mut session, &EVERYTHING)
            };
            let wrong = read(
                &mut session,
                &Table {
                    name: "everything",
                    keys: &[("n", KeyTy::Text), ("t", KeyTy::Text)],
                    values: &[],
                },
            );
            run(&mut session, EVERYTHING.upsert(&key, &nulls));
            let second = read(&mut session, &EVERYTHING);
            run(&mut session, EVERYTHING.delete(&key));
            (first, second, read(&mut session, &EVERYTHING), wrong)
        })
    };
    let rows = |values: &[Val]| {
        std::collections::BTreeMap::from([(
            vec![KeyVal::Int(-5), KeyVal::Text("k".to_owned())],
            values.to_vec(),
        )])
    };
    assert_eq!(written, Ok(rows(&full)));
    assert_eq!(overwritten, Ok(rows(&nulls)));
    assert_eq!(gone, Ok(std::collections::BTreeMap::new()));
    assert!(wrong.is_err(), "a bigint key is not read as text");
}

fn run(session: &mut Session, statement: Stmt) {
    session
        .execute(&statement.sql, &statement.refs())
        .expect("the statement runs");
}

fn read(session: &mut Session, table: &Table) -> Result<Rows, String> {
    let rows = session.query(&table.select(), &[]).expect("the select");
    table.rows_of(&rows).map_err(|e| e.to_string())
}

fn landed(commit: &Commit<'_>) -> Landing {
    Landing {
        number: 1,
        landed: true,
        change: Some(Owned::of(commit)),
    }
}

/// Proves: the desired state of a commit that landed is laid over memory row by row: a registry
/// entry and a state it made are taken, one it removed is removed, a service that memory changed
/// since stays as memory has it; a change that did not land, or had no desired state, gives
/// nothing; rows memory holds that the change leaves without their service are refused.
///
/// Catches: a landed change that overwrites what memory did since, or a state that does not read.
#[test]
fn a_desired_state_that_landed_is_laid_over_memory() {
    let (before, after) = (desired_at("1.0", 'a'), desired_at("2.0", 'b'));
    let commit = Commit {
        desired: Some((&before, &after)),
        ..Commit::default()
    };
    let landing = landed(&commit);
    assert_eq!(landing.desired_over(&before), Some(after.clone()));

    let moved = desired_at("1.5", 'c');
    let over = landing.desired_over(&moved).expect("laid over");
    assert_eq!(over.desired.get("kbf"), moved.desired.get("kbf"));

    let gone = Commit {
        desired: Some((&before, &desired::Held::default())),
        ..Commit::default()
    };
    let mut with_extra = before.clone();
    with_extra.digests = desired_at("1.0", 'd').digests;
    let registry = desired_at("3.0", 'e');
    with_extra
        .digests
        .get_mut("kbf")
        .expect("kbf")
        .clone_from(&registry.digests["kbf"]);
    assert_eq!(
        landed(&gone).desired_over(&with_extra),
        None,
        "rows without their service"
    );

    let nothing = Commit::default();
    assert_eq!(landed(&nothing).desired_over(&before), None);
    let not_landed = Landing {
        landed: false,
        ..landed(&commit)
    };
    assert_eq!(not_landed.desired_over(&before), None);
    assert!(not_landed.lines().is_empty());
}

use crate::desired;

/// Proves: the operations state of a commit that landed is laid over memory with its audit lines
/// in the ring, in order and each once, the ring cut to its size; a counter memory holds above what
/// the database keeps, or a change without an operations state, gives nothing.
///
/// Catches: an audit line twice in the ring, a ring over its size, and a ring out of order.
#[test]
fn an_operations_state_that_landed_is_laid_over_memory_with_its_audit_lines() {
    let before = OpsState::default();
    let after = rich_ops();
    let held = lines(55, 70);
    let commit = Commit {
        ops: Some((&before, &after)),
        audit: &held,
        ..Commit::default()
    };
    let landing = landed(&commit);
    let mut memory = before.clone();
    memory.audit = lines(1, 60)
        .iter()
        .map(|line| line.clone().into_logged().entry)
        .collect();
    let over = landing.ops_over(&memory).expect("laid over");
    let ring: Vec<u64> = over.audit.iter().map(|entry| entry.seq).collect();
    assert_eq!(ring, (7..=70).collect::<Vec<u64>>());
    assert_eq!(ring.len(), AUDIT_RING);
    let mut want = normal(after.clone());
    want.audit = over.audit.clone();
    assert_eq!(normal(over), want);

    let mut huge = before.clone();
    huge.services
        .entry("x".to_owned())
        .or_default()
        .halt_generation = u64::MAX;
    assert_eq!(landing.ops_over(&huge), None);
    let big_before = Commit {
        ops: Some((&huge, &after)),
        ..Commit::default()
    };
    assert_eq!(landed(&big_before).ops_over(&before), None);
    let big_after = Commit {
        ops: Some((&before, &huge)),
        ..Commit::default()
    };
    assert_eq!(landed(&big_after).ops_over(&before), None);
    assert_eq!(landed(&Commit::default()).ops_over(&before), None);
    let unlanded = Landing {
        landed: false,
        ..landing
    };
    assert_eq!(unlanded.ops_over(&before), None);
}

/// Proves: the verdicts of a commit that landed are laid over memory, and a change that did not
/// land or had none gives nothing.
///
/// Catches: verdicts a landed change made that memory never takes.
#[test]
fn verdicts_that_landed_are_laid_over_memory() {
    let before = crate::verdict_file::Held::default();
    let after = rich_verdicts();
    let commit = Commit {
        verdicts: Some((&before, &after)),
        ..Commit::default()
    };
    let landing = landed(&commit);
    assert_eq!(landing.verdicts_over(&before), Some(after));
    assert_eq!(landed(&Commit::default()).verdicts_over(&before), None);
    let unlanded = Landing {
        landed: false,
        ..landing
    };
    assert_eq!(unlanded.verdicts_over(&before), None);
}

/// Proves: a commit that landed with no change handed over (a records commit) gives memory
/// nothing: no lines and no state laid over.
///
/// Catches: a landing without its change read as an empty change that clears memory.
#[test]
fn a_landing_without_its_change_gives_memory_nothing() {
    let landing = Landing {
        number: 2,
        landed: true,
        change: None,
    };
    assert!(landing.lines().is_empty());
    let none: OpsState = OpsState::default();
    assert_eq!(landing.ops_over(&none), None);
}

/// Proves: the three stores take a landed change into memory: the desired state its commit set,
/// the verdicts it counted, and the operations state with its audit lines no longer pending and
/// the state as last committed laid over too; a landing with no part of a store leaves that
/// store as it was.
///
/// Catches: a store that keeps its memory as it was before a change the database holds, or one
/// that commits a landed change's audit lines again.
#[test]
fn the_stores_take_a_landed_change_and_leave_what_it_had_no_part_in() {
    use crate::desired::DesiredStore;
    use crate::ops::{AuditEntry, OpsStore, Write};
    use crate::store::{Fake, Shared};
    use crate::verdict_file::VerdictFile;
    let fake: Shared = Arc::new(Fake::new());

    let (before, after) = (desired_at("1.0", 'a'), desired_at("2.0", 'b'));
    let desired = DesiredStore::over(Arc::clone(&fake), before.clone());
    desired.take_landed(&landed(&Commit::default()));
    assert_eq!(
        desired.get("kbf").map(|d| d.version.to_string()),
        Some("1.0".to_owned())
    );
    desired.take_landed(&landed(&Commit {
        desired: Some((&before, &after)),
        ..Commit::default()
    }));
    assert_eq!(
        desired.get("kbf").map(|d| d.version.to_string()),
        Some("2.0".to_owned())
    );

    let none = crate::verdict_file::Held::default();
    let verdicts = VerdictFile::over(Arc::clone(&fake), none.clone());
    verdicts.take_landed(&landed(&Commit::default()));
    assert!(verdicts.counted().is_empty());
    let rich = rich_verdicts();
    verdicts.take_landed(&landed(&Commit {
        verdicts: Some((&none, &rich)),
        ..Commit::default()
    }));
    assert_eq!(verdicts.counted().len(), 2);

    // A raise pending (its commit refused), then a change that landed: memory and the state as
    // last committed both take it, and its line is no longer pending.
    let failing = Fake::new();
    failing.fail();
    let ops = OpsStore::over(Arc::new(failing.clone()), OpsState::default());
    // Refused, and standing in memory, pending.
    let refused = ops.change(Write::Halt, AuditEntry::new("coordinator", "halt"), |_| {});
    assert!(refused.is_err());
    assert!(ops.halt_unwritten());
    let mut taken = OpsState::default();
    taken.excluded = rich_ops().excluded;
    let line = lines(7, 7);
    ops.take_landed(&landed(&Commit::default()));
    assert!(
        ops.state().excluded.is_empty(),
        "no operations part, nothing taken"
    );
    ops.take_landed(&landed(&Commit {
        ops: Some((&OpsState::default(), &taken)),
        audit: &line,
        ..Commit::default()
    }));
    assert_eq!(ops.state().excluded, taken.excluded);
    assert!(ops.state().audit.iter().any(|entry| entry.seq == 7));
    failing.heal();
    ops.retry_unwritten(std::time::Duration::ZERO);
    assert!(!ops.halt_unwritten());
    assert_eq!(
        normal(failing.ops()).excluded,
        taken.excluded,
        "the base took it too"
    );
    assert!(
        failing.audit().iter().all(|line| line.seq != 7),
        "a landed line is not committed again"
    );
}
