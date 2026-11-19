use sha2::{Digest, Sha256};

use super::*;

/// The SHA-256 of each shipped migration file, in hex. A shipped migration is never edited: a
/// change is a migration of its own, so these only grow.
const PINNED: [(i32, &str, &str); 22] = [
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
    (
        3,
        "generations",
        "f08728227dac47c098dc32e55d8d40e0727379678205830ef1b5729a6882ac23",
    ),
    (
        4,
        "releases",
        "ac5c3d331b404227ee7de2955e1d559b1c2a7644744e91418d7d6e5f357bc86a",
    ),
    (
        5,
        "mdm_devices",
        "9ffd9dc58ad104072ebccea2c3b570a4f8879f40d2b5271197dd30706ed273d0",
    ),
    (
        6,
        "host_running",
        "02a0c986c0884717965f5526d7af642322c78fdedb115e7809522bf2ef0dd789",
    ),
    (
        7,
        "repositories",
        "afe77486621ecacc805b7892be3aeca7e1549d74129b49e68ee1b5c05206cb66",
    ),
    (
        8,
        "host_groups",
        "3122feea58f3c4327d4ff2416b287ec7c01abf340c6532b0df49243d72efa7ae",
    ),
    (
        9,
        "service_repository",
        "b4ad7fd88245f9047d58d41dec88dc8cae678a51cfdab4180568c4fc8819abaf",
    ),
    (
        10,
        "accounts",
        "8961fcf301e4b8c9894913ca695bb7b3aab8b461bae0e28c3a7a5701c3bd14b9",
    ),
    (
        11,
        "mdm",
        "d3656f75d7e6b204b0a7edfe8512ad74ea9c33098561f6f671b006ee864fdfa0",
    ),
    (
        12,
        "mdm_setup",
        "92d720d9a443e560b9367dc7093714eb29c57ffe247dfae871a3f373b38dc637",
    ),
    (
        13,
        "host_group_all",
        "f8bdbbea8749eb3037852fa02083730f8a9c28bb20132269f3c76fc049b403fa",
    ),
    (
        14,
        "trusted_publishers",
        "cb7fad3a059f9935f50b22dfc673ffb620df403a2befed1854dedd21d26f4475",
    ),
    (
        15,
        "lab_layout",
        "8e1591153ecc4007f3de3a5bc2b25c3edf418997504dcbbadab39cc8f48378ee",
    ),
    (
        16,
        "lab_devices",
        "5dc09c0a65064c2fa99076182870c43cffc07d94894b7cdaadf7385e2a07d3f9",
    ),
    (
        17,
        "lab_sleds",
        "719f9fd937596c6c701a9983a84eeb6105822f663b50edf29b654e427731c869",
    ),
    (
        18,
        "remove_host_group_all",
        "1500ba573e6baf8ce359c6dafa72e6f03314153d4a4619fd49e8444ea32ca271",
    ),
    (
        19,
        "tx_ledger",
        "eba7812553739ea55e1f800be9b86d44d4598081d2d992d9e3fd35d4309a5e13",
    ),
    (
        20,
        "ssh_certs",
        "c35c5def1fc077bc830efda7e4758eca0ff652cd85105787971ba2627003b835",
    ),
    (
        21,
        "ssh_tunnels",
        "6f71a4b2b1dfea1460b2a17ff22e7f3430d3742ee56ae8d18cf5f45244eea969",
    ),
    (
        22,
        "instances",
        "31503dab493d068923fd5e56c68358d533632f26b4cc248f75f0f11bf23efcee",
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
/// names are exactly [(1, "state"), (2, "first_serving"), (3, "generations"), (4, "releases"),
/// (5, "mdm_devices"), (6, "host_running"), (7, "repositories"), (8, "host_groups"),
/// (9, "service_repository"), (10, "accounts"), (11, "mdm"),
/// (12, "mdm_setup"), (13, "host_group_all"), (14, "trusted_publishers"), (15, "lab_layout"), (16, "lab_devices"), (17, "lab_sleds"), (18, "remove_host_group_all"), (19, "tx_ledger"), (20, "ssh_certs"), (21, "ssh_tunnels"), (22, "instances")], each file's
/// SHA-256 is the pinned one, and every one passes the header rule, the lint and the numbering;
/// the first three and `host_groups` say older releases must not run on them, and `releases`,
/// `mdm_devices`, `host_running`, `repositories`, `service_repository`, `accounts`, `mdm`, `mdm_setup`, `host_group_all`, `trusted_publishers`, `lab_layout`, `lab_devices`, `lab_sleds`, `remove_host_group_all`, `tx_ledger`, `ssh_certs`, `ssh_tunnels` and `instances` that
/// they may (an older release never reads any of those tables).
///
/// Catches: an edit to a shipped migration (a database that ran the old text would differ from
/// one that runs the new), and a shipped file the runner would refuse at every start.
#[test]
fn the_shipped_migrations_are_unchanged() {
    let listed: Vec<(i32, &str)> = SHIPPED.iter().map(|(n, name, _)| (*n, *name)).collect();
    assert_eq!(
        listed,
        vec![
            (1, "state"),
            (2, "first_serving"),
            (3, "generations"),
            (4, "releases"),
            (5, "mdm_devices"),
            (6, "host_running"),
            (7, "repositories"),
            (8, "host_groups"),
            (9, "service_repository"),
            (10, "accounts"),
            (11, "mdm"),
            (12, "mdm_setup"),
            (13, "host_group_all"),
            (14, "trusted_publishers"),
            (15, "lab_layout"),
            (16, "lab_devices"),
            (17, "lab_sleds"),
            (18, "remove_host_group_all"),
            (19, "tx_ledger"),
            (20, "ssh_certs"),
            (21, "ssh_tunnels"),
            (22, "instances")
        ]
    );
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
            (2, "first_serving", OlderReleases::Refuse),
            (3, "generations", OlderReleases::Refuse),
            (4, "releases", OlderReleases::MayRun),
            (5, "mdm_devices", OlderReleases::MayRun),
            (6, "host_running", OlderReleases::MayRun),
            (7, "repositories", OlderReleases::MayRun),
            (8, "host_groups", OlderReleases::Refuse),
            (9, "service_repository", OlderReleases::MayRun),
            (10, "accounts", OlderReleases::MayRun),
            (11, "mdm", OlderReleases::MayRun),
            (12, "mdm_setup", OlderReleases::MayRun),
            (13, "host_group_all", OlderReleases::MayRun),
            (14, "trusted_publishers", OlderReleases::MayRun),
            (15, "lab_layout", OlderReleases::MayRun),
            (16, "lab_devices", OlderReleases::MayRun),
            (17, "lab_sleds", OlderReleases::MayRun),
            (18, "remove_host_group_all", OlderReleases::MayRun),
            (19, "tx_ledger", OlderReleases::MayRun),
            (20, "ssh_certs", OlderReleases::MayRun),
            (21, "ssh_tunnels", OlderReleases::MayRun),
            (22, "instances", OlderReleases::MayRun),
        ]
    );
}

