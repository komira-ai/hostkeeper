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
//! **The conditional writes.** What a decision writes is also conditional on what it read, so the
//! step is made once even if two decisions ever reached it with the same inputs: a stage advance
//! is a compare-and-set on the stage the decider saw ([`advance_stage`]), a halt is an insert that
//! does nothing when the service is halted ([`raise_halt`]), and an order is an insert the primary
//! key of the host refuses a second of.
//!
//! Not here: the per-host plan of stages (`stages.rs`) and the rule order of `decide.rs` are the
//! pure code that feeds these functions in the next pull request; the audit lines are written by
//! the caller, in the transaction, when a function answers that it made the step.
// The coordinator's heartbeat does not call these yet; `tests/rollout.rs` runs every statement
// against a server, with two sessions deciding at once, until it does.
#![allow(
    dead_code,
    reason = "the heartbeat of the coordinator decides through these functions in the next pull request of the stack"
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

/// Decides whether `ask` gets an order, and writes it when it does. Runs inside [`decide`], with
/// the gate held, from the rows as they are now: the aged orders are dropped, then the host's
/// own order, the quiet time, the halt, the role's slots and `min_up`, and, for a service with no
/// order out, its place in the queue.
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
    if let Some(order) = session.query_opt(
        "SELECT service, target, uninstall_id IS NULL FROM fleet_orders WHERE host_id = $1",
        &[&ask.host_id],
    )? {
        let (service, target, update): (String, String, bool) =
            (order.get(0), order.get(1), order.get(2));
        return Ok(
            if update && service == ask.service && target == ask.target {
                Admission::Existing
            } else {
                Admission::HostBusy
            },
        );
    }
    let quiet: i64 = session
        .query_one("SELECT until_unix FROM cluster_quiet WHERE id = 1", &[])?
        .get(0);
    if quiet > ask.now {
        return Ok(Admission::Quiet);
    }
    if session
        .query_opt("SELECT 1 FROM halts WHERE service = $1", &[&ask.service])?
        .is_some()
    {
        return Ok(Admission::Halted);
    }
    let taken: i64 = session
        .query_one(
            "SELECT count(*) FROM fleet_orders WHERE service = $1 AND role = $2",
            &[&ask.service, &ask.role],
        )?
        .get(0);
    if taken >= limits.max_updating {
        return Ok(Admission::Full);
    }
    let staying: i64 = session
        .query_one(
            "SELECT count(*) FROM fleet_hosts f WHERE f.role = $1 AND f.up AND f.host_id <> $2 \
             AND f.seen_unix > $3::bigint - $4::bigint \
             AND NOT EXISTS (SELECT 1 FROM fleet_orders o WHERE o.host_id = f.host_id)",
            &[&ask.role, &ask.host_id, &ask.now, &limits.down_after],
        )?
        .get(0);
    if staying < limits.min_up {
        return Ok(Admission::Short);
    }
    if let Some(refused) = queue_refuses(session, ask.service, limits)? {
        return Ok(refused);
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

/// Why the queue refuses an order for `service`, if it does: a service that has an order out is
/// rolling and goes on; any other takes its place in the queue (kept when it is already there)
/// and rolls when fewer than `live_rollouts` services are rolling and the services asked before
/// it that are not rolling fill no more than the free places.
fn queue_refuses(
    session: &mut Session,
    service: &str,
    limits: &Limits,
) -> Result<Option<Admission>, postgres::Error> {
    let rolling: bool = session
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM fleet_orders WHERE service = $1)",
            &[&service],
        )?
        .get(0);
    if rolling {
        return Ok(None);
    }
    session.execute(
        "INSERT INTO rollout_queue (service) VALUES ($1) ON CONFLICT (service) DO NOTHING",
        &[&service],
    )?;
    let live: i64 = session
        .query_one("SELECT count(DISTINCT service) FROM fleet_orders", &[])?
        .get(0);
    if live >= limits.live_rollouts {
        return Ok(Some(Admission::Elsewhere));
    }
    let ahead: i64 = session
        .query_one(
            "SELECT count(*) FROM rollout_queue q WHERE q.seq < \
             (SELECT seq FROM rollout_queue WHERE service = $1) \
             AND NOT EXISTS (SELECT 1 FROM fleet_orders o WHERE o.service = q.service)",
            &[&service],
        )?
        .get(0);
    Ok((ahead >= limits.live_rollouts - live).then_some(Admission::Behind))
}

/// Puts `service` in the queue if it is not there; its place (the number from `desired_seq`)
/// either way. Called in the transaction that sets the desired version that needs a rollout.
///
/// # Errors
/// The client's error.
pub(crate) fn enqueue(session: &mut Session, service: &str) -> Result<i64, postgres::Error> {
    session.execute(
        "INSERT INTO rollout_queue (service) VALUES ($1) ON CONFLICT (service) DO NOTHING",
        &[&service],
    )?;
    Ok(session
        .query_one(
            "SELECT seq FROM rollout_queue WHERE service = $1",
            &[&service],
        )?
        .get(0))
}

/// Takes `service` out of the queue: its rollout is over. True when it was there.
///
/// # Errors
/// The client's error.
pub(crate) fn finish(session: &mut Session, service: &str) -> Result<bool, postgres::Error> {
    Ok(session.execute("DELETE FROM rollout_queue WHERE service = $1", &[&service])? == 1)
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
