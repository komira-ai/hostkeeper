use sha2::{Digest, Sha256};

use super::*;

/// The SHA-256 of each shipped migration file, in hex. A shipped migration is never edited: a
/// change is a migration of its own, so these only grow.
const PINNED: [(i32, &str, &str); 2] = [
    (
        1,
        "state",
        "6bbe0733ad81d8ae91f252c45653ab9bb3e08cc5f5bd7707f75b087042e540f5",
    ),
    (
        2,
        "first_serving",
        "f0f413e9d9c8347115e4ee48e86ec6972525b3e8e13d45ba8694517f7197ae74",
    ),
];

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

const REFUSE: &str = "-- older-releases: refuse\n";

fn refused(sql: &'static str) -> String {
    match migration(1, "test", sql) {
        Err(e) => e.to_string(),
        Ok(_) => "accepted".to_owned(),
    }
}

fn what(statement: &str) -> String {
    format!("migration 1 (test): {statement} is refused; a migration only adds")
}

/// Proves: the migrations this build ships are the ones that were released: their numbers and
/// names are exactly [(1, "state"), (2, "first_serving")], each file's SHA-256 is the pinned one, and every one passes
/// the header rule, the lint and the numbering.
///
/// Catches: an edit to a shipped migration (a database that ran the old text would differ from
/// one that runs the new), and a shipped file the runner would refuse at every start.
#[test]
fn the_shipped_migrations_are_unchanged() {
    let listed: Vec<(i32, &str)> = SHIPPED.iter().map(|(n, name, _)| (*n, *name)).collect();
    assert_eq!(listed, vec![(1, "state"), (2, "first_serving")]);
    let hashes: Vec<(i32, &str, String)> = SHIPPED
        .iter()
        .map(|(n, name, sql)| (*n, *name, hex(&Sha256::digest(sql.as_bytes()))))
        .collect();
    let pinned: Vec<(i32, &str, String)> = PINNED
        .iter()
        .map(|(n, name, sha)| (*n, *name, (*sha).to_owned()))
        .collect();
    assert_eq!(hashes, pinned);
    let set = checked(&SHIPPED).expect("every shipped migration passes");
    let read: Vec<(i32, &str, OlderReleases)> =
        set.iter().map(|m| (m.version, m.name, m.older)).collect();
    assert_eq!(
        read,
        vec![
            (1, "state", OlderReleases::Refuse),
            (2, "first_serving", OlderReleases::Refuse)
        ]
    );
}

