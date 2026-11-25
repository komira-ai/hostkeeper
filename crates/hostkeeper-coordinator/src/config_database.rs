//! The flags of the Postgres store beside `--database-dsn-file`
//! (`docs/design/coordinator-multi-instance.md`, section 12): the pool of writing sessions, how
//! often this instance's row of `instances` is upserted, how a migration waits for its locks
//! and is tried again, how long an edit waits for the rollout gate, when a heartbeat older by
//! its host's clock than the one stored is believed, how often a session's last use is written,
//! and how often the periodic jobs are claimed and each runs. Each has a default; with the file
//! store none is read.

use std::net::SocketAddr;
use std::num::{NonZeroU32, NonZeroU64, NonZeroUsize};
use std::time::Duration;

use clap::Args;

use crate::store::postgres::migrate::Retry;
use crate::store::postgres::periodic::{ALERTS, JOBS, PRUNE, REQUEST_EXPIRY, XCODE_FEED};
use crate::store::postgres::{Options, Timing};

/// The most milliseconds `--migrate-ddl-lock-timeout-ms` takes: below the statement timeout of
/// every session of the store (5 seconds), which would end a longer wait first and fail the start
/// instead of rolling the migration back to try again.
const MAX_DDL_LOCK_TIMEOUT_MS: u64 = 4999;

/// The bound `--beat-reorder-secs` stays below: the seconds of silence after which a host is down
/// ([`crate::fleet::DOWN_AFTER`]), so a host whose clock stepped back is believed again before it
/// would count as down.
const MAX_BEAT_REORDER_SECS: u64 = crate::fleet::DOWN_AFTER.as_secs();

/// The flags of the Postgres store.
#[derive(Clone, Debug, Args)]
pub struct DatabaseFlags {
    /// With --database-dsn-file: how many writing sessions this coordinator holds on the
    /// database, each holding the coordinator's lock shared, so a write waits for a free session
    /// rather than for every write before it. At least 1. A Postgres serving N coordinators needs
    /// N x (this + 1) connections.
    #[arg(long, value_name = "SESSIONS", default_value = "4")]
    pub database_pool: NonZeroUsize,
    /// With --database-dsn-file: how often, in seconds, this coordinator moves on its row of the
    /// `instances` table (`seen_unix`), which says which coordinators serve on the database and
    /// since when. At least 1. No decision reads it.
    #[arg(long, value_name = "SECS", default_value = "5")]
    pub instance_beat_secs: NonZeroU64,
    /// With --database-dsn-file: how long, in milliseconds, a statement of a migration at start
    /// waits for a lock before the whole migration is rolled back, so the statements of a running
    /// coordinator queued behind it wait no longer than this. 1 to 4999, below the store's
    /// statement timeout of 5 seconds.
    #[arg(
        long,
        value_name = "MS",
        default_value_t = 3000,
        value_parser = clap::value_parser!(u64).range(1..=MAX_DDL_LOCK_TIMEOUT_MS)
    )]
    pub migrate_ddl_lock_timeout_ms: u64,
    /// With --database-dsn-file: how long, in seconds, a start waits after a migration was
    /// rolled back for want of a lock before it tries again.
    #[arg(long, value_name = "SECS", default_value_t = 5)]
    pub migrate_retry_secs: u64,
    /// With --database-dsn-file: how many times in all a start tries a migration that is rolled
    /// back for want of a lock before it gives up and exits, naming the migration. At least 1.
    #[arg(long, value_name = "N", default_value = "6")]
    pub migrate_attempts: NonZeroU32,
    /// With --database-dsn-file: how long, in milliseconds, an edit of the state (an operator's,
    /// or CI's) waits for the rollout gate, which one decision or edit holds at a time, and for
    /// the rows of the parts it edits; an edit still waiting then is refused `store_failed` with
    /// nothing changed. A heartbeat's own write never waits for the gate. At least 1.
    #[arg(long, value_name = "MS", default_value = "30000")]
    pub operator_gate_wait_ms: NonZeroU64,
    /// With --database-dsn-file: the fewest seconds between two writes of a browser session's
    /// last use; a use inside them is not written, so a session's idle deadline may count from
    /// up to this long before its last use.
    #[arg(long, value_name = "SECS", default_value_t = 30)]
    pub session_touch_secs: u64,
    /// With --database-dsn-file: how often, in seconds, this coordinator claims the periodic
    /// jobs that are due. At least 1.
    #[arg(long, value_name = "SECS", default_value = "5")]
    pub job_tick_secs: NonZeroU64,
    /// With --database-dsn-file: the period of a periodic job, NAME=SECS, one of `alerts`
    /// (default 15), `request-expiry` (default 15), `prune` (default 3600) and `xcode-feed`
    /// (default 86400, run only with --xcode-feed-url); SECS at least 1. Repeatable; a later one
    /// for the same job wins.
    #[arg(long, value_name = "NAME=SECS", value_parser = job_every)]
    pub job_every: Vec<JobEvery>,
    /// With --database-dsn-file: how old, in seconds, the stored heartbeat of a host must be
    /// before a heartbeat that its host's clock says is older replaces it. A heartbeat reaches the
    /// database through any coordinator, so two can arrive out of order; the older one is dropped,
    /// unless the host's clock stepped back and the stored one is this old. 1 to 44, below the 45
    /// seconds of silence after which a host is down.
    #[arg(
        long,
        value_name = "SECS",
        default_value_t = 30,
        value_parser = clap::value_parser!(u64).range(1..MAX_BEAT_REORDER_SECS)
    )]
    pub beat_reorder_secs: u64,
}

