//! The decisions of a rollout as transactions on the fleet's rows
//! (`docs/design/coordinator-multi-instance.md`, sections 5.2 and 5.3): an order for a host, the
//! advance of a stage, a halt, and the queue that lets one service roll at a time in the order
//! it was asked for, correct however many coordinators run them at once.
//!
//! **The gate.** Every decision runs in [`decide`]: one transaction that takes the row of
//! `rollout_gate` `FOR UPDATE` first and reads what it decides from, after the lock is granted,
//! from the rows and not from any copy. Two decisions never run side by side, so the second sees
//! the first's order and its slot. A beat asks for the gate with `SKIP LOCKED` ([`Wait::Beat`]): a
//! busy gate answers it at once with no new order, and the host's next beat decides; a beat is
//! never queued, and never passes an operator waiting for the gate. An operator's edit waits for
//! it, up to a time it names ([`Wait::Operator`]). The lock is Postgres's, released with the
//! transaction, so a coordinator that dies or is cut off holds nothing.
//!
//! **The queue.** A service that needs a rollout and has no order out takes a place in
//! `rollout_queue` when a host of it asks, and every ask of it, rolling or waiting, says it is
//! still asking. A place counts for nothing, and is dropped, when its service is halted (it takes
//! its place again, behind, when it asks after the halt clears) and when its service has not asked
//! for [`Limits::queue_idle`] seconds (its rollout is over, its hosts are gone, its version was set
//! back): nothing waits behind a service that no longer asks.
//!
//! **The conditional writes.** What a decision writes is also conditional on what it read, so the
//! step is made once even if two decisions ever reached it with the same inputs: a stage advance
//! is a compare-and-set on the stage the decider saw ([`advance_stage`]), a halt is an insert that
//! does nothing when the service is halted ([`raise_halt`]), and an order is an insert the primary
//! key of the host refuses a second of.
//!
//! Not here, and not built: the per-host plan of stages (`stages.rs`) and the rule order of
//! `decide.rs` still run on the in-memory fleet, and nothing feeds these functions from them yet;
//! the audit lines are written by the caller, in the transaction, when a function answers that it
//! made the step.
// The coordinator's heartbeat does not decide through these yet: its decisions run on the
// in-memory fleet, and `tests/rollout.rs` runs every statement against a server, with two
// sessions deciding at once. The step that feeds them the rows is the one that removes this.
#![allow(
    dead_code,
    reason = "the decisions of the heartbeat still run on the in-memory fleet; no caller outside the tests feeds these functions yet"
)]

use postgres::error::SqlState;

use super::session::Session;

/// How a decision asks for the gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Wait {
    /// A heartbeat: `FOR UPDATE SKIP LOCKED`, never waits.
    Beat,
    /// An operator's edit of a decision's input: waits for the gate, for at most this many
    /// milliseconds (`--operator-gate-wait-ms`); the wait that runs out is the database's error
    /// ([`gate_timed_out`]).
    Operator { timeout_ms: i64 },
}

/// What [`decide`] did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Gated<R> {
    /// The gate was held by another decision: nothing was read or written.
    Busy,
    /// The gate was taken, the decision ran and committed; its answer.
    Decided(R),
}

/// Whether `error` is the wait for the gate running out ([`Wait::Operator`]).
#[must_use]
pub(crate) fn gate_timed_out(error: &postgres::Error) -> bool {
    error.code() == Some(&SqlState::LOCK_NOT_AVAILABLE)
}

/// Runs `body` in one transaction that holds the gate: the transaction's row of `tx_ledger`
/// first, then the gate, then `body`, which reads every input afresh and writes what it decides.
/// A body that fails rolls the transaction back, the gate released with it.
///
/// # Errors
/// The client's error, from the gate, the body or the commit; the wait for the gate that ran out
/// ([`gate_timed_out`]). A commit whose answer was lost is for the writer's ledger to settle.
pub(crate) fn decide<R>(
    session: &mut Session,
    wait: Wait,
    body: impl FnOnce(&mut Session) -> Result<R, postgres::Error>,
) -> Result<Gated<R>, postgres::Error> {
    session.batch("BEGIN")?;
    match take_gate(session, wait).and_then(|held| match held {
        true => body(session).map(Gated::Decided),
        false => Ok(Gated::Busy),
    }) {
        Ok(Gated::Decided(answer)) => {
            session.batch("COMMIT")?;
            Ok(Gated::Decided(answer))
        }
        Ok(Gated::Busy) => {
            session.batch("ROLLBACK")?;
            Ok(Gated::Busy)
        }
        Err(e) => {
            // A session that cannot roll back is gone, and the server ends the transaction.
            let _ = session.batch("ROLLBACK");
            Err(e)
        }
    }
}