/// Proves: the lint refuses each statement that takes something away or ends the transaction,
/// by the exact text that says which, and accepts the statements that only add; words in
/// comments and quoted text are not read, and a statement inside a dollar-quoted body is.
///
/// Catches: a migration that drops, renames, retypes, truncates or deletes, adds a NOT NULL
/// column an older release cannot fill, or commits the runner's transaction early.
#[test]
fn the_migration_lint_refuses_a_drop_and_a_rename() {
    let cases: [(&'static str, &str); 15] = [
        ("DROP TABLE t;", "a DROP"),
        ("drop index i;", "a DROP"),
        ("ALTER TABLE t RENAME TO u;", "a RENAME"),
        ("ALTER TABLE t RENAME COLUMN a TO b;", "a RENAME"),
        ("ALTER TABLE t DROP COLUMN c;", "a DROP"),
        (
            "ALTER TABLE t ALTER COLUMN c TYPE bigint;",
            "a column type change",
        ),
        ("TRUNCATE t;", "a TRUNCATE"),
        ("DELETE FROM t;", "a DELETE"),
        (
            "ALTER TABLE t ADD COLUMN c int NOT NULL;",
            "a NOT NULL column added without a default",
        ),
        ("COMMIT;", "a statement that ends the transaction"),
        ("ROLLBACK;", "a statement that ends the transaction"),
        ("SAVEPOINT s;", "a statement that ends the transaction"),
        ("ABORT;", "a statement that ends the transaction"),
        ("RELEASE s;", "a statement that ends the transaction"),
        (
            "CREATE FUNCTION f() RETURNS void LANGUAGE sql AS $$ DROP TABLE t; $$;",
            "a DROP",
        ),
    ];
    for (sql, statement) in cases {
        assert_eq!(lint(sql), Some(statement), "{sql}");
    }
    let with_header: Vec<String> = cases
        .iter()
        .map(|(sql, _)| refused(Box::leak(format!("{REFUSE}{sql}").into_boxed_str())))
        .collect();
    let want: Vec<String> = cases.iter().map(|(_, s)| what(s)).collect();
    assert_eq!(with_header, want);

    let accepted = [
        "CREATE TABLE t (a int NOT NULL, b text);",
        "ALTER TABLE t ADD COLUMN c int NOT NULL DEFAULT 0;",
        "ALTER TABLE t ADD COLUMN c int;",
        "ALTER TABLE t ADD COLUMN c int CHECK (NOT (c IS NULL OR c = 1));",
        "ALTER TABLE t ALTER COLUMN c SET DEFAULT 1;",
        "CREATE INDEX i ON t (a, lower(b)) WHERE a > 1 - 1;",
        "-- DROP TABLE t\nSELECT 1;",
        "SELECT 'DROP TABLE t; DELETE FROM t';",
        "SELECT 'it''s a DROP', \"TRUNCATE\", \"say \"\"DELETE\"\"\";",
        "SELECT 1 -- COMMIT;\n;",
        "SELECT 'unterminated",
        "",
    ];
    for sql in accepted {
        assert_eq!(lint(sql), None, "{sql}");
    }
}

/// Proves: a first line that is not `-- older-releases: refuse` or `-- older-releases: may-run`
/// (missing, other, misspelt, not first) is refused with the one text that names the migration;
/// both spellings are read, with surrounding whitespace dropped, into the rule they name.
///
/// Catches: a migration that ships without saying what an older release must do on a database
/// that holds it.
#[test]
fn a_migration_without_its_header_rule_is_refused() {
    let no_rule = "migration 1 (test): its first line is not `-- older-releases: refuse` or `-- older-releases: may-run`";
    for sql in [
        "",
        "CREATE TABLE t (a int);",
        "-- older-releases: always\nSELECT 1;",
        "-- older releases: refuse\nSELECT 1;",
        "\n-- older-releases: refuse\nSELECT 1;",
    ] {
        assert_eq!(refused(sql), no_rule, "{sql:?}");
    }
    let refuse = migration(1, "test", "  -- older-releases: refuse  \nSELECT 1;").expect("refuse");
    assert_eq!(refuse.older, OlderReleases::Refuse);
    let may_run = migration(2, "later", "-- older-releases: may-run\nSELECT 1;").expect("may-run");
    assert_eq!(
        (may_run.version, may_run.name, may_run.older),
        (2, "later", OlderReleases::MayRun)
    );
    assert_eq!(
        (
            OlderReleases::Refuse.to_string(),
            OlderReleases::MayRun.to_string()
        ),
        ("refuse".to_owned(), "may-run".to_owned())
    );
}

/// Proves: numbers that do not run 1, 2, 3 and on (a gap, a repeat, a start above 1) are refused
/// naming the pair, a refused migration inside a set is refused in the set, and a set that runs
/// in order is taken in that order.
///
/// Catches: a migration numbered out of order that the runner would skip or apply twice.
#[test]
fn migrations_numbered_out_of_order_are_refused() {
    let ok: &'static str = "-- older-releases: refuse\nSELECT 1;";
    let gap = checked(&[(1, "a", ok), (3, "c", ok)]).expect_err("a gap");
    assert_eq!(
        gap,
        MigrationError::Numbering {
            before: 1,
            found: 3
        }
    );
    assert_eq!(
        gap.to_string(),
        "migration 3 follows 1: the numbers run 1, 2, 3 and on"
    );
    let repeat = checked(&[(1, "a", ok), (1, "b", ok)]).expect_err("a repeat");
    assert_eq!(
        repeat,
        MigrationError::Numbering {
            before: 1,
            found: 1
        }
    );
    let start = checked(&[(2, "b", ok)]).expect_err("a start above 1");
    assert_eq!(
        start.to_string(),
        "migration 2 follows 0: the numbers run 1, 2, 3 and on"
    );

    let bad = checked(&[
        (1, "a", ok),
        (2, "b", "-- older-releases: may-run\nDROP TABLE t;"),
    ])
    .expect_err("a refused migration");
    assert_eq!(
        bad.to_string(),
        "migration 2 (b): a DROP is refused; a migration only adds"
    );

    let set = checked(&[
        (1, "a", ok),
        (2, "b", "-- older-releases: may-run\nSELECT 2;"),
    ])
    .expect("a set in order");
    let order: Vec<(i32, &str, &str)> = set
        .iter()
        .map(|m| (m.version, m.name, m.older.as_str()))
        .collect();
    assert_eq!(order, vec![(1, "a", "refuse"), (2, "b", "may-run")]);
}
