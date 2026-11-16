//! The fleet's rows (`docs/design/coordinator-multi-instance.md`, section 5.1 and 5.2): what each
//! host last said, how long it has been healthy, the orders that hold a slot, and the start
//! rule's quiet time, as the tables of migration `0020`, so that any number of coordinators
//! hear the beats of one host and agree.
//!
//! A beat is one statement, [`beat`]: it writes the host's row only when the beat is not older
//! than the one stored (a host dials a new connection per beat, so two coordinators can write
//! the beats of one host out of order), advances the host's bakes and frees the slots its report
//! settles, in that statement and only when the beat was written, and, when the table of hosts
//! as the statement saw it is empty or stale as a whole, sets the quiet time that holds every
//! decision back ([`quiet_until`]) until the fleet has been heard again.
//!
//! Nothing here decides: a decision is made under the gate (the next module of the stack). These functions take a session
//! and run one statement each, so they run on any session of a pool.
// The coordinator's heartbeat does not call these yet: the pull request that moves its fleet here
// does, and the tests in `tests/fleet_rows.rs` run every statement against a server until then.
#![allow(
    dead_code,
    reason = "the heartbeat of the coordinator takes its fleet from these rows in the next pull request of the stack"
)]

use super::session::Session;

/// The clock and the rules of one beat, as the configuration keys carry them (section 12).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Timing {
    /// The coordinator's wall clock, seconds since the epoch.
    pub(crate) now: i64,
    /// How long a host may be silent before it is down (`DOWN_AFTER`), and how long a bake's
    /// last beat may be before the count starts again.
    pub(crate) down_after: i64,
    /// How far behind the stored beat a beat may be, in the stored row's age, before the host is
    /// believed again (`--beat-reorder-secs`): a host whose clock stepped back.
    pub(crate) reorder: i64,
    /// The quiet time set by a gap in the beats (`--quiet-secs`).
    pub(crate) quiet: i64,
}

/// What one beat says of one service of the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ServiceBeat<'a> {
    pub(crate) service: &'a str,
    /// The version the host reports it runs.
    pub(crate) target: &'a str,
    /// Whether the beat counts toward a bake: healthy, not updating, no promotion pending.
    pub(crate) counts: bool,
}

/// One heartbeat, as the coordinator that heard it stores it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HostBeat<'a> {
    pub(crate) host_id: &'a str,
    pub(crate) role: &'a str,
    /// The heartbeat message, as received.
    pub(crate) beat: &'a [u8],
    /// The host's own clock in the beat, 0 for an agent that sends none.
    pub(crate) sent_unix: i64,
    /// Whether the host counts as up (live, healthy, not updating, no promotion pending).
    pub(crate) up: bool,
    /// What the host runs, when the beat says it; the stored copy stands when it does not.
    pub(crate) running: Option<&'a [u8]>,
    /// The host's repository report, as `running`.
    pub(crate) repo_report: Option<&'a [u8]>,
    /// The coordinator instance that heard the beat.
    pub(crate) instance: &'a str,
    pub(crate) services: &'a [ServiceBeat<'a>],
}

const BEAT: &str = "\
WITH gap AS (SELECT coalesce(max(seen_unix), 0) AS last FROM fleet_hosts), \
quiet AS (UPDATE cluster_quiet SET until_unix = greatest(until_unix, $3::bigint + $6::bigint) \
  WHERE (SELECT last FROM gap) < $3::bigint - $4::bigint RETURNING 1), \
written AS (INSERT INTO fleet_hosts AS f \
  (host_id, role, beat, seen_unix, sent_unix, up, running, repo_report, instance) \
  VALUES ($1, $2, $7, $3, $8, $9, $10, $11, $12) \
  ON CONFLICT (host_id) DO UPDATE SET role = excluded.role, beat = excluded.beat, \
    seen_unix = excluded.seen_unix, sent_unix = excluded.sent_unix, up = excluded.up, \
    running = coalesce(excluded.running, f.running), \
    repo_report = coalesce(excluded.repo_report, f.repo_report), instance = excluded.instance \
  WHERE excluded.sent_unix >= f.sent_unix OR f.seen_unix < $3::bigint - $5::bigint \
  RETURNING 1), \
svc AS (SELECT service, target, counts FROM unnest($13::text[], $14::text[], $15::bool[]) \
  AS s(service, target, counts)), \
bake_up AS (INSERT INTO fleet_bakes AS b (host_id, service, target, beats, at_unix) \
  SELECT $1, service, target, 1, $3 FROM svc WHERE counts AND EXISTS (SELECT 1 FROM written) \
  ON CONFLICT (host_id, service) DO UPDATE SET \
    beats = CASE WHEN b.target = excluded.target AND b.at_unix > $3::bigint - $4::bigint \
                 THEN least(b.beats, 4294967294) + 1 ELSE 1 END, \
    target = excluded.target, at_unix = excluded.at_unix RETURNING 1), \
bake_end AS (DELETE FROM fleet_bakes b USING svc WHERE b.host_id = $1 AND b.service = svc.service \
  AND NOT svc.counts AND EXISTS (SELECT 1 FROM written) RETURNING 1), \
settled AS (DELETE FROM fleet_orders o USING svc WHERE o.host_id = $1 AND o.service = svc.service \
  AND o.target = svc.target AND o.uninstall_id IS NULL AND EXISTS (SELECT 1 FROM written) \
  RETURNING 1) \
SELECT EXISTS (SELECT 1 FROM written)";

/// Stores `beat` as the host's last, if it is not older than the one stored; true when it was
/// stored, false when it was dropped as an older one (the caller still answers it, from the row
/// that stands).
///
/// # Errors
/// The client's error.
pub(crate) fn beat(
    session: &mut Session,
    beat: &HostBeat<'_>,
    timing: &Timing,
) -> Result<bool, postgres::Error> {
    let services: Vec<&str> = beat.services.iter().map(|s| s.service).collect();
    let targets: Vec<&str> = beat.services.iter().map(|s| s.target).collect();
    let counts: Vec<bool> = beat.services.iter().map(|s| s.counts).collect();
    let row = session.query_one(
        BEAT,
        &[
            &beat.host_id,
            &beat.role,
            &timing.now,
            &timing.down_after,
            &timing.reorder,
            &timing.quiet,
            &beat.beat,
            &beat.sent_unix,
            &beat.up,
            &beat.running,
            &beat.repo_report,
            &beat.instance,
            &services,
            &targets,
            &counts,
        ],
    )?;
    Ok(row.get(0))
}

/// The time before which no decision of the fleet's state is taken (0 for never set).
///
/// # Errors
/// The client's error.
pub(crate) fn quiet_until(session: &mut Session) -> Result<i64, postgres::Error> {
    Ok(session
        .query_one("SELECT until_unix FROM cluster_quiet WHERE id = 1", &[])?
        .get(0))
}

/// The bake of `host_id` for `service`: the version, the healthy beats in a row and the time of
/// the last of them.
///
/// # Errors
/// The client's error.
pub(crate) fn bake(
    session: &mut Session,
    host_id: &str,
    service: &str,
) -> Result<Option<(String, i64, i64)>, postgres::Error> {
    Ok(session
        .query_opt(
            "SELECT target, beats, at_unix FROM fleet_bakes WHERE host_id = $1 AND service = $2",
            &[&host_id, &service],
        )?
        .map(|row| (row.get(0), row.get(1), row.get(2))))
}
