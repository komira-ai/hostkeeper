//! The operator's browser sessions and the login limiter as rows of migration `0023`
//! (`docs/design/coordinator-multi-instance.md`, section 8), with the rules of
//! [`crate::session`]: the same deadlines, the same caps, the same counts, so that a cookie made
//! by one coordinator is answered by any, a failed login counts once however many coordinators
//! hear it, and a deploy signs nobody out.
//!
//! A lookup reads the session's row on the read-only session and writes only what it must: an
//! expired row is deleted, and `last_used_unix` is moved on when it is at least
//! `--session-touch-secs` old, each by a conditional statement on that one row. An end and a
//! change of one session are one statement on its row. What reads or moves several rows (making a
//! session against the cap, counting a failed login against the cap on names) runs in a
//! transaction that first locks its row of `login_gate` `FOR UPDATE`: a row lock that Postgres
//! releases at the commit, the rollback or the death of the backend, so a coordinator that dies in
//! the middle holds nothing.
//!
//! Time is the database's `now()`, read in the statement that judges by it; a test passes a fixed
//! second instead ([`PgLoginRows::at`]).

use super::Postgres;
use super::session::Session;
use super::writer::{Tx, WriteFail};
use crate::audit_file::UnixClock;
use crate::session::{Failure, LimiterSettings, SessionSettings};

/// The database's second when the caller gives none: `$n` is the caller's.
fn now_of(n: usize) -> String {
    format!("coalesce(${n}::bigint, extract(epoch FROM now())::bigint)")
}

/// The deadlines, the cap and the touch interval of the session rows (`SessionSettings`,
/// `--session-touch-secs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SessionRules {
    pub(crate) idle_secs: i64,
    pub(crate) max_secs: i64,
    pub(crate) max_count: i64,
    pub(crate) touch_secs: i64,
}

/// The limiter's numbers (`LimiterSettings`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LimiterRules {
    pub(crate) max_failures: i64,
    pub(crate) lockout_secs: i64,
    pub(crate) max_tracked: i64,
}

/// A session as its row holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StoredSession {
    pub(crate) username: String,
    pub(crate) csrf: String,
    pub(crate) must_change: bool,
    pub(crate) must_change_until: Option<u64>,
}

/// Seconds as the columns hold them: a value past what they hold is the most they hold.
pub(crate) fn secs(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn unsigned(value: i64) -> u64 {
    u64::try_from(value).unwrap_or_default()
}

/// The expired test of the in-memory table over the columns of `sessions`, at the second `now`
/// (an SQL expression), with the elapsed time saturating at 0 so that a clock that stepped back
/// never ends a session; `$idle` and `$max` are parameter numbers.
fn expired(now: &str, idle: usize, max: usize) -> String {
    format!(
        "(greatest({now} - last_used_unix, 0) >= ${idle}::bigint \
         OR greatest({now} - created_unix, 0) >= ${max}::bigint)"
    )
}

fn stored(row: &postgres::Row) -> StoredSession {
    StoredSession {
        username: row.get(0),
        csrf: row.get(1),
        must_change: row.get(2),
        must_change_until: row.get::<_, Option<i64>>(3).map(unsigned),
    }
}

/// Locks the gate row `name` for the rest of the transaction, then reads the second the
/// transaction judges by.
fn gate(tx: &mut Tx<'_>, name: &str, at: Option<i64>) -> Result<i64, postgres::Error> {
    tx.query_one(
        "SELECT name FROM login_gate WHERE name = $1 FOR UPDATE",
        &[&name],
    )?;
    Ok(tx
        .query_one(&format!("SELECT {}", now_of(1)), &[&at])?
        .get(0))
}

/// Makes the session `id_sha256` for `new`: the expired sessions go first, then, with the cap
/// held, the one least recently used (then the oldest, then the first made). Under the gate row
/// `sessions`, so that two logins at once never pass the cap.
///
/// # Errors
/// The client's error.
pub(crate) fn create(
    tx: &mut Tx<'_>,
    rules: &SessionRules,
    id_sha256: &[u8],
    new: &StoredSession,
    at: Option<i64>,
) -> Result<(), postgres::Error> {
    let now = gate(tx, "sessions", at)?;
    tx.execute(
        &format!("DELETE FROM sessions WHERE {}", expired("$1::bigint", 2, 3)),
        &[&now, &rules.idle_secs, &rules.max_secs],
    )?;
    let held: i64 = tx.query_one("SELECT count(*) FROM sessions", &[])?.get(0);
    if held >= rules.max_count {
        tx.execute(
            "DELETE FROM sessions WHERE id_sha256 IN (SELECT id_sha256 FROM sessions \
             ORDER BY last_used_unix, created_unix, serial LIMIT $1)",
            &[&(held - rules.max_count + 1)],
        )?;
    }
    let until = new.must_change_until.map(secs);
    tx.execute(
        "INSERT INTO sessions (id_sha256, username, csrf, must_change, must_change_until, \
         created_unix, last_used_unix) VALUES ($1, $2, $3, $4, $5, $6, $6)",
        &[
            &id_sha256,
            &new.username,
            &new.csrf,
            &new.must_change,
            &until,
            &now,
        ],
    )?;
    Ok(())
}

/// What a read of one session's row found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Found {
    /// No row.
    None,
    /// A row past its idle or absolute deadline.
    Expired,
    /// A live row, and whether its `last_used_unix` is due to be moved on.
    Live(StoredSession, bool),
}

