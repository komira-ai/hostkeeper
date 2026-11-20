//! The migrations (`docs/design/coordinator-storage.md`, section 6): numbered SQL files under
//! `migrations/`, embedded in the binary, each run once, at start, after the coordinator holds the
//! lock and before it loads anything, in its own transaction with its `schema_version` row, so a
//! migration that fails is rolled back whole and the start is refused, naming it.
//!
//! The first line of each file says what an older release may do on a database that holds it:
//! `-- older-releases: refuse` or `-- older-releases: may-run`. A database that holds a migration
//! this binary does not know is refused when any such migration says `refuse`, and taken when
//! every one says `may-run`.
//!
//! A migration only adds: [`lint`] refuses `DROP`, `RENAME`, a column type change, `TRUNCATE`,
//! `DELETE`, a `NOT NULL` column added without a default, and statements that end the
//! transaction. A migration that removes rows (the data of a feature the owner removed) says so
//! on a line of its own that starts `-- deletes-rows:` and gives the reason; only then is its
//! `DELETE` accepted, and everything else is refused as before. On a table every heartbeat of a
//! running coordinator uses ([`HOT_TABLES`]) that the migration did not create, [`hot`] takes
//! nothing but a column added with no default or a constant one: no rewrite, no index build
//! (`docs/design/coordinator-multi-instance.md`, section 4, "Migrations"). The lint reads text; it
//! is a review guard for what ships, not a security boundary.
//!
//! Each migration's transaction runs with its own `lock_timeout` ([`Retry`]): a statement that
//! cannot have its lock in that time, behind a running coordinator's transaction, rolls the whole
//! migration back instead of making that coordinator's statements queue behind it, and the runner
//! tries again after a pause, a bounded number of times, then refuses the start
//! ([`ApplyError::Busy`]).

use std::fmt;
use std::num::NonZeroU32;
use std::time::Duration;

use postgres::error::SqlState;

use super::session::{SCHEMA, Session, describe};

/// What an older release may do on a database that holds a migration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OlderReleases {
    /// Refuse to start: the migration adds what an older release would act wrongly without.
    Refuse,
    /// Run: the migration only adds what an older release can ignore.
    MayRun,
}

impl OlderReleases {
    /// The spelling of the header line and of `schema_version.older_releases`.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            OlderReleases::Refuse => "refuse",
            OlderReleases::MayRun => "may-run",
        }
    }
}

/// One migration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Migration {
    /// Its number, from 1, one more than the one before.
    pub(crate) version: i32,
    /// Its name: the file's name after the number.
    pub(crate) name: &'static str,
    /// What an older release may do.
    pub(crate) older: OlderReleases,
    /// The statements.
    pub(crate) sql: &'static str,
}

