//! The operator's browser sessions and the login limiter as rows
//! (`docs/design/coordinator-multi-instance.md`, section 8), with the rules of
//! [`crate::session`]: the same deadlines, the same cap, the same counts, so that a cookie made by
//! one coordinator is answered by any, and a deploy signs nobody out.
//!
//! A lookup, an end and a change of one session are one statement on the session's row. What
//! reads or moves several rows (making a session past the cap, counting a failed login past the
//! number of names tracked) runs in a transaction that holds a transaction-scoped advisory lock of
//! its own, taken first: the lock is Postgres's and goes with the transaction, so a coordinator
//! that dies in the middle holds nothing, and these two paths are rare (a login, a failed login).
//! Time is the `now` the caller passes, the injected clock of the in-memory table.
// The login routes do not use these yet; `tests/login_rows.rs` runs every statement against a
// server, and against the in-memory table's answers, until they do.
#![allow(
    dead_code,
    reason = "the login routes read and write sessions and counts through these functions in the next pull request of the stack"
)]

use super::session::Session;
use crate::session::Failure;

/// The key of the advisory lock that serialises the making of sessions.
const SESSIONS_LOCK: i64 = 0x686b_7365_7373;
/// The key of the advisory lock that serialises the counting of failed logins.
const LIMITER_LOCK: i64 = 0x686b_6c69_6d74;

/// The deadlines and the cap of the session table (`SessionSettings`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SessionRules {
    pub(crate) idle_secs: i64,
    pub(crate) max_secs: i64,
    pub(crate) max_count: i64,
}

/// A session to make.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NewSession<'a> {
    /// The SHA-256 of the cookie's id.
    pub(crate) id_sha256: &'a [u8],
    pub(crate) username: &'a str,
    pub(crate) csrf: &'a str,
    pub(crate) must_change: bool,
    pub(crate) must_change_until: Option<i64>,
}

/// What a live session says of itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Live {
    pub(crate) username: String,
    pub(crate) csrf: String,
    pub(crate) must_change: bool,
    pub(crate) must_change_until: Option<i64>,
}

/// The expired test of the in-memory table, with the elapsed time saturating at 0 so that a clock
/// that stepped back never ends a session.
const EXPIRED: &str = "(greatest($2::bigint - last_used_unix, 0) >= $3::bigint \
                       OR greatest($2::bigint - created_unix, 0) >= $4::bigint)";

fn live(row: &postgres::Row) -> Live {
    Live {
        username: row.get(0),
        csrf: row.get(1),
        must_change: row.get(2),
        must_change_until: row.get(3),
    }
}