/// Reads the session `id_sha256` on the read-only session.
///
/// # Errors
/// The client's error.
pub(crate) fn find(
    session: &mut Session,
    rules: &SessionRules,
    id_sha256: &[u8],
    at: Option<i64>,
) -> Result<Found, postgres::Error> {
    let now = now_of(2);
    let row = session.query_opt(
        &format!(
            "SELECT username, csrf, must_change, must_change_until, {}, \
             greatest({now} - last_used_unix, 0) >= $5::bigint \
             FROM sessions WHERE id_sha256 = $1",
            expired(&now, 3, 4)
        ),
        &[
            &id_sha256,
            &at,
            &rules.idle_secs,
            &rules.max_secs,
            &rules.touch_secs,
        ],
    )?;
    Ok(match row {
        None => Found::None,
        Some(row) if row.get::<_, bool>(4) => Found::Expired,
        Some(row) => Found::Live(stored(&row), row.get(5)),
    })
}

/// Deletes the session `id_sha256` if it has expired; whether it did.
///
/// # Errors
/// The client's error.
pub(crate) fn forget_expired(
    tx: &mut Tx<'_>,
    rules: &SessionRules,
    id_sha256: &[u8],
    at: Option<i64>,
) -> Result<bool, postgres::Error> {
    Ok(tx.execute(
        &format!(
            "DELETE FROM sessions WHERE id_sha256 = $1 AND {}",
            expired(&now_of(2), 3, 4)
        ),
        &[&id_sha256, &at, &rules.idle_secs, &rules.max_secs],
    )? == 1)
}

/// Moves the idle deadline of the live session `id_sha256` out to now; whether it is live.
///
/// # Errors
/// The client's error.
pub(crate) fn touch(
    tx: &mut Tx<'_>,
    rules: &SessionRules,
    id_sha256: &[u8],
    at: Option<i64>,
) -> Result<bool, postgres::Error> {
    let now = now_of(2);
    Ok(tx.execute(
        &format!(
            "UPDATE sessions SET last_used_unix = greatest(last_used_unix, {now}) \
             WHERE id_sha256 = $1 AND NOT {}",
            expired(&now, 3, 4)
        ),
        &[&id_sha256, &at, &rules.idle_secs, &rules.max_secs],
    )? == 1)
}

/// Clears the session's `must_change` and its deadline: the user chose a new password. The
/// session as it is now; none for an unknown or expired one (which is left for a lookup to
/// forget).
///
/// # Errors
/// The client's error.
pub(crate) fn clear_must_change(
    tx: &mut Tx<'_>,
    rules: &SessionRules,
    id_sha256: &[u8],
    at: Option<i64>,
) -> Result<Option<StoredSession>, postgres::Error> {
    Ok(tx
        .query_opt(
            &format!(
                "UPDATE sessions SET must_change = false, must_change_until = NULL \
                 WHERE id_sha256 = $1 AND NOT {} \
                 RETURNING username, csrf, must_change, must_change_until",
                expired(&now_of(2), 3, 4)
            ),
            &[&id_sha256, &at, &rules.idle_secs, &rules.max_secs],
        )?
        .map(|row| stored(&row)))
}