/// The migrations this binary ships, in order.
pub(crate) const SHIPPED: [(i32, &str, &str); 29] = [
    (1, "state", include_str!("migrations/0001_state.sql")),
    (
        2,
        "first_serving",
        include_str!("migrations/0002_first_serving.sql"),
    ),
    (
        3,
        "generations",
        include_str!("migrations/0003_generations.sql"),
    ),
    (4, "releases", include_str!("migrations/0004_releases.sql")),
    (
        5,
        "mdm_devices",
        include_str!("migrations/0005_mdm_devices.sql"),
    ),
    (
        6,
        "host_running",
        include_str!("migrations/0006_host_running.sql"),
    ),
    (
        7,
        "repositories",
        include_str!("migrations/0007_repositories.sql"),
    ),
    (
        8,
        "host_groups",
        include_str!("migrations/0008_host_groups.sql"),
    ),
    (
        9,
        "service_repository",
        include_str!("migrations/0009_service_repository.sql"),
    ),
    (10, "accounts", include_str!("migrations/0010_accounts.sql")),
    (11, "mdm", include_str!("migrations/0011_mdm.sql")),
    (
        12,
        "mdm_setup",
        include_str!("migrations/0012_mdm_setup.sql"),
    ),
    (
        13,
        "host_group_all",
        include_str!("migrations/0013_host_group_all.sql"),
    ),
    (
        14,
        "trusted_publishers",
        include_str!("migrations/0014_trusted_publishers.sql"),
    ),
    (
        15,
        "lab_layout",
        include_str!("migrations/0015_lab_layout.sql"),
    ),
    (
        16,
        "lab_devices",
        include_str!("migrations/0016_lab_devices.sql"),
    ),
    (
        17,
        "lab_sleds",
        include_str!("migrations/0017_lab_sleds.sql"),
    ),
    (
        18,
        "remove_host_group_all",
        include_str!("migrations/0018_remove_host_group_all.sql"),
    ),
    (
        19,
        "tx_ledger",
        include_str!("migrations/0019_tx_ledger.sql"),
    ),
    (
        20,
        "ssh_certs",
        include_str!("migrations/0020_ssh_certs.sql"),
    ),
    (
        21,
        "ssh_tunnels",
        include_str!("migrations/0021_ssh_tunnels.sql"),
    ),
    (
        22,
        "instances",
        include_str!("migrations/0022_instances.sql"),
    ),
    (
        23,
        "state_versions",
        include_str!("migrations/0023_state_versions.sql"),
    ),
    (24, "fleet", include_str!("migrations/0024_fleet.sql")),
    (
        25,
        "sessions_and_jobs",
        include_str!("migrations/0025_sessions_and_jobs.sql"),
    ),
    (
        26,
        "decisions",
        include_str!("migrations/0026_decisions.sql"),
    ),
    (
        27,
        "audit_api_token",
        include_str!("migrations/0027_audit_api_token.sql"),
    ),
    (
        28,
        "xcode_releases",
        include_str!("migrations/0028_xcode_releases.sql"),
    ),
    (
        29,
        "maintenance_runs",
        include_str!("migrations/0029_maintenance_runs.sql"),
    ),
];

/// Why a migration, or the set of them, is refused before it runs.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum MigrationError {
    /// The first line names no rule for older releases.
    #[error(
        "migration {version} ({name}): its first line is not `-- older-releases: refuse` or `-- older-releases: may-run`"
    )]
    NoRule { version: i32, name: &'static str },
    /// A statement takes something away or ends the transaction.
    #[error("migration {version} ({name}): {what} is refused; a migration only adds")]
    Refused {
        version: i32,
        name: &'static str,
        what: &'static str,
    },
    /// The numbers do not run 1, 2, 3 and on.
    #[error("migration {found} follows {before}: the numbers run 1, 2, 3 and on")]
    Numbering { before: i32, found: i32 },
    /// A statement rewrites, or builds an index on, a table every heartbeat uses ([`HOT_TABLES`])
    /// that the migration did not create.
    #[error(
        "migration {version} ({name}): {what} the table {table} is refused; a migration on a table a running coordinator uses only adds a column with no default or a constant one"
    )]
    Hot {
        version: i32,
        name: &'static str,
        what: &'static str,
        table: String,
    },
}

/// The migration `version`, `name` and `sql`, its rule read and its statements linted.
///
/// # Errors
/// [`MigrationError`]: no rule line, or a statement the lint refuses.
pub(crate) fn migration(
    version: i32,
    name: &'static str,
    sql: &'static str,
) -> Result<Migration, MigrationError> {
    let older = match sql.lines().next().map(str::trim) {
        Some("-- older-releases: refuse") => OlderReleases::Refuse,
        Some("-- older-releases: may-run") => OlderReleases::MayRun,
        _ => return Err(MigrationError::NoRule { version, name }),
    };
    if let Some(what) = lint(sql) {
        return Err(MigrationError::Refused {
            version,
            name,
            what,
        });
    }
    if let Some((what, table)) = hot(sql) {
        return Err(MigrationError::Hot {
            version,
            name,
            what,
            table,
        });
    }
    Ok(Migration {
        version,
        name,
        older,
        sql,
    })
}

