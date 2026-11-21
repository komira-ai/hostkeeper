//! Compatibility of migration `0025` (sessions, login failures, periodic jobs and alerts as rows;
//! `docs/design/coordinator-multi-instance.md`, sections 8 and 9) with the release before, both
//! ways. Nothing here reads a clock.

use super::{TestDatabase, with_session};
use crate::store::postgres::admin_rows::PgAdmin;
use crate::store::postgres::alert_rows::PgAlerts;
use crate::store::postgres::migrate;
use crate::store::postgres::writer::{LOCK_KEY, try_lock};

/// The tables `0025` adds.
const TABLES: [&str; 5] = [
    "sessions",
    "login_failures",
    "login_gate",
    "periodic_jobs",
    "alerts",
];

fn one(text: &str) -> Vec<Vec<Option<String>>> {
    vec![vec![Some(text.to_owned())]]
}

/// The columns of every table of the database but those of `0025`, and but what `0026` adds
/// (`fleet_tries` and two columns of `desired` and `group_desired`) and `0027` (a column of
/// `audit`), `0028` (`xcode_releases`) and `0029` (`maintenance_runs` and `maintenance_hosts`;
/// each has its own test), as text, sorted.
fn old_columns(db: &TestDatabase) -> Vec<Vec<Option<String>>> {
    db.rows(&format!(
        "SELECT table_name, column_name, data_type, is_nullable, column_default \
         FROM information_schema.columns WHERE table_schema = current_schema() \
         AND table_name NOT IN ({}) \
         AND table_name NOT IN ('fleet_tries', 'xcode_releases', 'maintenance_runs', \
         'maintenance_hosts') \
         AND column_name NOT IN ('set_seq', 'set_for', 'api_token') \
         ORDER BY 1, 2",
        TABLES.map(|name| format!("'{name}'")).join(", ")
    ))
}

/// Proves (B8, both ways, for `0025`): the release before's runner (migrations `0001` to `0024`)
/// accepts a database this release made its session, job and alert tables in, runs nothing, and
/// leaves them, and takes the coordinator's lock once this release stopped; this release, started
/// on a database the release before migrated, runs `0025` (and `0026` to `0029` after it) and
/// makes the tables, with `login_gate` holding its two rows, while the lock refuses the release
/// before.
///
/// Catches: a `0025` marked `refuse`, which would turn a rollback into a restore, a release that
/// does not bring the release before's database up to its tables, and a gate row not made.
#[test]
fn the_release_before_runs_on_a_database_with_0025_and_this_one_on_its() {
    let db = TestDatabase::new();
    let (store, _) = db.open("this release").expect("this release starts");
    store.stop();
    let (ran, locked) = with_session(&db, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..24]).expect("the release before's set");
        let ran = migrate::apply(session, &before).expect("the release before is accepted");
        (ran, try_lock(session).expect("a try"))
    });
    assert_eq!(ran, Vec::<i32>::new());
    assert!(
        locked,
        "the release before has the lock once this release stopped"
    );
    assert_eq!(
        db.rows("SELECT name FROM login_gate ORDER BY name"),
        vec![
            vec![Some("login_failures".to_owned())],
            vec![Some("sessions".to_owned())]
        ]
    );

    let old = TestDatabase::new();
    with_session(&old, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..24]).expect("the release before's set");
        migrate::apply(session, &before).expect("the release before migrates");
    });
    assert_eq!(
        old.rows(&format!(
            "SELECT count(*) FROM pg_class WHERE relkind = 'r' AND relname IN ({})",
            TABLES.map(|name| format!("'{name}'")).join(", ")
        )),
        one("0"),
        "the release before has none of the tables"
    );
    let (store, _) = old.open("this release").expect("this release starts on it");
    assert_eq!(
        old.rows("SELECT max(version) FROM schema_version"),
        one("30")
    );
    assert_eq!(
        old.rows("SELECT version FROM schema_version WHERE version > 24 ORDER BY version"),
        [
            one("25"),
            one("26"),
            one("27"),
            one("28"),
            one("29"),
            one("30")
        ]
        .concat(),
        "0025 ran, and 0026 to 0030 after it"
    );
    for table in TABLES {
        assert_eq!(
            old.rows(&format!(
                "SELECT count(*) FROM pg_class WHERE relkind = 'r' AND relname = '{table}'"
            )),
            one("1"),
            "{table}"
        );
    }
    assert_eq!(
        old.rows("SELECT name FROM login_gate ORDER BY name"),
        vec![
            vec![Some("login_failures".to_owned())],
            vec![Some("sessions".to_owned())]
        ]
    );
    let mut client = old.client();
    let taken: bool = client
        .query_one("SELECT pg_try_advisory_lock($1)", &[&LOCK_KEY])
        .expect("a try")
        .get(0);
    assert!(
        !taken,
        "the release before is refused while this release runs"
    );
    store.stop();
}