/// Ends the session `id_sha256`; whether there was one.
///
/// # Errors
/// The client's error.
pub(crate) fn end(tx: &mut Tx<'_>, id_sha256: &[u8]) -> Result<bool, postgres::Error> {
    Ok(tx.execute("DELETE FROM sessions WHERE id_sha256 = $1", &[&id_sha256])? == 1)
}

/// Ends every session of `username` but the one `keep` names; how many ended.
///
/// # Errors
/// The client's error.
pub(crate) fn end_user(
    tx: &mut Tx<'_>,
    username: &str,
    keep: Option<&[u8]>,
) -> Result<u64, postgres::Error> {
    tx.execute(
        "DELETE FROM sessions WHERE username = $1 AND ($2::bytea IS NULL OR id_sha256 <> $2)",
        &[&username, &keep],
    )
}

/// Whether `name` is locked now.
///
/// # Errors
/// The client's error.
pub(crate) fn is_locked(
    session: &mut Session,
    name: &str,
    at: Option<i64>,
) -> Result<bool, postgres::Error> {
    Ok(session
        .query_opt(
            &format!(
                "SELECT 1 FROM login_failures WHERE name = $1 AND locked_until_unix > {}",
                now_of(2)
            ),
            &[&name, &at],
        )?
        .is_some())
}

/// Counts a failed login of `name`, as [`crate::session::LoginLimiter::failed`] does: names
/// forgotten once a lockout has passed since their last failure, the least important name dropped
/// to make room past `max_tracked`, the count that reaches `max_failures` locking the name, a
/// failure while locked counted without moving the lock. Under the gate row `login_failures`, so
/// that failures counted at once on several coordinators are each counted.
///
/// # Errors
/// The client's error.
pub(crate) fn failed(
    tx: &mut Tx<'_>,
    rules: &LimiterRules,
    name: &str,
    at: Option<i64>,
) -> Result<Failure, postgres::Error> {
    let now = gate(tx, "login_failures", at)?;
    tx.execute(STALE_FAILURES, &[&now, &rules.lockout_secs])?;
    let row = tx.query_opt(
        "SELECT failures, last_failure_unix, locked_until_unix FROM login_failures \
         WHERE name = $1",
        &[&name],
    )?;
    let tracked: i64 = tx
        .query_one("SELECT count(*) FROM login_failures", &[])?
        .get(0);
    if row.is_none() && tracked >= rules.max_tracked {
        tx.execute(
            "DELETE FROM login_failures WHERE name IN (SELECT name FROM login_failures \
             ORDER BY coalesce(locked_until_unix > $1, false), last_failure_unix, name LIMIT $2)",
            &[&now, &(tracked - rules.max_tracked + 1)],
        )?;
    }
    let (mut failures, mut last, mut until): (i64, i64, Option<i64>) =
        row.map_or((0, now, None), |r| (r.get(0), r.get(1), r.get(2)));
    if until.is_some_and(|u| u <= now) {
        failures = 0;
        until = None;
    }
    failures = failures.saturating_add(1);
    last = last.max(now);
    let outcome = if until.is_some_and(|u| now < u) {
        Failure::Counted
    } else if failures >= rules.max_failures {
        until = Some(now.saturating_add(rules.lockout_secs));
        Failure::Locked
    } else if failures == 1 {
        Failure::First
    } else {
        Failure::Counted
    };
    tx.execute(
        "INSERT INTO login_failures (name, failures, last_failure_unix, locked_until_unix) \
         VALUES ($1, $2, $3, $4) ON CONFLICT (name) DO UPDATE SET failures = excluded.failures, \
         last_failure_unix = excluded.last_failure_unix, \
         locked_until_unix = excluded.locked_until_unix",
        &[&name, &failures, &last, &until],
    )?;
    Ok(outcome)
}

