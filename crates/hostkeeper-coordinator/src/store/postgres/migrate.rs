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
//! transaction. The lint reads text; it is a review guard for what ships, not a security boundary.

use std::fmt;

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
pub(crate) const SHIPPED: [(i32, &str, &str); 10] = [
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
    (10, "mdm", include_str!("migrations/0010_mdm.sql")),
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

/// The statements of `sql`, each a list of its upper-cased words: comments and quoted text
/// dropped, the bodies of dollar-quoted text kept as words of their own statements, so a
/// function body is read too.
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
            '\'' | '"' => {
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
            // A `;`, and a dollar quote's tag, end the statement they follow: so the first
            // statement of a dollar-quoted body is read as a statement of its own, like the
            // ones after its `;`.
            ';' | '$' => {
                flush(&mut word, &mut current);
                out.push(std::mem::take(&mut current));
            }
            c if c.is_ascii_alphanumeric() || c == '_' => word.push(c),
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

/// The first statement of `sql` that takes something away or ends the transaction, as what it
/// is; `None` when every statement only adds.
pub(crate) fn lint(sql: &str) -> Option<&'static str> {
    for words in statements(sql) {
        let has = |w: &str| words.iter().any(|x| x == w);
        let first = words.first().map(String::as_str);
        match first {
            Some("COMMIT" | "ROLLBACK" | "SAVEPOINT" | "ABORT" | "RELEASE") => {
                return Some("a statement that ends the transaction");
            }
            Some("DROP") => return Some("a DROP"),
            Some("TRUNCATE") => return Some("a TRUNCATE"),
            Some("DELETE") => return Some("a DELETE"),
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
}

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

/// Refuses the database of `session` when it holds a migration newer than `set` that says
/// `refuse`, as [`apply`] does, reading and making nothing: a database no migration ran on holds
/// none.
///
/// # Errors
/// [`ApplyError`]: a newer schema, or the bookkeeping could not be read.
pub(crate) fn refuse_newer(session: &mut Session, set: &[Migration]) -> Result<(), ApplyError> {
    let database = |e: postgres::Error| ApplyError::Database(describe(&e));
    let made: bool = session
        .query_one("SELECT to_regclass('schema_version') IS NOT NULL", &[])
        .map_err(database)?
        .get(0);
    if !made {
        return Ok(());
    }
    let rows: Vec<Applied> = session
        .query(
            "SELECT version, name, older_releases FROM schema_version ORDER BY version",
            &[],
        )
        .map_err(database)?
        .iter()
        .map(|row| Applied {
            version: row.get(0),
            name: row.get(1),
            older: row.get(2),
        })
        .collect();
    newer(&rows, set)
}

/// Brings the database of `session`, which holds the lock, up to `set`: refuses a database that
/// holds a migration `set` does not know and that says `refuse`, then runs each migration it
/// lacks, in order, each in its own transaction with its `schema_version` row. Answers the
/// numbers it ran.
///
/// # Errors
/// [`ApplyError`]: a newer schema, a migration that failed (rolled back), or the bookkeeping
/// could not be read.
pub(crate) fn apply(session: &mut Session, set: &[Migration]) -> Result<Vec<i32>, ApplyError> {
    let database = |e: postgres::Error| ApplyError::Database(describe(&e));
    let rows = applied(session).map_err(database)?;
    newer(&rows, set)?;
    let mut ran = Vec::new();
    for migration in set {
        if rows.iter().any(|row| row.version == migration.version) {
            continue;
        }
        let failed = |e: postgres::Error| ApplyError::Failed {
            version: migration.version,
            name: migration.name,
            why: describe(&e),
        };
        let run = session.batch("BEGIN").and_then(|()| {
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
            return Err(failed(e));
        }
        ran.push(migration.version);
    }
    Ok(ran)
}

impl fmt::Display for OlderReleases {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
#[path = "migrate_tests.rs"]
mod tests;