/// Proves (B8, for `0025`): the migration only adds tables that no earlier table references and
/// that reference none: every column of every other table is as the release before made it, no
/// foreign key joins the new tables to the others, and a database holding a session, a failure
/// count, a job row and an alert row is accepted by the release before's set with nothing run and
/// every other table as it was.
///
/// Catches: a `0025` that alters a table the release before reads, a foreign key that makes the
/// release before's deletes fail on a new row, and a new table the release before refuses.
#[test]
fn the_tables_of_0025_are_new_and_the_release_before_reads_none_of_them() {
    let old = TestDatabase::new();
    with_session(&old, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..24]).expect("the release before's set");
        migrate::apply(session, &before).expect("the release before migrates");
    });
    let columns_before = old_columns(&old);
    assert!(
        columns_before.len() > 50,
        "the release before's tables are read"
    );

    let db = TestDatabase::new();
    let (store, _) = db.open("this release").expect("this release starts");
    store.stop();
    assert_eq!(old_columns(&db), columns_before, "no old table changed");
    assert_eq!(
        db.rows(
            "SELECT count(*) FROM pg_constraint WHERE contype = 'f' AND \
             (conrelid::regclass::text IN ('sessions', 'login_failures', 'login_gate', \
              'periodic_jobs', 'alerts') OR confrelid::regclass::text IN ('sessions', \
              'login_failures', 'login_gate', 'periodic_jobs', 'alerts'))"
        ),
        one("0")
    );
    for (_, name, sql) in &migrate::SHIPPED[..24] {
        for table in TABLES {
            assert!(
                !sql.contains(&format!(" {table} ")) && !sql.contains(&format!(" {table}(")),
                "migration {name} names {table}"
            );
        }
    }

    db.run(
        "INSERT INTO sessions (id_sha256, username, csrf, must_change, created_unix, \
         last_used_unix) VALUES ('\\x01', 'ada', 'csrf', false, 10, 10); \
         INSERT INTO login_failures (name, failures, last_failure_unix) VALUES ('ada', 3, 11); \
         INSERT INTO periodic_jobs (name, next_run_unix) VALUES ('request-expiry', 12); \
         INSERT INTO alerts (kind, service, host, version, request, other_host, first_unix) \
         VALUES ('k', 's', 'h', 'v', 'r', 'o', 13)",
    )
    .expect("this release's rows are written");
    let tables_before =
        db.rows("SELECT (SELECT count(*) FROM instances), (SELECT count(*) FROM schema_version)");
    let (ran, locked) = with_session(&db, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..24]).expect("the release before's set");
        let ran = migrate::apply(session, &before).expect("the release before is accepted");
        (ran, try_lock(session).expect("a try"))
    });
    assert_eq!(ran, Vec::<i32>::new());
    assert!(locked);
    assert_eq!(
        db.rows("SELECT (SELECT count(*) FROM instances), (SELECT count(*) FROM schema_version)"),
        tables_before
    );
    assert_eq!(
        db.rows(
            "SELECT (SELECT count(*) FROM sessions), (SELECT count(*) FROM login_failures), \
             (SELECT count(*) FROM periodic_jobs), (SELECT count(*) FROM alerts)"
        ),
        vec![vec![
            Some("1".to_owned()),
            Some("1".to_owned()),
            Some("1".to_owned()),
            Some("1".to_owned())
        ]],
        "the release before leaves this release's rows"
    );
}

/// Proves: the row-backed tables print themselves without their store: `PgAdmin` and `PgAlerts`
/// as their names with `..`, `PgLoginRows` with its session and limiter rules (the touch interval
/// 30, the cap 10, the deadlines and the failure count it was given), and `PgJobs` with its
/// periods.
///
/// Catches: a debug text that leaks the store's connections, and one that hides the rules a
/// failure report needs.
#[test]
fn the_row_tables_print_their_rules_and_not_their_store() {
    let db = TestDatabase::new();
    let (store, _) = db.open("debug").expect("the store opens");
    let login = super::periodic::login_rows_of(&store);
    let (_, clock) = super::periodic::clock(1);
    let jobs = super::periodic::jobs_of(&db, &store, super::periodic::periods(), &clock);
    assert_eq!(
        format!("{:?}", PgAdmin::new(store.clone())),
        "PgAdmin { .. }"
    );
    assert_eq!(
        format!("{:?}", PgAlerts::new(store.clone())),
        "PgAlerts { .. }"
    );
    assert_eq!(
        format!("{login:?}"),
        "PgLoginRows { sessions: SessionRules { idle_secs: 100, max_secs: 1000, max_count: 10, \
         touch_secs: 30 }, limiter: LimiterRules { max_failures: 3, lockout_secs: 60, \
         max_tracked: 10 }, .. }"
    );
    assert_eq!(
        format!("{jobs:?}"),
        "PgJobs { periods: [(\"alerts\", 15), (\"request-expiry\", 15), (\"prune\", 3600)], .. }"
    );
    store.stop();
}