fn take_gate(session: &mut Session, wait: Wait) -> Result<bool, postgres::Error> {
    session.execute(
        "INSERT INTO tx_ledger (at_unix) VALUES (extract(epoch FROM now())::bigint)",
        &[],
    )?;
    match wait {
        Wait::Beat => Ok(session
            .query_opt(
                "SELECT 1 FROM rollout_gate WHERE id = 1 FOR UPDATE SKIP LOCKED",
                &[],
            )?
            .is_some()),
        Wait::Operator { timeout_ms } => {
            session.execute(
                "SELECT set_config('lock_timeout', $1::bigint::text || 'ms', true)",
                &[&timeout_ms],
            )?;
            session.query_one("SELECT 1 FROM rollout_gate WHERE id = 1 FOR UPDATE", &[])?;
            Ok(true)
        }
    }
}

/// The host's request for an update slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Ask<'a> {
    pub(crate) host_id: &'a str,
    pub(crate) role: &'a str,
    pub(crate) service: &'a str,
    /// The version to order.
    pub(crate) target: &'a str,
    pub(crate) now: i64,
}

/// The limits an order is judged by (the desired state's, and the configuration's).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Limits {
    /// The most hosts of the role updating at once.
    pub(crate) max_updating: i64,
    /// The fewest hosts of the role that stay up while one updates.
    pub(crate) min_up: i64,
    /// The most services rolling at once (`--live-rollouts`).
    pub(crate) live_rollouts: i64,
    /// How long an order holds its slot unreported (`ORDER_AGE`).
    pub(crate) order_age: i64,
    /// How long a host may be silent before it is not up (`DOWN_AFTER`).
    pub(crate) down_after: i64,
    /// How long a service may go without asking before it leaves its place in the queue: a
    /// rollout that is over, or a service that is gone, frees the places behind it by itself.
    pub(crate) queue_idle: i64,
}

/// What [`admit`] answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Admission {
    /// An order was written for the host.
    Placed,
    /// The host has this order already (an earlier beat, or one whose reply was lost with its
    /// coordinator): the same order is sent again.
    Existing,
    /// The host holds an order for another service or a removal.
    HostBusy,
    /// The fleet's rows are not to be judged yet ([`super::fleet_rows::quiet_until`]).
    Quiet,
    /// The service is halted.
    Halted,
    /// The role's slots are all taken.
    Full,
    /// Fewer hosts than `min_up` would stay up.
    Short,
    /// The most services are rolling, and this one is not one of them.
    Elsewhere,
    /// A service asked earlier waits for the free place.
    Behind,
}

/// What a decision reads of the rows, in one statement, so that one snapshot answers it all: a
/// beat of a host (`fleet_beat`) lands between two statements of a decision, never inside one.
const READ: &str = "\
SELECT \
  (SELECT service FROM fleet_orders WHERE host_id = $1), \
  (SELECT target FROM fleet_orders WHERE host_id = $1), \
  (SELECT uninstall_id IS NULL FROM fleet_orders WHERE host_id = $1), \
  (SELECT until_unix FROM cluster_quiet WHERE id = 1), \
  EXISTS (SELECT 1 FROM halts WHERE service = $2), \
  (SELECT count(*) FROM fleet_orders WHERE service = $2 AND role = $3), \
  (SELECT count(*) FROM fleet_hosts f WHERE f.role = $3 AND f.up AND f.host_id <> $1 \
     AND f.seen_unix > $4::bigint - $5::bigint \
     AND NOT EXISTS (SELECT 1 FROM fleet_orders o WHERE o.host_id = f.host_id)), \
  EXISTS (SELECT 1 FROM fleet_orders WHERE service = $2), \
  (SELECT count(DISTINCT service) FROM fleet_orders), \
  (SELECT count(*) FROM rollout_queue q \
     WHERE q.seq < coalesce((SELECT seq FROM rollout_queue WHERE service = $2), 9223372036854775807) \
     AND NOT EXISTS (SELECT 1 FROM fleet_orders o WHERE o.service = q.service) \
     AND NOT EXISTS (SELECT 1 FROM halts h WHERE h.service = q.service))";

