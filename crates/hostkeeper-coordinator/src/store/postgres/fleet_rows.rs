//! The fleet's rows (`docs/design/coordinator-multi-instance.md`, sections 5.1 and 5.2; migration
//! `0024`): a heartbeat this coordinator answers is kept as the host's row of `fleet_hosts`, with
//! its bakes and the orders its report settles, in one statement ([`BEAT`]) run on a writing
//! session outside a transaction ([`super::writer::Writer::once`]), so any instance that hears a
//! host's next beat finds the rows where the last one left them.
//!
//! The statement, in its parts, each seeing the rows as they were before it:
//!
//! - **The gap rule.** The newest `seen_unix` of `fleet_hosts`, read before the beat's own
//!   upsert: when it is [`Timing::down_after`] old or older, or there is no row, the whole fleet is
//!   stale (a database restart, an outage, a first start) and `cluster_quiet.until_unix` is raised
//!   to the beat's moment plus [`Timing::quiet`], with `greatest`, so it never goes back and two
//!   beats that both see the gap write the same time.
//! - **The ordered upsert.** The host's row is replaced only by a beat whose `sent_unix` (the
//!   host's clock) is not older than the stored one, or when the stored row is older than
//!   [`Timing::reorder`] by the database's clock (a host whose clock stepped back is believed
//!   again then). An agent dials a new connection per beat, so two instances can write one host's
//!   beats out of order; a dropped beat changes nothing below, and the heartbeat is answered all
//!   the same. The upsert takes the host's row lock, so two beats of one host are applied one
//!   after the other, the second judged against the first.
//! - **The bake**, only when the beat was applied: for each service of a beat that counts, one
//!   more beat when the stored bake is at the same version and its last beat is younger than
//!   [`Timing::down_after`], else one; the row deleted for a service whose beat does not count.
//!   This is [`crate::bake::Bake::after`] as a `CASE`, tested against it.
//! - **The release of slots**, only when the beat was applied: an update or install order of a
//!   service whose version the beat reports running is done, and its row is deleted. An
//!   uninstall's is not: it ends at its verdict, which the deciding transaction reads.
//!
//! Every time is the database's `now()`; none is the coordinator's. Nothing here decides: the
//! heartbeat's deciding transaction reads the rows under the gate ([`super::decide`]).

use super::session::Session;
use super::writer::WriteFail;
use super::{Postgres, on_writer};
use crate::fleet::DOWN_AFTER;
use crate::store::beat::HostBeat;

/// The longest quiet time the beat statement sets, in seconds: `u32::MAX`, about 136 years.
pub(crate) const MAX_QUIET_SECS: i64 = 4_294_967_295;

/// The times of the beat statement, in seconds (section 12 of the design).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Timing {
    /// How long a host may be silent before it is down ([`DOWN_AFTER`]): a bake whose last beat
    /// is this old starts again, and a fleet whose newest beat is this old is in a gap.
    pub(crate) down_after: i64,
    /// How old the stored beat must be before a beat older by the host's clock replaces it
    /// (`--beat-reorder-secs`).
    pub(crate) reorder: i64,
    /// How long after a gap in the beats nothing is decided (`--quiet-secs`).
    pub(crate) quiet: i64,
}

impl Timing {
    /// The times of `--beat-reorder-secs` and `--quiet-secs`, with [`DOWN_AFTER`]. A quiet time
    /// past [`MAX_QUIET_SECS`] is that many seconds, more than a century: the statement adds it to
    /// the database's clock in a `bigint`, which a larger one would overflow at every gap.
    pub(crate) fn of(reorder_secs: u64, quiet_secs: u64) -> Timing {
        let secs = |secs: u64| i64::try_from(secs).unwrap_or(i64::MAX);
        Timing {
            down_after: secs(DOWN_AFTER.as_secs()),
            reorder: secs(reorder_secs),
            quiet: secs(quiet_secs).min(MAX_QUIET_SECS),
        }
    }
}

