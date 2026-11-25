//! Periodic work claimed per job by advancing its due time (`docs/design/coordinator-multi-instance.md`,
//! section 9): every coordinator ticks, and a job that is due goes to one of them for the period.
//!
//! The claim is one statement: the rows of `periodic_jobs` that are due and that no other claimer
//! holds (`FOR UPDATE SKIP LOCKED`) have `next_run_unix` moved on by the job's period. A claimer
//! that waits instead re-checks the row after the first commits and finds it no longer due. This
//! is not a lease: the row has no holder and no expiry. A coordinator that dies after claiming
//! loses that run, and the job is due again one period later for any coordinator; every job it
//! names derives its work from the rows and is idempotent, and its effects are conditional writes,
//! so the claim only saves work. Time is the database's `now()`.

use super::Postgres;
use super::login_rows::{self, LimiterRules, SessionRules};
use super::writer::{Tx, WriteFail};
use crate::audit_file::UnixClock;

/// The job that observes the alerts and logs each new one.
pub(crate) const ALERTS: &str = "alerts";
/// The job that expires the enrollment requests no operator approved in time.
pub(crate) const REQUEST_EXPIRY: &str = "request-expiry";
/// The job that deletes rows no rule reads any more.
pub(crate) const PRUNE: &str = "prune";
/// The job that reads the list of Xcode releases ([`crate::xcode_feed`]).
pub(crate) const XCODE_FEED: &str = "xcode-feed";
/// Every job, as `--job-every` names them.
pub(crate) const JOBS: [&str; 4] = [ALERTS, REQUEST_EXPIRY, PRUNE, XCODE_FEED];

/// How long an instance's row of `instances` stays after it was last seen: a day.
pub(crate) const INSTANCE_KEEP_SECS: i64 = 86_400;

/// The database's second, or the caller's `$n` when it gives one.
fn now_of(n: usize) -> String {
    format!("coalesce(${n}::bigint, extract(epoch FROM now())::bigint)")
}

/// Makes a row for each job of `names` that has none, due at once; a job that has one keeps its
/// due time.
///
/// # Errors
/// The client's error.
pub(crate) fn seed(
    tx: &mut Tx<'_>,
    names: &[&str],
    at: Option<i64>,
) -> Result<u64, postgres::Error> {
    tx.execute(
        &format!(
            "INSERT INTO periodic_jobs (name, next_run_unix) SELECT name, {} \
             FROM unnest($1::text[]) AS name ON CONFLICT (name) DO NOTHING",
            now_of(2)
        ),
        &[&names, &at],
    )
}

/// Claims for `instance` the jobs of `jobs` (each a name and its period in seconds) that are due
/// and that no other claimer holds, moving each one's due time on by its period: the names of the
/// jobs this coordinator runs now.
///
/// # Errors
/// The client's error.
pub(crate) fn claim(
    tx: &mut Tx<'_>,
    jobs: &[(String, i64)],
    instance: &str,
    at: Option<i64>,
) -> Result<Vec<String>, postgres::Error> {
    let (names, periods): (Vec<&str>, Vec<i64>) = jobs
        .iter()
        .map(|(name, period)| (name.as_str(), *period))
        .unzip();
    let rows = tx.query(
        &format!(
            "WITH t AS (SELECT {} AS now), \
             want AS (SELECT * FROM unnest($1::text[], $2::bigint[]) AS w(name, period)), \
             due AS (SELECT j.\"name\" FROM periodic_jobs j, t \
                     WHERE j.\"name\" IN (SELECT name FROM want) AND j.next_run_unix <= t.\"now\" \
                     ORDER BY j.next_run_unix, j.\"name\" FOR UPDATE OF j SKIP LOCKED) \
             UPDATE periodic_jobs j SET next_run_unix = t.\"now\" + want.period, \
               last_run_unix = t.\"now\", last_instance = $3 \
             FROM due, want, t WHERE j.\"name\" = due.\"name\" AND want.\"name\" = j.\"name\" \
             RETURNING j.\"name\"",
            now_of(4)
        ),
        &[&names, &periods, &instance, &at],
    )?;
    let mut claimed: Vec<String> = rows.iter().map(|row| row.get(0)).collect();
    claimed.sort_unstable();
    Ok(claimed)
}

/// Deletes the rows of the instances not seen for [`INSTANCE_KEEP_SECS`]; how many went. A
/// running instance's next beat makes its row again.
///
/// # Errors
/// The client's error.
pub(crate) fn prune_instances(tx: &mut Tx<'_>, at: Option<i64>) -> Result<u64, postgres::Error> {
    tx.execute(
        &format!("DELETE FROM instances WHERE seen_unix < {} - $2", now_of(1)),
        &[&at, &INSTANCE_KEEP_SECS],
    )
}