/// Every migration of `set`, checked, in order.
///
/// # Errors
/// [`MigrationError`]: one is refused, or the numbers are not 1, 2, 3 and on.
pub(crate) fn checked(
    set: &[(i32, &'static str, &'static str)],
) -> Result<Vec<Migration>, MigrationError> {
    let mut out: Vec<Migration> = Vec::new();
    for &(version, name, sql) in set {
        let before = out.last().map_or(0, |m| m.version);
        if version != before + 1 {
            return Err(MigrationError::Numbering {
                before,
                found: version,
            });
        }
        out.push(migration(version, name, sql)?);
    }
    Ok(out)
}

/// The statements of `sql`, each a list of its upper-cased words: comments (`--` and nested
/// `/* */`) and quoted text dropped, a quoted name kept as a word and a name with its schema (`a.b`) as one word, the
/// bodies of dollar-quoted text kept as words of their own statements, so a function body is
/// read too.
fn statements(sql: &str) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut chars = sql.chars().peekable();
    let flush = |word: &mut String, current: &mut Vec<String>| {
        if !word.is_empty() {
            current.push(word.to_uppercase());
            word.clear();
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '-' if chars.peek() == Some(&'-') => {
                flush(&mut word, &mut current);
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            // A block comment, nested as the server nests them, separates words and is not read.
            '/' if chars.peek() == Some(&'*') => {
                flush(&mut word, &mut current);
                chars.next();
                let mut depth = 1_usize;
                while depth > 0 {
                    match (chars.next(), chars.peek()) {
                        (Some('*'), Some('/')) => {
                            chars.next();
                            depth -= 1;
                        }
                        (Some('/'), Some('*')) => {
                            chars.next();
                            depth += 1;
                        }
                        (Some(_), _) => {}
                        (None, _) => break,
                    }
                }
            }
            '\'' => {
                flush(&mut word, &mut current);
                while let Some(q) = chars.next() {
                    if q == c {
                        if chars.peek() == Some(&c) {
                            chars.next();
                        } else {
                            break;
                        }
                    }
                }
            }
            // A quoted name is a word, or the part of one (`"hostkeeper"."fleet_orders"`), so a
            // table is named by it as by its bare name.
            '"' => {
                while let Some(q) = chars.next() {
                    if q == c {
                        if chars.peek() == Some(&c) {
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    word.push(q);
                }
            }
            // A `;`, and a dollar quote's tag, end the statement they follow: so the first
            // statement of a dollar-quoted body is read as a statement of its own, like the
            // ones after its `;`.
            ';' | '$' => {
                flush(&mut word, &mut current);
                out.push(std::mem::take(&mut current));
            }
            // A dot joins a schema's name to its table's: `hostkeeper.fleet_orders` is one word.
            c if c.is_ascii_alphanumeric() || c == '_' || c == '.' => word.push(c),
            '(' | ')' | ',' => {
                flush(&mut word, &mut current);
                current.push(c.to_string());
            }
            // The rest separates words.
            _ => flush(&mut word, &mut current),
        }
    }
    flush(&mut word, &mut current);
    out.push(current);
    out.retain(|statement| !statement.is_empty());
    out
}

/// The start of the line by which a migration says it removes rows ([`lint`]).
const DELETES_ROWS: &str = "-- deletes-rows:";

/// The first statement of `sql` that takes something away or ends the transaction, as what it
/// is; `None` when every statement only adds.
pub(crate) fn lint(sql: &str) -> Option<&'static str> {
    let may_delete = sql.lines().any(|line| line.starts_with(DELETES_ROWS));
    for words in statements(sql) {
        let has = |w: &str| words.iter().any(|x| x == w);
        let first = words.first().map(String::as_str);
        match first {
            Some("COMMIT" | "ROLLBACK" | "SAVEPOINT" | "ABORT" | "RELEASE") => {
                return Some("a statement that ends the transaction");
            }
            Some("DROP") => return Some("a DROP"),
            Some("TRUNCATE") => return Some("a TRUNCATE"),
            Some("DELETE") if !may_delete => return Some("a DELETE"),
            _ => {}
        }
        if first == Some("ALTER") {
            if has("RENAME") {
                return Some("a RENAME");
            }
            if has("DROP") {
                return Some("a DROP");
            }
            if has("TYPE") {
                return Some("a column type change");
            }
            let not_null = words.windows(2).any(|w| w[0] == "NOT" && w[1] == "NULL");
            if has("ADD") && not_null && !has("DEFAULT") {
                return Some("a NOT NULL column added without a default");
            }
        }
    }
    None
}

/// The tables a running coordinator touches in every heartbeat or decision
/// (`docs/design/coordinator-multi-instance.md`, section 4, "Migrations"). A migration's DDL that
/// waits for one of them makes every later statement on it wait too, so on one of them that the
/// migration did not create [`hot`] takes nothing but a column added with no default or a
/// constant one: a change of the catalog, held for moments.
pub(crate) const HOT_TABLES: [&str; 4] = [
    "fleet_orders",
    "fleet_hosts",
    "state_versions",
    "rollout_gate",
];

/// The words that may follow a column's type when it is added to a hot table: the column's
/// default (a constant), whether it may be null, and its collation.
const ADDED_COLUMN_WORDS: [&str; 4] = ["DEFAULT", "NOT", "NULL", "COLLATE"];

/// The words of SQL's value functions, which are not constants though they take no parentheses.
const NOT_CONSTANT: [&str; 12] = [
    "CURRENT_DATE",
    "CURRENT_TIME",
    "CURRENT_TIMESTAMP",
    "LOCALTIME",
    "LOCALTIMESTAMP",
    "CURRENT_USER",
    "CURRENT_ROLE",
    "SESSION_USER",
    "SYSTEM_USER",
    "USER",
    "CURRENT_SCHEMA",
    "CURRENT_CATALOG",
];

/// The table of `word` when it is one of [`HOT_TABLES`], its schema's name dropped.
fn hot_table(word: &str) -> Option<&'static str> {
    let bare = word.rsplit('.').next().unwrap_or(word);
    HOT_TABLES
        .into_iter()
        .find(|table| table.eq_ignore_ascii_case(bare))
}

