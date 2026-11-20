//! Migration `0027` (the audit's `api_token`, the API token an operator called with) and the
//! release before, both ways: this release keeps the token of a line and reads the lines the
//! release before wrote as holding none; the release before runs on a database this release
//! migrated, and the rows it writes leave the column NULL. Nothing here reads a clock.

use super::{TestDatabase, with_session};
use crate::audit_file::{AuditLine, Filter, Query};
use crate::ops::AuditEntry;
use crate::store::postgres::migrate;
use crate::store::{Backend, Commit};
use crate::tokens::Operator;

/// The line `seq` of `alice` calling with her token `alice-bot`, and the line `seq + 1` of the
/// coordinator.
fn lines(seq: u64) -> Vec<AuditLine> {
    let alice = Operator::by_token(
        "alice".parse().expect("a name"),
        "alice-bot".parse().expect("a name"),
    );
    let mut by_token = AuditEntry::new(&alice, "exclude");
    by_token.seq = seq;
    by_token.node_id = Some("s1".to_owned());
    let mut own = AuditEntry::new("coordinator", "last_good");
    own.seq = seq + 1;
    vec![
        AuditLine::of(&by_token, 1_800_000_000),
        AuditLine::of(&own, 1_800_000_001),
    ]
}

/// The `(by, api_token)` of every entry of a page of the whole log.
fn page(store: &impl Backend) -> Vec<(String, Option<String>)> {
    store
        .audit_page(&Query {
            since_seq: 0,
            limit: 100,
            newest: false,
            filter: Filter::default(),
        })
        .expect("a page")
        .into_iter()
        .map(|logged| (logged.entry.by, logged.entry.api_token))
        .collect()
}

/// Proves: a line made with an API token keeps the token in the column `api_token`, by the owner,
/// and is read back with it by the page, by every line's visit and by the ring a start loads; a
/// line made by no token holds NULL and reads as none.
///
/// Catches: a token the database drops, so the audit of the live store, which is on Postgres,
/// cannot say which of a person's tokens made a change.
#[test]
fn an_audit_line_keeps_the_api_token_it_was_made_with() {
    let db = TestDatabase::new();
    let (store, _) = db.open("this release").expect("the store opens");
    let held = lines(1);
    store
        .commit(&Commit {
            audit: &held,
            ..Commit::default()
        })
        .expect("the lines are kept");
    assert_eq!(
        db.rows("SELECT by_name, api_token FROM audit ORDER BY seq"),
        [
            vec![Some("alice".to_owned()), Some("alice-bot".to_owned())],
            vec![Some("coordinator".to_owned()), None],
        ]
    );
    let expected = vec![
        ("alice".to_owned(), Some("alice-bot".to_owned())),
        ("coordinator".to_owned(), None),
    ];
    assert_eq!(page(&store), expected);
    let mut visited = Vec::new();
    store
        .audit_each(&mut |logged| visited.push((logged.entry.by, logged.entry.api_token)))
        .expect("every line");
    assert_eq!(visited, expected);
    store.stop();
    let (_, loaded) = db.open("again").expect("the store opens again");
    let ring: Vec<(String, Option<String>)> = loaded
        .ops
        .audit
        .into_iter()
        .map(|entry| (entry.by, entry.api_token))
        .collect();
    assert_eq!(ring, expected);
}

/// Proves: the release before (migrations `0001` to `0026`) runs on a database this release
/// migrated, running nothing, and a line written as it writes one (its columns, no `api_token`)
/// holds NULL, which this release reads as no token; this release, started on a database the
/// release before migrated, runs `0027`, and `0028` after it.
///
/// Catches: a `0027` that is not `may-run`, which would turn a rollback into a restore; a column
/// the release before's insert must name (a `NOT NULL` without a default); and a line of the
/// release before read as made with a token.
#[test]
fn the_release_before_runs_on_a_database_with_0027_and_this_one_on_its() {
    let db = TestDatabase::new();
    let (store, _) = db.open("this release").expect("this release starts");
    store.stop();
    let ran = with_session(&db, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..26]).expect("the release before's set");
        migrate::apply(session, &before).expect("the release before is accepted")
    });
    assert_eq!(ran, Vec::<i32>::new());
    db.run(
        "INSERT INTO audit (seq, at_unix, by_name, action, service, node_id, reason, host_group) \
         VALUES (1, 1800000000, 'ops-file', 'exclude', NULL, 's1', NULL, NULL)",
    )
    .expect("the release before's line");
    let (store, _) = db.open("this release again").expect("this release starts");
    assert_eq!(page(&store), [("ops-file".to_owned(), None)]);
    store.stop();

    let old = TestDatabase::new();
    with_session(&old, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..26]).expect("the release before's set");
        migrate::apply(session, &before).expect("the release before migrates");
    });
    assert!(
        old.rows(
            "SELECT 1 FROM information_schema.columns \
             WHERE table_name = 'audit' AND column_name = 'api_token'"
        )
        .is_empty(),
        "the release before has no such column"
    );
    let (store, _) = old.open("this release").expect("this release starts on it");
    store.stop();
    assert_eq!(
        old.rows("SELECT version FROM schema_version WHERE version > 26"),
        [vec![Some("27".to_owned())], vec![Some("28".to_owned())]],
        "0027 ran, and 0028 after it"
    );
}