/// The counts the limiter has forgotten at `$1` under the lockout `$2`: not locked, and their
/// last failure a lockout or more ago.
const STALE_FAILURES: &str = "DELETE FROM login_failures \
     WHERE NOT (coalesce(locked_until_unix > $1::bigint, false) \
     OR greatest($1::bigint - last_failure_unix, 0) < $2::bigint)";

/// A successful login of `name` clears its count and its lock.
///
/// # Errors
/// The client's error.
pub(crate) fn succeeded(tx: &mut Tx<'_>, name: &str) -> Result<(), postgres::Error> {
    tx.execute("DELETE FROM login_failures WHERE name = $1", &[&name])?;
    Ok(())
}

/// How many rows `table` holds.
///
/// # Errors
/// The client's error.
fn count(session: &mut Session, table: &str) -> Result<usize, postgres::Error> {
    let n: i64 = session
        .query_one(&format!("SELECT count(*) FROM {table}"), &[])?
        .get(0);
    Ok(usize::try_from(n).unwrap_or_default())
}

/// The expired sessions and the forgotten failure counts deleted, at the database's second (or
/// `at`): the periodic job `prune`'s part of the login. How many rows of each went.
///
/// # Errors
/// The client's error.
pub(crate) fn prune(
    tx: &mut Tx<'_>,
    sessions: &SessionRules,
    limiter: &LimiterRules,
    at: Option<i64>,
) -> Result<(u64, u64), postgres::Error> {
    let now: i64 = tx
        .query_one(&format!("SELECT {}", now_of(1)), &[&at])?
        .get(0);
    let ended = tx.execute(
        &format!("DELETE FROM sessions WHERE {}", expired("$1::bigint", 2, 3)),
        &[&now, &sessions.idle_secs, &sessions.max_secs],
    )?;
    let forgotten = tx.execute(STALE_FAILURES, &[&now, &limiter.lockout_secs])?;
    Ok((ended, forgotten))
}

/// The session rows and the failure counts of the login over a database: what
/// [`crate::session::Sessions`] and [`crate::session::LoginLimiter`] keep when the coordinator
/// runs on Postgres.
pub(crate) struct PgLoginRows {
    store: Postgres,
    sessions: SessionRules,
    limiter: LimiterRules,
    /// A fixed clock in place of the database's, for a test.
    clock: Option<UnixClock>,
}

impl std::fmt::Debug for PgLoginRows {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PgLoginRows")
            .field("sessions", &self.sessions)
            .field("limiter", &self.limiter)
            .finish_non_exhaustive()
    }
}

/// The text of a write that failed.
fn written<R>(done: Result<R, WriteFail>) -> Result<R, String> {
    done.map_err(|fail| fail.why().to_owned())
}

impl PgLoginRows {
    /// The rows of `store`, under the deadlines and counts of `sessions` and `limiter`, a
    /// session's `last_used_unix` written at most every `touch_secs`.
    pub(crate) fn new(
        store: Postgres,
        sessions: SessionSettings,
        limiter: LimiterSettings,
        touch_secs: u64,
    ) -> PgLoginRows {
        PgLoginRows {
            store,
            sessions: SessionRules {
                idle_secs: secs(sessions.idle_secs),
                max_secs: secs(sessions.max_secs),
                max_count: secs(sessions.max_count.max(1) as u64),
                touch_secs: secs(touch_secs),
            },
            limiter: LimiterRules {
                max_failures: i64::from(limiter.max_failures.max(1)),
                lockout_secs: secs(limiter.lockout_secs),
                max_tracked: secs(limiter.max_tracked.max(1) as u64),
            },
            clock: None,
        }
    }

    /// The same rows judged by `clock` instead of the database's `now()`.
    #[cfg(test)]
    pub(crate) fn with_clock(mut self, clock: UnixClock) -> PgLoginRows {
        self.clock = Some(clock);
        self
    }

    /// The second the next statement judges by: none for the database's own.
    pub(crate) fn at(&self) -> Option<i64> {
        self.clock.as_ref().map(|clock| secs(clock()))
    }

    /// The session rules.
    pub(crate) fn session_rules(&self) -> SessionRules {
        self.sessions
    }

    /// The limiter rules.
    pub(crate) fn limiter_rules(&self) -> LimiterRules {
        self.limiter
    }

