//! Periodic work claimed per job (`docs/design/coordinator-multi-instance.md`, section 9): each
//! coordinator ticks, and a job that is due goes to one of them for the period.
//!
//! The claim is one statement that moves the job's `next_run_unix` on by its period, over the
//! rows that are due and not locked by another claimer (`FOR UPDATE SKIP LOCKED`); a claimer that
//! arrives after the first committed finds the row no longer due. This is not a lease: the row
//! has no holder and no expiry. A coordinator that dies after claiming loses that run, and the
//! job is due again one period later for any coordinator, so the jobs it names are level-triggered
//! and idempotent, and the claim only saves work.
use std::sync::Arc;

use super::Postgres;
use super::session::Session;
use crate::audit_file::UnixClock;
use crate::store::Claims;

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

/// Claims the jobs of `names` that are due at `now` and that no other claimer holds, for
/// `instance`: the names of the jobs this coordinator runs now.
///
/// # Errors
/// The client's error.
pub(crate) fn claim(
    session: &mut Session,
    now: i64,
    instance: &str,
    names: &[&str],
) -> Result<Vec<String>, postgres::Error> {
    Ok(session
        .query(
            "WITH due AS (SELECT name FROM periodic_jobs WHERE next_run_unix <= $1 AND name = ANY($3) \
             ORDER BY next_run_unix, name FOR UPDATE SKIP LOCKED) \
             UPDATE periodic_jobs j SET next_run_unix = $1 + j.period_secs, last_run_unix = $1, \
             last_instance = $2 FROM due WHERE j.\"name\" = due.\"name\" RETURNING j.\"name\"",
            &[&now, &instance, &names],
        )?
        .iter()
        .map(|row| row.get(0))
        .collect())
}

/// The periodic jobs of a coordinator over a database: each runs once a period on whichever
/// coordinator ticks first after it is due.
pub(crate) struct PgClaims {
    store: Postgres,
    instance: String,
    unix: UnixClock,
}

impl PgClaims {
    /// Makes sure each of `jobs` (a name and its period in seconds) exists, due `first_unix`
    /// when new to the table, and claims for `instance`, at the time `unix` reads.
    ///
    /// # Errors
    /// What the session or the server said.
    pub(crate) fn start(
        store: Postgres,
        instance: &str,
        jobs: &[(&str, i64)],
        unix: UnixClock,
    ) -> Result<Arc<PgClaims>, String> {
        let first_unix = i64::try_from(unix()).unwrap_or(i64::MAX);
        let jobs: Vec<(String, i64)> = jobs.iter().map(|(n, p)| ((*n).to_owned(), *p)).collect();
        store.shared(move |session| {
            jobs.iter()
                .try_for_each(|(name, period)| seed(session, name, *period, first_unix))
        })?;
        Ok(Arc::new(PgClaims {
            store,
            instance: instance.to_owned(),
            unix,
        }))
    }
}

impl Claims for PgClaims {
    fn claimed(&self, job: &str) -> bool {
        let now = i64::try_from((self.unix)()).unwrap_or(i64::MAX);
        let (instance, job) = (self.instance.clone(), job.to_owned());
        let claimed = self.store.shared(move |session| {
            claim(session, now, &instance, &[job.as_str()]).map(|names| !names.is_empty())
        });
        claimed.unwrap_or_else(|why| {
            // The job is due again for the next claimer, and a look it skips is made up by the
            // next one.
            tracing::warn!(%why, "a periodic job could not be claimed");
            false
        })
    }
}