/// The index of the word that names the table in `words` from `at` on: `IF EXISTS`,
/// `IF NOT EXISTS` and `ONLY` skipped.
fn named_at(words: &[String], mut at: usize) -> usize {
    while let Some(word) = words.get(at) {
        match word.as_str() {
            "IF" | "NOT" | "EXISTS" | "ONLY" => at += 1,
            _ => break,
        }
    }
    at
}

/// The parts of `words` between the commas that are not inside parentheses.
fn split_actions(words: &[String]) -> Vec<&[String]> {
    let mut out = Vec::new();
    let (mut depth, mut start) = (0_usize, 0_usize);
    for (i, word) in words.iter().enumerate() {
        match word.as_str() {
            "(" => depth += 1,
            ")" => depth = depth.saturating_sub(1),
            "," if depth == 0 => {
                out.push(&words[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&words[start..]);
    out
}

/// What is refused of the action `action` of an `ALTER TABLE` of a hot table: `None` for a
/// column added with no default or a constant one, and nothing else after its type but
/// `NOT NULL`, `NULL` or a collation.
fn added_column(action: &[String]) -> Option<&'static str> {
    let other = Some("a change other than ADD COLUMN of");
    if action.first().map(String::as_str) != Some("ADD") {
        return other;
    }
    let mut at = 1;
    match action.get(at).map(String::as_str) {
        Some("COLUMN") => at += 1,
        Some("CONSTRAINT" | "PRIMARY" | "UNIQUE" | "CHECK" | "FOREIGN" | "EXCLUDE") | None => {
            return other;
        }
        Some(_) => {}
    }
    // `IF NOT EXISTS`, the column's name, then its type: the words up to the first that says
    // more about the column.
    let rest = &action[named_at(action, at) + 1..];
    let after_type = rest
        .iter()
        .position(|w| ADDED_COLUMN_WORDS.contains(&w.as_str()) || is_constraint(w))
        .unwrap_or(rest.len());
    let (column_type, more) = rest.split_at(after_type);
    let serial = column_type.iter().any(|w| w.contains("SERIAL"));
    let not_constant = more
        .iter()
        .any(|w| is_constraint(w) || w == "(" || NOT_CONSTANT.contains(&w.as_str()));
    (serial || not_constant)
        .then_some("a column with more than a type, NOT NULL and a constant default added to")
}

/// Whether `word` starts what a column added to a hot table may not have: a constraint, which is
/// checked over every row or builds an index, or a value computed for every row.
fn is_constraint(word: &str) -> bool {
    matches!(
        word,
        "CHECK" | "UNIQUE" | "PRIMARY" | "REFERENCES" | "CONSTRAINT" | "GENERATED"
    )
}

/// The first statement of `sql` that rewrites one of [`HOT_TABLES`], or builds an index on it,
/// when `sql` did not create it: what it is, and the table. `None` when there is none.
pub(crate) fn hot(sql: &str) -> Option<(&'static str, String)> {
    let words = statements(sql);
    // Every statement from each of its words on: the DDL of a dollar-quoted body (`DO $$ BEGIN
    // ALTER TABLE ...`) is read where it starts, not only as a statement's first words.
    let starts: Vec<&[String]> = words
        .iter()
        .flat_map(|w| (0..w.len()).map(move |at| &w[at..]))
        .collect();
    let created: Vec<&str> = starts
        .iter()
        .filter(|w| w.first().map(String::as_str) == Some("CREATE"))
        .filter_map(|w| {
            let table = w.iter().position(|x| x == "TABLE")?;
            w.get(named_at(w, table + 1)).and_then(|t| hot_table(t))
        })
        .collect();
    let existing = |word: Option<&String>| {
        word.and_then(|t| hot_table(t))
            .filter(|table| !created.contains(table))
    };
    for statement in starts {
        let first = statement.first().map(String::as_str);
        let second = statement.get(1).map(String::as_str);
        match (first, second) {
            (Some("ALTER"), Some("TABLE")) => {
                let at = named_at(statement, 2);
                if let Some(table) = existing(statement.get(at)) {
                    let refused = split_actions(&statement[at + 1..])
                        .into_iter()
                        .find_map(added_column);
                    if let Some(what) = refused {
                        return Some((what, table.to_owned()));
                    }
                }
            }
            (Some("CREATE"), _) if statement.iter().any(|w| w == "INDEX") => {
                let on = statement.iter().position(|w| w == "ON");
                let table = on.and_then(|on| existing(statement.get(named_at(statement, on + 1))));
                if let Some(table) = table {
                    return Some(("an index build on", table.to_owned()));
                }
            }
            (Some("CLUSTER" | "REINDEX" | "VACUUM"), _) => {
                if let Some(table) = statement.iter().find_map(|w| existing(Some(w))) {
                    return Some(("a rewrite of", table.to_owned()));
                }
            }
            _ => {}
        }
    }
    None
}

/// A row of `schema_version`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Applied {
    pub(crate) version: i32,
    pub(crate) name: String,
    pub(crate) older: String,
}

/// Why the migrations could not be brought up to date.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ApplyError {
    /// A migration this binary ships is refused before it runs.
    #[error(transparent)]
    Shipped(#[from] MigrationError),
    /// The database holds a migration this binary does not know, which an older release must
    /// not run on.
    #[error("the database's schema is newer than this coordinator (migration {version}, {name})")]
    Newer { version: i32, name: String },
    /// A migration failed, and was rolled back whole.
    #[error("migration {version} ({name}) failed and was rolled back: {why}")]
    Failed {
        version: i32,
        name: &'static str,
        why: String,
    },
    /// The bookkeeping of the migrations could not be read or written.
    #[error("the schema's migrations could not be read: {0}")]
    Database(String),
    /// A migration could not have the locks it needs within its lock timeout at any of its
    /// attempts, each rolled back whole.
    #[error(
        "migration {version} ({name}) could not have its locks within its lock timeout in {attempts} attempts, each rolled back: {why}"
    )]
    Busy {
        version: i32,
        name: &'static str,
        attempts: NonZeroU32,
        why: String,
    },
}