/// The beat statement: `$1` host, `$2` role, `$3` beat, `$4` the host's clock, `$5` instance,
/// `$6` [`Timing::down_after`], `$7` [`Timing::reorder`], `$8` [`Timing::quiet`], and the services
/// the beat names as four arrays: name, version, whether the beat counts toward its bake, whether
/// the host runs that version. It answers whether the beat was applied. A column whose name is a
/// word of the hygiene lint is quoted after a dot.
pub(crate) const BEAT: &str = "\
WITH clock AS (SELECT extract(epoch FROM now())::bigint AS now), \
gap AS (SELECT coalesce(max(seen_unix), 0) AS newest FROM fleet_hosts), \
quiet AS (UPDATE cluster_quiet SET until_unix = greatest(until_unix, (SELECT now FROM clock) + $8) \
  WHERE id = 1 AND (SELECT newest FROM gap) <= (SELECT now FROM clock) - $6 RETURNING 1), \
written AS (INSERT INTO fleet_hosts AS f (host_id, role, beat, seen_unix, sent_unix, instance) \
  SELECT $1, $2, $3, now, $4, $5 FROM clock \
  ON CONFLICT (host_id) DO UPDATE SET \"role\" = excluded.\"role\", beat = excluded.\"beat\", \
    seen_unix = excluded.seen_unix, sent_unix = excluded.sent_unix, \
    \"instance\" = excluded.\"instance\" \
  WHERE excluded.sent_unix >= f.sent_unix OR f.seen_unix < excluded.seen_unix - $7 \
  RETURNING 1), \
said AS (SELECT * FROM unnest($9::text[], $10::text[], $11::bool[], $12::bool[]) \
  AS s(said_service, said_version, said_counts, said_runs)), \
baked AS (INSERT INTO fleet_bakes AS b (host_id, service, \"target\", \"beats\", at_unix) \
  SELECT $1, said_service, said_version, 1, (SELECT now FROM clock) FROM said \
  WHERE said_counts AND EXISTS (SELECT 1 FROM written) \
  ON CONFLICT (host_id, service) DO UPDATE SET \
    \"beats\" = CASE WHEN b.\"target\" = excluded.\"target\" AND b.at_unix > excluded.at_unix - $6 \
                   THEN least(b.\"beats\", 2147483646) + 1 ELSE 1 END, \
    \"target\" = excluded.\"target\", at_unix = excluded.at_unix \
  RETURNING 1), \
unbaked AS (DELETE FROM fleet_bakes b USING said \
  WHERE b.host_id = $1 AND b.\"service\" = said_service AND NOT said_counts \
    AND EXISTS (SELECT 1 FROM written) RETURNING 1), \
freed AS (DELETE FROM fleet_orders o USING said \
  WHERE o.host_id = $1 AND o.\"service\" = said_service AND o.\"target\" = said_version \
    AND said_runs AND o.uninstall_id IS NULL AND EXISTS (SELECT 1 FROM written) RETURNING 1) \
SELECT EXISTS (SELECT 1 FROM written)";

/// Runs the beat statement for `beat`, heard by the instance `instance`, on `session`: true when
/// the beat was applied, false when it was dropped as older than the stored one.
///
/// # Errors
/// The client's error.
pub(crate) fn beat(
    session: &mut Session,
    beat: &HostBeat,
    instance: &str,
    timing: &Timing,
) -> Result<bool, postgres::Error> {
    let services: Vec<&str> = beat.services.iter().map(|s| s.service.as_str()).collect();
    let versions: Vec<&str> = beat.services.iter().map(|s| s.version.as_str()).collect();
    let counts: Vec<bool> = beat.services.iter().map(|s| s.counts).collect();
    let runs: Vec<bool> = beat.services.iter().map(|s| s.runs).collect();
    let row = session.query_one(
        BEAT,
        &[
            &beat.host_id,
            &beat.role,
            &beat.beat,
            &beat.sent_unix,
            &instance,
            &timing.down_after,
            &timing.reorder,
            &timing.quiet,
            &services,
            &versions,
            &counts,
            &runs,
        ],
    )?;
    Ok(row.get(0))
}

/// Keeps `beat` in the fleet's rows of `store`, on a writing session of its pool. A beat dropped
/// as older, or one that cannot be written, is logged; the heartbeat is answered all the same.
pub(super) fn keep(store: &Postgres, beat: HostBeat) {
    let host = beat.host_id.clone();
    let (instance, timing) = (store.inner.instance.id.clone(), store.inner.fleet);
    let kept = on_writer(&store.inner.writers, &store.inner.link, move |writer| {
        writer.once(move |session| self::beat(session, &beat, &instance, &timing))
    });
    match kept {
        Ok(true) => {}
        Ok(false) => {
            tracing::debug!(node_id = %host, "a beat older than the stored one is not kept");
        }
        // The store says it is lost, and a heartbeat revives it: no word for each beat.
        Err(WriteFail::Lost(why)) => {
            tracing::debug!(node_id = %host, %why, "a beat is not kept: the store is lost");
        }
        Err(fail) => {
            tracing::warn!(node_id = %host, why = fail.why(), "a beat is not kept");
        }
    }
}