/// Deletes the orders of `fleet_orders` that have aged out ([`crate::fleet::ORDER_AGE`] or
/// more since they were given): every reader already takes them as freed slots, so deleting one
/// without the gate frees nothing a decision holds (section 5.2); how many went.
///
/// # Errors
/// The client's error.
pub(crate) fn prune_orders(tx: &mut Tx<'_>, at: Option<i64>) -> Result<u64, postgres::Error> {
    let age = i64::try_from(crate::fleet::ORDER_AGE.as_secs()).unwrap_or(i64::MAX);
    tx.execute(
        &format!(
            "DELETE FROM fleet_orders WHERE at_unix <= {} - $2",
            now_of(1)
        ),
        &[&at, &age],
    )
}

/// What one run of the job `prune` deleted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Pruned {
    /// Sessions past their idle or absolute deadline.
    pub(crate) sessions: u64,
    /// Failure counts the limiter has forgotten.
    pub(crate) login_failures: u64,
    /// Rows of instances not seen for a day.
    pub(crate) instances: u64,
    /// Orders that aged out.
    pub(crate) fleet_orders: u64,
}

/// The periodic jobs of a coordinator over a database.
pub(crate) struct PgJobs {
    store: Postgres,
    periods: Vec<(String, i64)>,
    sessions: SessionRules,
    limiter: LimiterRules,
    /// A fixed clock in place of the database's, for a test.
    clock: Option<UnixClock>,
}

impl std::fmt::Debug for PgJobs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PgJobs")
            .field("periods", &self.periods)
            .finish_non_exhaustive()
    }
}

fn written<R>(done: Result<R, WriteFail>) -> Result<R, String> {
    done.map_err(|fail| fail.why().to_owned())
}

impl PgJobs {
    /// The jobs of `periods` on `store`, each made due at once if the table has no row for it,
    /// pruning by the session and limiter rules of `login`.
    ///
    /// # Errors
    /// Why the rows could not be made.
    pub(crate) fn start(
        store: Postgres,
        periods: Vec<(String, i64)>,
        login: &login_rows::PgLoginRows,
    ) -> Result<PgJobs, String> {
        let jobs = PgJobs {
            store,
            periods,
            sessions: login.session_rules(),
            limiter: login.limiter_rules(),
            clock: None,
        };
        jobs.seed()?;
        Ok(jobs)
    }

    /// The same jobs judged by `clock` instead of the database's `now()`.
    #[cfg(test)]
    pub(crate) fn with_clock(mut self, clock: UnixClock) -> PgJobs {
        self.clock = Some(clock);
        self
    }

    fn at(&self) -> Option<i64> {
        self.clock.as_ref().map(|clock| login_rows::secs(clock()))
    }

    /// Makes the rows of the jobs that have none.
    ///
    /// # Errors
    /// Why they could not be written.
    pub(crate) fn seed(&self) -> Result<(), String> {
        let names: Vec<String> = self.periods.iter().map(|(name, _)| name.clone()).collect();
        let at = self.at();
        written(self.store.write(move |tx| {
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            seed(tx, &names, at)
        }))
        .map(drop)
    }

    /// Claims for this instance the jobs that are due ([`claim`]).
    ///
    /// # Errors
    /// Why the claim could not be written: nothing is claimed.
    pub(crate) fn claim(&self) -> Result<Vec<String>, String> {
        let (periods, at) = (self.periods.clone(), self.at());
        let instance = self.store.instance().id.clone();
        written(
            self.store
                .write(move |tx| claim(tx, &periods, &instance, at)),
        )
    }

    /// The job `prune`: expired sessions, forgotten failure counts, instances unseen for a day and
    /// aged orders deleted, in one transaction. The ledger of transactions is pruned by every
    /// transaction already, and the audit is append-only.
    ///
    /// # Errors
    /// Why the rows could not be deleted.
    pub(crate) fn prune(&self) -> Result<Pruned, String> {
        let (sessions, limiter, at) = (self.sessions, self.limiter, self.at());
        written(self.store.write(move |tx| {
            let (ended, forgotten) = login_rows::prune(tx, &sessions, &limiter, at)?;
            Ok(Pruned {
                sessions: ended,
                login_failures: forgotten,
                instances: prune_instances(tx, at)?,
                fleet_orders: prune_orders(tx, at)?,
            })
        }))
    }
}