/// A period of `--job-every`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JobEvery {
    /// The job.
    pub name: &'static str,
    /// Its period.
    pub secs: NonZeroU64,
}

/// The default period of each job.
const JOB_DEFAULTS: [(&str, u64); 4] = [
    (ALERTS, 15),
    (REQUEST_EXPIRY, 15),
    (PRUNE, 3600),
    (XCODE_FEED, crate::xcode_feed::FEED_EVERY.as_secs()),
];

/// Parses `NAME=SECS` of `--job-every`.
fn job_every(text: &str) -> Result<JobEvery, String> {
    let (name, secs) = text
        .split_once('=')
        .ok_or_else(|| format!("{text:?} is not NAME=SECS"))?;
    let name = JOBS
        .into_iter()
        .find(|job| *job == name)
        .ok_or_else(|| format!("no job is named {name:?}: one of {}", JOBS.join(", ")))?;
    let secs = secs
        .parse::<NonZeroU64>()
        .map_err(|_| format!("{secs:?} is not a number of seconds of at least 1"))?;
    Ok(JobEvery { name, secs })
}

impl DatabaseFlags {
    /// The store's options of these flags, for an instance that runs `release`, listens on
    /// `listen` and holds decisions back for `quiet_secs` (`--quiet-secs`) after a gap in the
    /// heartbeats.
    pub(crate) fn options(&self, release: &str, listen: &str, quiet_secs: u64) -> Options {
        Options {
            pool: self.database_pool,
            migrate: Retry {
                lock_timeout: Duration::from_millis(self.migrate_ddl_lock_timeout_ms),
                pause: Duration::from_secs(self.migrate_retry_secs),
                attempts: self.migrate_attempts,
            },
            beat: Duration::from_secs(self.instance_beat_secs.get()),
            release: release.to_owned(),
            listen: listen.to_owned(),
            gate_wait: Duration::from_millis(self.operator_gate_wait_ms.get()),
            fleet: Timing::of(self.beat_reorder_secs, quiet_secs),
        }
    }
}

impl DatabaseFlags {
    /// Every job with its period in seconds: the default, or the last `--job-every` of it. The
    /// job `xcode-feed` is left out unless `xcode_feed` (a feed URL is set), so that no row of it
    /// is seeded or claimed without one.
    pub(crate) fn job_periods(&self, xcode_feed: bool) -> Vec<(String, i64)> {
        JOB_DEFAULTS
            .iter()
            .filter(|(name, _)| xcode_feed || *name != XCODE_FEED)
            .map(|(name, secs)| {
                let secs = self
                    .job_every
                    .iter()
                    .rev()
                    .find(|every| every.name == *name)
                    .map_or(*secs, |every| every.secs.get());
                ((*name).to_owned(), i64::try_from(secs).unwrap_or(i64::MAX))
            })
            .collect()
    }

    /// How often the jobs are claimed.
    pub(crate) fn job_tick(&self) -> Duration {
        Duration::from_secs(self.job_tick_secs.get())
    }
}

/// The addresses an instance listens on, as its row of `instances` says them: the heartbeat
/// listener's, the private one's, and the LAN listener's when there is one.
pub(crate) fn listen_of(
    heartbeat: SocketAddr,
    private: SocketAddr,
    lan: Option<SocketAddr>,
) -> String {
    let mut listen = format!("heartbeat={heartbeat} private={private}");
    if let Some(lan) = lan {
        listen.push_str(&format!(" lan={lan}"));
    }
    listen
}

#[cfg(test)]
#[path = "config_database_tests.rs"]
mod tests;
