//! Periodic work claimed per job (`docs/design/coordinator-multi-instance.md`, section 9): each
//! coordinator ticks, and a job that is due goes to one of them for the period.
//!
//! The claim is one statement that moves the job's `next_run_unix` on by its period, over the
//! rows that are due and not locked by another claimer (`FOR UPDATE SKIP LOCKED`); a claimer that
//! arrives after the first committed finds the row no longer due. This is not a lease: the row
//! has no holder and no expiry. A coordinator that dies after claiming loses that run, and the
//! job is due again one period later for any coordinator, so the jobs it names are level-triggered
//! and idempotent, and the claim only saves work.
// The coordinator's loops do not claim through these yet; `tests/periodic.rs` runs the statements
// against a server until they do.
#![allow(
    dead_code,
    reason = "the coordinator's periodic loops claim through these functions in the next pull request of the stack"
)]

use super::session::Session;

/// Makes sure the job `name` exists and runs every `period_secs`; a job new to the table is
/// first due at `first_unix`. A changed period takes effect at the job's next claim.
///
/// # Errors
/// The client's error.
pub(crate) fn seed(
    session: &mut Session,
    name: &str,
    period_secs: i64,
    first_unix: i64,
) -> Result<(), postgres::Error> {
    session.execute(
        "INSERT INTO periodic_jobs (name, period_secs, next_run_unix) VALUES ($1, $2, $3) \
         ON CONFLICT (name) DO UPDATE SET period_secs = excluded.period_secs",
        &[&name, &period_secs, &first_unix],
    )?;
    Ok(())
}

/// Claims every job that is due at `now` and that no other claimer holds, for `instance`: the
/// names of the jobs this coordinator runs now.
///
/// # Errors
/// The client's error.
pub(crate) fn claim(
    session: &mut Session,
    now: i64,
    instance: &str,
) -> Result<Vec<String>, postgres::Error> {
    Ok(session
        .query(
            "WITH due AS (SELECT name FROM periodic_jobs WHERE next_run_unix <= $1 \
             ORDER BY next_run_unix, name FOR UPDATE SKIP LOCKED) \
             UPDATE periodic_jobs j SET next_run_unix = $1 + j.period_secs, last_run_unix = $1, \
             last_instance = $2 FROM due WHERE j.\"name\" = due.\"name\" RETURNING j.\"name\"",
            &[&now, &instance],
        )?
        .iter()
        .map(|row| row.get(0))
        .collect())
}