/// How a migration waits for the locks of its statements, and how often it is tried again when it
/// cannot have them (`--migrate-ddl-lock-timeout-ms`, `--migrate-retry-secs`,
/// `--migrate-attempts`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Retry {
    /// The `lock_timeout` of each migration's transaction: a statement that waits longer for a
    /// lock rolls the whole migration back, so a running coordinator's statements queued behind
    /// it wait no longer than this.
    pub(crate) lock_timeout: Duration,
    /// The pause between an attempt that was rolled back so and the next one.
    pub(crate) pause: Duration,
    /// How many attempts are made in all before the start is refused.
    pub(crate) attempts: NonZeroU32,
}

/// The defaults of [`Retry`]: a lock waited for 3 seconds, six attempts 5 seconds apart.
pub(crate) const RETRY: Retry = Retry {
    lock_timeout: Duration::from_millis(3000),
    pause: Duration::from_secs(5),
    attempts: NonZeroU32::MIN.saturating_add(5),
};

/// The rows of `schema_version`, in order: none on a fresh database.
fn applied(session: &mut Session) -> Result<Vec<Applied>, postgres::Error> {
    session.batch(&format!(
        "CREATE SCHEMA IF NOT EXISTS {SCHEMA}; \
         CREATE TABLE IF NOT EXISTS schema_version (version integer PRIMARY KEY, \
         name text NOT NULL, \
         older_releases text NOT NULL CHECK (older_releases IN ('refuse', 'may-run')), \
         applied_unix bigint NOT NULL DEFAULT extract(epoch FROM now()))"
    ))?;
    Ok(session
        .query(
            "SELECT version, name, older_releases FROM schema_version ORDER BY version",
            &[],
        )?
        .iter()
        .map(|row| Applied {
            version: row.get(0),
            name: row.get(1),
            older: row.get(2),
        })
        .collect())
}