    /// Makes the session of SHA-256 `id`.
    ///
    /// # Errors
    /// Why it could not be written.
    pub(crate) fn create(&self, id: &[u8; 32], new: &StoredSession) -> Result<(), String> {
        let (rules, at, id, new) = (self.sessions, self.at(), id.to_vec(), new.clone());
        written(
            self.store
                .write(move |tx| create(tx, &rules, &id, &new, at)),
        )
    }

    /// The live session of SHA-256 `id`, its idle deadline pushed out when it is due, and
    /// forgotten when it has expired; with `push` false, nothing is written.
    ///
    /// # Errors
    /// Why it could not be read or written.
    pub(crate) fn lookup(
        &self,
        id: &[u8; 32],
        push: bool,
    ) -> Result<Option<StoredSession>, String> {
        let (rules, at, key) = (self.sessions, self.at(), id.to_vec());
        let found = self.store.read(move |s| find(s, &rules, &key, at))?;
        match found {
            Found::None => Ok(None),
            Found::Expired => {
                if push {
                    let key = id.to_vec();
                    written(
                        self.store
                            .write(move |tx| forget_expired(tx, &rules, &key, at)),
                    )?;
                }
                Ok(None)
            }
            Found::Live(session, due) => {
                if push && due {
                    let key = id.to_vec();
                    let live = written(self.store.write(move |tx| touch(tx, &rules, &key, at)))?;
                    return Ok(live.then_some(session));
                }
                Ok(Some(session))
            }
        }
    }

    /// [`clear_must_change`] of SHA-256 `id`.
    ///
    /// # Errors
    /// Why it could not be written.
    pub(crate) fn clear_must_change(&self, id: &[u8; 32]) -> Result<Option<StoredSession>, String> {
        let (rules, at, id) = (self.sessions, self.at(), id.to_vec());
        written(
            self.store
                .write(move |tx| clear_must_change(tx, &rules, &id, at)),
        )
    }

    /// Ends the session of SHA-256 `id`; whether there was one.
    ///
    /// # Errors
    /// Why it could not be written.
    pub(crate) fn end(&self, id: &[u8; 32]) -> Result<bool, String> {
        let id = id.to_vec();
        written(self.store.write(move |tx| end(tx, &id)))
    }

    /// Ends every session of `username` but the one of SHA-256 `keep`; how many ended.
    ///
    /// # Errors
    /// Why it could not be written.
    pub(crate) fn end_user(
        &self,
        username: &str,
        keep: Option<&[u8; 32]>,
    ) -> Result<usize, String> {
        let (username, keep) = (username.to_owned(), keep.map(|k| k.to_vec()));
        written(
            self.store
                .write(move |tx| end_user(tx, &username, keep.as_deref())),
        )
        .map(|ended| usize::try_from(ended).unwrap_or_default())
    }

    /// How many sessions are held, expired ones not yet forgotten included.
    ///
    /// # Errors
    /// Why it could not be read.
    pub(crate) fn held(&self) -> Result<usize, String> {
        self.store.read(|s| count(s, "sessions"))
    }

    /// Whether `name` is locked now.
    ///
    /// # Errors
    /// Why it could not be read.
    pub(crate) fn is_locked(&self, name: &str) -> Result<bool, String> {
        let (name, at) = (name.to_owned(), self.at());
        self.store.read(move |s| is_locked(s, &name, at))
    }

    /// Counts a failed login of `name` ([`failed`]).
    ///
    /// # Errors
    /// Why it could not be written.
    pub(crate) fn failed(&self, name: &str) -> Result<Failure, String> {
        let (rules, at, name) = (self.limiter, self.at(), name.to_owned());
        written(self.store.write(move |tx| failed(tx, &rules, &name, at)))
    }

    /// Clears the count of `name`.
    ///
    /// # Errors
    /// Why it could not be written.
    pub(crate) fn succeeded(&self, name: &str) -> Result<(), String> {
        let name = name.to_owned();
        written(self.store.write(move |tx| succeeded(tx, &name)))
    }

    /// How many names hold a count.
    ///
    /// # Errors
    /// Why it could not be read.
    pub(crate) fn tracked(&self) -> Result<usize, String> {
        self.store.read(|s| count(s, "login_failures"))
    }
}