/// Proves: the lint refuses each statement that takes something away or ends the transaction,
/// by the exact text that says which, and accepts the statements that only add; words in
/// comments and quoted text (a quoted name that is not the statement's first word among it) are
/// not taken for its keywords, and a statement inside a dollar-quoted body is read.
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

/// Proves: a `DELETE` is accepted only in a migration with a line that starts `-- deletes-rows:`
/// (a reason follows); with that line every other statement that takes something away is still
/// refused, and a line that only mentions the words, or is not at the line's start, does not
/// count.
///
/// Catches: a migration that deletes rows without saying so, and a deletes-rows line that lets a
/// DROP or a TRUNCATE through.
#[test]
fn a_delete_needs_the_deletes_rows_line_and_it_lets_nothing_else_through() {
    let said = "-- deletes-rows: the group all\n";
    assert_eq!(lint(&format!("{said}DELETE FROM t WHERE a = 1;")), None);
    assert_eq!(
        lint(&format!("{said}DROP TABLE t;")),
        Some("a DROP"),
        "a DROP stays refused"
    );
    assert_eq!(
        lint(&format!("{said}TRUNCATE t;")),
        Some("a TRUNCATE"),
        "a TRUNCATE stays refused"
    );
    assert_eq!(lint("DELETE FROM t;"), Some("a DELETE"));
    assert_eq!(
        lint("-- no deletes-rows: here\nDELETE FROM t;"),
        Some("a DELETE")
    );
    assert_eq!(
        lint(" -- deletes-rows: indented\nDELETE FROM t;"),
        Some("a DELETE")
    );
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

/// A migration that ends with `sql`, under a `may-run` header.
fn hot_refused(sql: &str) -> Option<(&'static str, String)> {
    hot(&format!("-- older-releases: may-run\n{sql}"))
}

/// The negative fixtures of the hot-table rule: each statement, on a table of [`HOT_TABLES`] that
/// the migration did not create, with what it is and the table, as the lint names them.
const HOT_FIXTURES: [(&str, &str, &str); 23] = [
    (
        "ALTER TABLE fleet_orders ADD COLUMN c uuid DEFAULT gen_random_uuid();",
        "a column with more than a type, NOT NULL and a constant default added to",
        "fleet_orders",
    ),
    (
        "ALTER TABLE hostkeeper.fleet_hosts ADD COLUMN c bigint DEFAULT now();",
        "a column with more than a type, NOT NULL and a constant default added to",
        "fleet_hosts",
    ),
    (
        "ALTER TABLE \"hostkeeper\".\"state_versions\" ADD COLUMN c bigserial;",
        "a column with more than a type, NOT NULL and a constant default added to",
        "state_versions",
    ),
    (
        "alter table only Rollout_Gate add column c int generated always as identity;",
        "a column with more than a type, NOT NULL and a constant default added to",
        "rollout_gate",
    ),
    (
        "ALTER TABLE fleet_orders ADD COLUMN c timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP;",
        "a column with more than a type, NOT NULL and a constant default added to",
        "fleet_orders",
    ),
    (
        "ALTER TABLE fleet_orders ADD COLUMN c int UNIQUE;",
        "a column with more than a type, NOT NULL and a constant default added to",
        "fleet_orders",
    ),
    (
        "ALTER TABLE fleet_orders ADD COLUMN c int CHECK (c > 0);",
        "a column with more than a type, NOT NULL and a constant default added to",
        "fleet_orders",
    ),
    (
        "ALTER TABLE fleet_orders ADD COLUMN c text REFERENCES fleet_hosts (host_id);",
        "a column with more than a type, NOT NULL and a constant default added to",
        "fleet_orders",
    ),
    (
        "ALTER TABLE fleet_orders ADD COLUMN c int GENERATED ALWAYS AS (1) STORED;",
        "a column with more than a type, NOT NULL and a constant default added to",
        "fleet_orders",
    ),
    (
        "ALTER TABLE fleet_orders ALTER COLUMN c SET NOT NULL;",
        "a change other than ADD COLUMN of",
        "fleet_orders",
    ),
    (
        "ALTER TABLE fleet_hosts SET LOGGED;",
        "a change other than ADD COLUMN of",
        "fleet_hosts",
    ),
    (
        "ALTER TABLE IF EXISTS fleet_hosts SET (fillfactor = 50);",
        "a change other than ADD COLUMN of",
        "fleet_hosts",
    ),
    (
        "ALTER TABLE fleet_hosts ADD CONSTRAINT k UNIQUE (host_id);",
        "a change other than ADD COLUMN of",
        "fleet_hosts",
    ),
    (
        "ALTER TABLE fleet_hosts ADD PRIMARY KEY (host_id);",
        "a change other than ADD COLUMN of",
        "fleet_hosts",
    ),
    (
        "ALTER TABLE fleet_hosts ADD COLUMN a int, ALTER COLUMN b SET DEFAULT 1;",
        "a change other than ADD COLUMN of",
        "fleet_hosts",
    ),
    (
        "CREATE INDEX ON fleet_hosts (seen_unix);",
        "an index build on",
        "fleet_hosts",
    ),
    (
        "create unique index if not exists i on only Fleet_Orders using btree (host_id);",
        "an index build on",
        "fleet_orders",
    ),
    (
        "CLUSTER fleet_orders USING fleet_orders_host;",
        "a rewrite of",
        "fleet_orders",
    ),
    (
        "REINDEX TABLE state_versions;",
        "a rewrite of",
        "state_versions",
    ),
    ("VACUUM FULL rollout_gate;", "a rewrite of", "rollout_gate"),
    (
        "ALTER /* a note */ TABLE fleet_orders ADD COLUMN c uuid DEFAULT gen_random_uuid();",
        "a column with more than a type, NOT NULL and a constant default added to",
        "fleet_orders",
    ),
    (
        "CREATE /* outer /* nested; */ still a comment */ INDEX ON fleet_hosts (seen_unix);",
        "an index build on",
        "fleet_hosts",
    ),
    (
        "DO $$ BEGIN ALTER TABLE fleet_orders ALTER COLUMN c TYPE bigint; END $$;",
        "a change other than ADD COLUMN of",
        "fleet_orders",
    ),
];

/// Proves (the negative fixtures of the migration lint's hot-table rule): on a table of
/// [`HOT_TABLES`] that the migration did not create, the lint refuses a column added with a default
/// that is not a constant (a function, a value function such as `CURRENT_TIMESTAMP`), a serial or
/// generated column, a column with a constraint, any other `ALTER TABLE` action (one among several
/// is enough), an index build and a rewrite (`CLUSTER`, `REINDEX`, `VACUUM FULL`), whatever the
/// spelling of the table's name (its schema before it, quoted, `ONLY`, `IF EXISTS`, any case), with
/// a block comment (nested) between its words, or inside a dollar-quoted `DO` body; each
/// by what it is and the table, and a shipped set with one of them after it is refused at start
/// with the text that names the migration and the table.
///
/// Catches: a migration that rewrites or indexes a table every heartbeat uses while a running
/// coordinator holds it, so its `ACCESS EXCLUSIVE` wait stalls that coordinator's heartbeats; and a
/// lint a quoted or schema-qualified name slips past.
#[test]
fn migration_lint_refuses_a_rewrite_of_a_hot_table() {
    for (sql, what, table) in HOT_FIXTURES {
        assert_eq!(hot_refused(sql), Some((what, table.to_owned())), "{sql}");
    }
    let (sql, _, _) = HOT_FIXTURES[0];
    let fixture: &'static str =
        Box::leak(format!("-- older-releases: may-run\n{sql}").into_boxed_str());
    let mut set: Vec<(i32, &'static str, &'static str)> = SHIPPED.to_vec();
    let next = i32::try_from(set.len()).expect("a few migrations") + 1;
    set.push((next, "hot_fixture", fixture));
    let refused = checked(&set).expect_err("the fixture is refused at start");
    assert_eq!(
        refused.to_string(),
        format!(
            "migration {next} (hot_fixture): a column with more than a type, NOT NULL and a \
             constant default added to the table fleet_orders is refused; a migration on a table \
             a running coordinator uses only adds a column with no default or a constant one"
        )
    );
}

/// Proves: the hot-table rule takes a column added to a hot table with no default or a constant
/// one (a number, a string, a boolean, an empty array, `NOT NULL`, a collation, `IF NOT EXISTS`,
/// several such columns in one statement), anything on a hot table the same migration creates (its
/// index, its constraint), a new table that references a hot one, every statement on a table that
/// is not hot (a name that only starts like one among them), and the words of comments and quoted
/// text; and every migration this build ships passes it.
///
/// Catches: a rule so wide that the additive migrations it is meant to let through (the next
/// columns of the fleet's tables, and the migration that creates them) are refused.
#[test]
fn the_hot_table_rule_takes_a_constant_column_and_what_the_migration_creates() {
    for sql in [
        "ALTER TABLE fleet_orders ADD COLUMN c int;",
        "ALTER TABLE fleet_orders ADD COLUMN IF NOT EXISTS c bigint NOT NULL DEFAULT 0;",
        "ALTER TABLE fleet_hosts ADD c text DEFAULT 'x' COLLATE \"C\", ADD COLUMN d boolean DEFAULT false;",
        "ALTER TABLE fleet_hosts ADD COLUMN e numeric(10, 2) DEFAULT -1.5;",
        "ALTER TABLE hostkeeper.state_versions ADD COLUMN f text[] NOT NULL DEFAULT '{}';",
        "CREATE TABLE fleet_orders (host_id text); \
         CREATE UNIQUE INDEX fleet_orders_host ON fleet_orders (host_id); \
         ALTER TABLE fleet_orders ADD CONSTRAINT k CHECK (host_id <> '');",
        "CREATE UNLOGGED TABLE IF NOT EXISTS hostkeeper.rollout_gate (id int); \
         CREATE INDEX ON rollout_gate (id);",
        "CREATE TABLE fleet_tries (host_id text REFERENCES fleet_hosts (host_id));",
        "CREATE INDEX audit_at ON audit (at_unix); ALTER TABLE audit ALTER COLUMN by_name SET NOT NULL;",
        "ALTER TABLE fleet_orders_old SET LOGGED; CREATE INDEX ON fleet_hosts_archive (a);",
        "-- ALTER TABLE fleet_orders SET LOGGED\nSELECT 'CREATE INDEX ON fleet_orders';",
        "/* ALTER TABLE fleet_orders SET LOGGED; /* CLUSTER fleet_hosts; */ */ SELECT 1;",
        "DO $$ BEGIN CREATE TABLE fleet_orders (a int); CREATE INDEX ON fleet_orders (a); END $$;",
        "",
    ] {
        assert_eq!(hot_refused(sql), None, "{sql}");
    }
    for (_, name, sql) in SHIPPED {
        assert_eq!(hot(sql), None, "{name}");
    }
}