/// Refuses the migrations `rows` a database holds when one of them is newer than `set` and says
/// `refuse`.
fn newer(rows: &[Applied], set: &[Migration]) -> Result<(), ApplyError> {
    let newest = set.last().map_or(0, |m| m.version);
    match rows
        .iter()
        .find(|row| row.version > newest && row.older != OlderReleases::MayRun.as_str())
    {
        Some(row) => Err(ApplyError::Newer {
            version: row.version,
            name: row.name.clone(),
        }),
        None => Ok(()),
    }
}

/// The key of the advisory lock that serializes the runners of several coordinators that start
/// at once: not [`super::writer::LOCK_KEY`], which every coordinator holds together. It is taken
/// for the length of [`apply`] only, so it is a mutex around a few moments of start, and elects
/// nobody.
pub(crate) const MIGRATE_KEY: i64 = 0x686b_6d69_6772;

/// Brings the database of `session`, which holds the coordinator's lock, up to `set` with the
/// lock timeout and the attempts of [`RETRY`], pausing on this thread between attempts.
///
/// # Errors
/// As [`apply_with`].
pub(crate) fn apply(session: &mut Session, set: &[Migration]) -> Result<Vec<i32>, ApplyError> {
    apply_with(session, set, RETRY, &mut std::thread::sleep)
}