/// Runs `body` in a transaction that holds the advisory lock `key`, committing when it answers.
fn locked<R>(
    session: &mut Session,
    key: i64,
    body: impl FnOnce(&mut Session) -> Result<R, postgres::Error>,
) -> Result<R, postgres::Error> {
    session.batch("BEGIN")?;
    let done = session
        .execute("SELECT pg_advisory_xact_lock($1)", &[&key])
        .and_then(|_| body(session));
    match done {
        Ok(answer) => {
            session.batch("COMMIT")?;
            Ok(answer)
        }
        Err(e) => {
            // A session that cannot roll back is gone, and the server ends the transaction.
            let _ = session.batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Makes a session at `now`: the expired ones go first, then, with `max_count` held, the one
/// least recently used (then the oldest, then the first made).
///
/// # Errors
/// The client's error.
pub(crate) fn create(
    session: &mut Session,
    rules: &SessionRules,
    new: &NewSession<'_>,
    now: i64,
) -> Result<(), postgres::Error> {
    locked(session, SESSIONS_LOCK, |s| {
        s.execute(
            "DELETE FROM sessions WHERE greatest($1::bigint - last_used_unix, 0) >= $2::bigint \
             OR greatest($1::bigint - created_unix, 0) >= $3::bigint",
            &[&now, &rules.idle_secs, &rules.max_secs],
        )?;
        let held: i64 = s.query_one("SELECT count(*) FROM sessions", &[])?.get(0);
        if held >= rules.max_count {
            s.execute(
                "DELETE FROM sessions WHERE id_sha256 = (SELECT id_sha256 FROM sessions \
                 ORDER BY last_used_unix, created_unix, serial LIMIT 1)",
                &[],
            )?;
        }
        s.execute(
            "INSERT INTO sessions (id_sha256, username, csrf, must_change, must_change_until, \
             created_unix, last_used_unix) VALUES ($1, $2, $3, $4, $5, $6, $6)",
            &[
                &new.id_sha256,
                &new.username,
                &new.csrf,
                &new.must_change,
                &new.must_change_until,
                &now,
            ],
        )?;
        Ok(())
    })
}

/// The live session of a cookie, its idle deadline pushed out to `now`; none for an unknown or
/// expired one, which is forgotten.
///
/// # Errors
/// The client's error.
pub(crate) fn lookup(
    session: &mut Session,
    rules: &SessionRules,
    id_sha256: &[u8],
    now: i64,
) -> Result<Option<Live>, postgres::Error> {
    Ok(session
        .query_opt(
            &format!(
                "WITH gone AS (DELETE FROM sessions WHERE id_sha256 = $1 AND {EXPIRED} RETURNING 1), \
                 pushed AS (UPDATE sessions SET last_used_unix = greatest(last_used_unix, $2::bigint) \
                   WHERE id_sha256 = $1 AND NOT {EXPIRED} \
                   RETURNING username, csrf, must_change, must_change_until) \
                 SELECT username, csrf, must_change, must_change_until FROM pushed"
            ),
            &[&id_sha256, &now, &rules.idle_secs, &rules.max_secs],
        )?
        .map(|row| live(&row)))
}

/// Clears the session's `must_change` and its deadline: the user chose a new password. The
/// session as it is now; none for an unknown or expired one (which is left for a lookup to forget).
///
/// # Errors
/// The client's error.
pub(crate) fn clear_must_change(
    session: &mut Session,
    rules: &SessionRules,
    id_sha256: &[u8],
    now: i64,
) -> Result<Option<Live>, postgres::Error> {
    Ok(session
        .query_opt(
            &format!(
                "UPDATE sessions SET must_change = false, must_change_until = NULL \
                 WHERE id_sha256 = $1 AND NOT {EXPIRED} \
                 RETURNING username, csrf, must_change, must_change_until"
            ),
            &[&id_sha256, &now, &rules.idle_secs, &rules.max_secs],
        )?
        .map(|row| live(&row)))
}

/// Ends the session of a cookie; whether there was one.
///
/// # Errors
/// The client's error.
pub(crate) fn end(session: &mut Session, id_sha256: &[u8]) -> Result<bool, postgres::Error> {
    Ok(session.execute("DELETE FROM sessions WHERE id_sha256 = $1", &[&id_sha256])? == 1)
}

/// Ends every session of `username` but the one `keep` names; how many ended.
///
/// # Errors
/// The client's error.
pub(crate) fn end_user(
    session: &mut Session,
    username: &str,
    keep: Option<&[u8]>,
) -> Result<u64, postgres::Error> {
    session.execute(
        "DELETE FROM sessions WHERE username = $1 AND ($2::bytea IS NULL OR id_sha256 <> $2)",
        &[&username, &keep],
    )
}

/// How many sessions are held, expired ones not yet forgotten included.
///
/// # Errors
/// The client's error.
pub(crate) fn held(session: &mut Session) -> Result<i64, postgres::Error> {
    Ok(session
        .query_one("SELECT count(*) FROM sessions", &[])?
        .get(0))
}

/// The limiter's numbers (`LimiterSettings`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LimiterRules {
    pub(crate) max_failures: i64,
    pub(crate) lockout_secs: i64,
    pub(crate) max_tracked: i64,
}

/// Whether `name` is locked at `now`.
///
/// # Errors
/// The client's error.
pub(crate) fn is_locked(
    session: &mut Session,
    name: &str,
    now: i64,
) -> Result<bool, postgres::Error> {
    Ok(session
        .query_opt(
            "SELECT 1 FROM login_failures WHERE name = $1 AND locked_until_unix > $2",
            &[&name, &now],
        )?
        .is_some())
}

/// Counts a failed login of `name` at `now`, as [`crate::session::LoginLimiter::failed`] does:
/// names forgotten once a lockout has passed since their last failure, the least important name
/// dropped to make room past `max_tracked`, the count that reaches `max_failures` locking the
/// name, a failure while locked counted without moving the lock.
///
/// # Errors
/// The client's error.
pub(crate) fn failed(
    session: &mut Session,
    rules: &LimiterRules,
    name: &str,
    now: i64,
) -> Result<Failure, postgres::Error> {
    locked(session, LIMITER_LOCK, |s| {
        s.execute(
            "DELETE FROM login_failures WHERE NOT (coalesce(locked_until_unix > $1, false) \
             OR greatest($1::bigint - last_failure_unix, 0) < $2)",
            &[&now, &rules.lockout_secs],
        )?;
        let row = s.query_opt(
            "SELECT failures, last_failure_unix, locked_until_unix FROM login_failures \
             WHERE name = $1",
            &[&name],
        )?;
        let tracked: i64 = s
            .query_one("SELECT count(*) FROM login_failures", &[])?
            .get(0);
        if row.is_none() && tracked >= rules.max_tracked {
            s.execute(
                "DELETE FROM login_failures WHERE name = (SELECT name FROM login_failures \
                 ORDER BY coalesce(locked_until_unix > $1, false), last_failure_unix, name LIMIT 1)",
                &[&now],
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
        s.execute(
            "INSERT INTO login_failures (name, failures, last_failure_unix, locked_until_unix) \
             VALUES ($1, $2, $3, $4) ON CONFLICT (name) DO UPDATE SET failures = excluded.failures, \
             last_failure_unix = excluded.last_failure_unix, \
             locked_until_unix = excluded.locked_until_unix",
            &[&name, &failures, &last, &until],
        )?;
        Ok(outcome)
    })
}

/// A successful login of `name` clears its count and its lock.
///
/// # Errors
/// The client's error.
pub(crate) fn succeeded(session: &mut Session, name: &str) -> Result<(), postgres::Error> {
    session.execute("DELETE FROM login_failures WHERE name = $1", &[&name])?;
    Ok(())
}

/// How many names hold a count.
///
/// # Errors
/// The client's error.
pub(crate) fn tracked(session: &mut Session) -> Result<i64, postgres::Error> {
    Ok(session
        .query_one("SELECT count(*) FROM login_failures", &[])?
        .get(0))
}