/// Decides whether `ask` gets an order, and writes it when it does. Runs inside [`decide`], with
/// the gate held, from the rows as they are now: the aged orders and the idle places of the
/// queue are dropped, then one statement reads what the decision rests on, and the host's own
/// order, the quiet time, the halt, the role's slots and `min_up`, and, for a service with no
/// order out, its place in the queue are judged in that order.
///
/// # Errors
/// The client's error.
pub(crate) fn admit(
    session: &mut Session,
    ask: &Ask<'_>,
    limits: &Limits,
) -> Result<Admission, postgres::Error> {
    session.execute(
        "DELETE FROM fleet_orders WHERE at_unix <= $1::bigint - $2::bigint",
        &[&ask.now, &limits.order_age],
    )?;
    session.execute(
        "DELETE FROM rollout_queue WHERE asked_unix <= $1::bigint - $2::bigint",
        &[&ask.now, &limits.queue_idle],
    )?;
    let row = session.query_one(
        READ,
        &[
            &ask.host_id,
            &ask.service,
            &ask.role,
            &ask.now,
            &limits.down_after,
        ],
    )?;
    let own: Option<(String, String, bool)> = row
        .get::<_, Option<String>>(0)
        .zip(row.get::<_, Option<String>>(1))
        .zip(row.get::<_, Option<bool>>(2))
        .map(|((service, target), update)| (service, target, update));
    if let Some((service, target, update)) = own {
        return Ok(
            if update && service == ask.service && target == ask.target {
                Admission::Existing
            } else {
                Admission::HostBusy
            },
        );
    }
    if row.get::<_, i64>(3) > ask.now {
        return Ok(Admission::Quiet);
    }
    if row.get::<_, bool>(4) {
        return Ok(Admission::Halted);
    }
    if row.get::<_, i64>(5) >= limits.max_updating {
        return Ok(Admission::Full);
    }
    if row.get::<_, i64>(6) < limits.min_up {
        return Ok(Admission::Short);
    }
    let rolling: bool = row.get(7);
    if rolling {
        // It goes on, and a service that goes on is asking: its place is not idle.
        session.execute(
            "UPDATE rollout_queue SET asked_unix = $2 WHERE service = $1",
            &[&ask.service, &ask.now],
        )?;
    } else {
        // A service with no order out takes its place in the queue, or keeps it, and says it is
        // still asking; it rolls when fewer than `live_rollouts` services are rolling and the
        // services asked before it that are not rolling fill no more than the free places.
        session.execute(
            "INSERT INTO rollout_queue (service, asked_unix) VALUES ($1, $2) \
             ON CONFLICT (service) DO UPDATE SET asked_unix = excluded.asked_unix",
            &[&ask.service, &ask.now],
        )?;
        let live: i64 = row.get(8);
        if live >= limits.live_rollouts {
            return Ok(Admission::Elsewhere);
        }
        let ahead: i64 = row.get(9);
        if ahead >= limits.live_rollouts - live {
            return Ok(Admission::Behind);
        }
    }
    // The primary key on the host is the backstop: under the gate no order of the host can have
    // been written since it was read, and one that was fails the transaction.
    session.execute(
        "INSERT INTO fleet_orders (host_id, service, role, target, at_unix) \
         VALUES ($1, $2, $3, $4, $5)",
        &[&ask.host_id, &ask.service, &ask.role, &ask.target, &ask.now],
    )?;
    Ok(Admission::Placed)
}

/// Moves the plan of `service` (of the group `group`, when it has one) from the stage `from` to
/// the next, if it is at `from` for `version`: true when this call moved it, false when another
/// decider had (or the version changed), which re-decides from the plan as it is. The caller
/// writes the audit line of the advance only when this answers true.
///
/// # Errors
/// The client's error.
pub(crate) fn advance_stage(
    session: &mut Session,
    group: Option<&str>,
    service: &str,
    version: &str,
    from: i64,
) -> Result<bool, postgres::Error> {
    let moved = match group {
        None => session.execute(
            "UPDATE rollouts SET entered = entered + 1 \
             WHERE service = $1 AND version = $2 AND entered = $3",
            &[&service, &version, &from],
        )?,
        Some(group) => session.execute(
            "UPDATE group_rollouts SET entered = entered + 1 \
             WHERE host_group = $1 AND service = $2 AND version = $3 AND entered = $4",
            &[&group, &service, &version, &from],
        )?,
    };
    Ok(moved == 1)
}

/// Halts `service` for a failure `node_id` reported, if it is not halted: true when this call
/// raised the halt (one row, so one audit line however many deciders saw the failure), false when
/// it was halted already.
///
/// # Errors
/// The client's error.
pub(crate) fn raise_halt(
    session: &mut Session,
    service: &str,
    node_id: &str,
    check_name: &str,
    target: &str,
    origin: &str,
) -> Result<bool, postgres::Error> {
    Ok(session.execute(
        "INSERT INTO halts (service, node_id, check_name, target, origin) \
         VALUES ($1, $2, $3, $4, $5) ON CONFLICT (service) DO NOTHING",
        &[&service, &node_id, &check_name, &target, &origin],
    )? == 1)
}