/// Brings the database of `session`, which holds the coordinator's lock, up to `set`: refuses a
/// database that holds a migration `set` does not know and that says `refuse`, then runs each
/// migration it lacks, in order, each in its own transaction with its `schema_version` row and
/// with `retry`'s lock timeout. Answers the numbers it ran. The runner holds [`MIGRATE_KEY`] while
/// it works, so coordinators that start together run each migration once: the one that waits
/// finds it applied and runs nothing. That wait is bounded by the session's lock timeout.
///
/// A migration that could not have a lock within `retry.lock_timeout` is rolled back whole; the
/// runner lets go of [`MIGRATE_KEY`], calls `pause` with `retry.pause`, and tries again from the
/// migration it stopped at, up to `retry.attempts` attempts in all.
///
/// # Errors
/// [`ApplyError`]: a newer schema, a migration that failed (rolled back), a migration that could
/// not have its locks at any attempt ([`ApplyError::Busy`]), or the bookkeeping could not be read,
/// or the runners' lock could not be had.
pub(crate) fn apply_with(
    session: &mut Session,
    set: &[Migration],
    retry: Retry,
    pause: &mut dyn FnMut(Duration),
) -> Result<Vec<i32>, ApplyError> {
    let mut ran = Vec::new();
    let mut attempt = 1;
    loop {
        session
            .execute("SELECT pg_advisory_lock($1)", &[&MIGRATE_KEY])
            .map_err(|e| ApplyError::Database(describe(&e)))?;
        let held = apply_held(session, set, retry.lock_timeout, &mut ran);
        // A session that cannot unlock is broken, and the server frees the lock as it ends it.
        let _ = session.execute("SELECT pg_advisory_unlock($1)", &[&MIGRATE_KEY]);
        match held {
            Ok(()) => return Ok(ran),
            Err(Stop::Busy { version, name, why }) if attempt < retry.attempts.get() => {
                tracing::warn!(
                    version,
                    name,
                    attempt,
                    %why,
                    "a migration could not have its locks within its lock timeout and was rolled back: it is tried again"
                );
                pause(retry.pause);
                attempt += 1;
            }
            Err(Stop::Busy { version, name, why }) => {
                return Err(ApplyError::Busy {
                    version,
                    name,
                    attempts: retry.attempts,
                    why,
                });
            }
            Err(Stop::Failed(e)) => return Err(e),
        }
    }
}

/// Why [`apply_held`] stopped.
enum Stop {
    /// The migration could not have a lock within its lock timeout: it was rolled back.
    Busy {
        version: i32,
        name: &'static str,
        why: String,
    },
    /// Anything else.
    Failed(ApplyError),
}

/// [`apply_with`]'s one attempt while the runners' lock is held: the numbers it ran are added to
/// `ran`.
fn apply_held(
    session: &mut Session,
    set: &[Migration],
    lock_timeout: Duration,
    ran: &mut Vec<i32>,
) -> Result<(), Stop> {
    let database = |e: postgres::Error| Stop::Failed(ApplyError::Database(describe(&e)));
    let rows = applied(session).map_err(database)?;
    newer(&rows, set).map_err(Stop::Failed)?;
    for migration in set {
        if rows.iter().any(|row| row.version == migration.version) {
            continue;
        }
        let run = session.batch("BEGIN").and_then(|()| {
            session.batch(&format!(
                "SET LOCAL lock_timeout = {}",
                lock_timeout.as_millis()
            ))?;
            session.batch(migration.sql)?;
            session.execute(
                "INSERT INTO schema_version (version, name, older_releases) VALUES ($1, $2, $3)",
                &[
                    &migration.version,
                    &migration.name,
                    &migration.older.as_str(),
                ],
            )?;
            session.batch("COMMIT")
        });
        if let Err(e) = run {
            // A failed transaction is rolled back; a lost session took it with it.
            let _ = session.batch("ROLLBACK");
            let (version, name, why) = (migration.version, migration.name, describe(&e));
            if e.code() == Some(&SqlState::LOCK_NOT_AVAILABLE) {
                return Err(Stop::Busy { version, name, why });
            }
            return Err(Stop::Failed(ApplyError::Failed { version, name, why }));
        }
        ran.push(migration.version);
    }
    Ok(())
}

impl fmt::Display for OlderReleases {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
#[path = "migrate_tests.rs"]
mod tests;
