//! A heartbeat's maintenance ([`crate::decide_maintenance`]): the bake of a host whose turn is
//! done, counted from the heartbeat before its report is read; the host's turn of a run as its
//! daemon reports it, stored before anything is decided; then the `maintain` the host is sent, or
//! the package orders it is not given.
//!
//! The host's `macos` left out by its report ([`decide_maintenance::Verdict::Skip`]) is stored the
//! same way, in place of an order.
//!
//! Each is a coordinator's change of the ops state, made under the fleet's lock and, over a
//! shared store, in the heartbeat's decision transaction, so a decision that does not land puts
//! them back with the rest. A step, a start and a skip are audited by `coordinator`
//! (`maintenance_step`, `maintenance_start`); a bake's count is stored at each heartbeat with no
//! entry of its own, and the heartbeat that ends it is audited `maintenance_step` (`baked`). A
//! start is stored before the order is answered (storage first): a start that cannot
//! be stored sends no order, and the host is asked again at its next heartbeat.

use std::collections::BTreeMap;
use std::time::Duration;

use hostkeeper_wire::HostId;

use super::Coordinator;
use crate::audit_actions::{MAINTENANCE_START, MAINTENANCE_STEP};
use crate::decide_maintenance::{self, Ordered, Skipping, Verdict, View};
use crate::fleet::Fleet;
use crate::holds::marks_of;
use crate::host_groups::GroupName;
use crate::host_groups::Policy;
use crate::maintenance::FEATURE;
use crate::maintenance_runs::{Kind, Step};
use crate::ops::{AuditEntry, OpsError, Write};
use crate::orders::Order;
use crate::reconcile::COORDINATOR;

/// What the maintenance of one heartbeat leaves the package orders.
pub(super) enum Next {
    /// The host is in no turn, or waits: packages are decided as ever.
    Free,
    /// The host is in its turn and is sent nothing: no package order either.
    Busy,
    /// Send the host this `maintain`, and nothing else.
    Order(Order),
}

/// An audit entry of the coordinator's, `action`, about the host `id`'s turn of the run `run_id`
/// in the group `group` (empty: none), with `then` after the run in its reason.
fn entry(action: &str, id: &HostId, group: &str, run_id: &str, then: &str) -> AuditEntry {
    let mut entry = AuditEntry::new(COORDINATOR, action);
    entry.node_id = Some(id.to_string());
    entry.host_group = Some(group.to_owned()).filter(|group| !group.is_empty());
    entry.reason = Some(format!("run {run_id}{then}"));
    entry
}

impl Coordinator {
    /// Stores the host `id`'s turn as its daemon's last kept report says it, when that differs
    /// from what the run holds ([`decide_maintenance::folded`]), audited `maintenance_step`, and
    /// refreshes the marks of `fleet` from it, so the packages of this heartbeat are decided over
    /// the turn as stored ([`crate::fleet::Marks::in_maintenance`]): a host whose turn ended here
    /// takes its place in its plans again at once. A change that cannot be stored is logged, and
    /// the next heartbeat's report folds it again.
    pub(super) fn fold_maintenance(&self, fleet: &mut Fleet, id: &HostId) {
        let unix_now = self.ops.unix_now();
        let Some((r, h, turn, run_id)) = self.ops.read(|state| {
            let runs = &state.maintenance_runs;
            decide_maintenance::folded(runs, fleet.node(id), id, unix_now)
                .map(|(r, h, turn)| (r, h, turn, runs[r].id.clone()))
        }) else {
            return;
        };
        let then = format!(" {}", turn.step.as_str());
        let audit = entry(MAINTENANCE_STEP, id, &turn.host_group, &run_id, &then);
        let stored = self.ops.change(Write::BeforeMemory, audit, |state| {
            if let Some(held) = state
                .maintenance_runs
                .get_mut(r)
                .filter(|run| run.id == run_id)
                .and_then(|run| run.hosts.get_mut(h))
            {
                *held = turn;
            }
        });
        match stored {
            Ok(_) => fleet.set_marks(self.ops.read(marks_of)),
            Err(why) => {
                tracing::warn!(%why, node_id = %id, run = %run_id, "a host's maintenance step could not be stored: its next heartbeat reports it again");
            }
        }
    }

    /// The group policy of the group named `group` (empty: none), or every default.
    fn group_policy(&self, group: &str) -> Policy {
        group
            .parse::<GroupName>()
            .ok()
            .and_then(|name| self.desired.host_group(&name))
            .map_or_else(Policy::default, |held| held.policy)
    }

    /// Counts the host `id`'s heartbeat toward the bake of a done turn of it
    /// ([`decide_maintenance::baked`]): the count is stored with no audit entry of its own, and
    /// the heartbeat that completes the bake, which frees the host's batch place, is audited
    /// `maintenance_step` with `baked` after the run. A count that cannot be stored is logged; the
    /// next healthy heartbeat counts again from what is held.
    pub(super) fn bake_maintenance(&self, fleet: &Fleet, id: &HostId) {
        let need = |group: &str| self.group_policy(group).bake_beats;
        let counts: Vec<(usize, usize, u32, String, String, bool)> = self.ops.read(|state| {
            let runs = &state.maintenance_runs;
            decide_maintenance::baked(runs, fleet.node(id), id, need)
                .into_iter()
                .map(|(r, h, beats)| {
                    let host = &runs[r].hosts[h];
                    let done = beats >= need(&host.host_group);
                    (
                        r,
                        h,
                        beats,
                        runs[r].id.clone(),
                        host.host_group.clone(),
                        done,
                    )
                })
                .collect()
        });
        if counts.is_empty() {
            return;
        }
        let stored = self.ops.decide_changes(Write::BeforeMemory, |state| {
            let mut audit = Vec::new();
            for (r, h, beats, run_id, group, done) in &counts {
                if let Some(held) = state
                    .maintenance_runs
                    .get_mut(*r)
                    .filter(|run| run.id == *run_id)
                    .and_then(|run| run.hosts.get_mut(*h))
                    .filter(|host| host.step == Step::Done)
                {
                    held.baked_beats = *beats;
                    if *done {
                        audit.push(entry(MAINTENANCE_STEP, id, group, run_id, " baked"));
                    }
                }
            }
            Ok::<_, crate::ops::OpsError>(audit)
        });
        if let Err(why) = stored {
            tracing::warn!(%why, node_id = %id, "a host's maintenance bake could not be stored: its next healthy heartbeat counts again");
        }
    }

    /// The maintenance of the heartbeat of the host `id` at `now`, decided over `fleet`: a
    /// `maintain` only over a transport that carries typed orders (`orders`) to a daemon that
    /// announced `maintenance-v1` (a lost store is a quiet start, which starts no turn).
    pub(super) fn maintenance_next(
        &self,
        fleet: &Fleet,
        id: &HostId,
        orders: bool,
        now: Duration,
    ) -> Next {
        let unix_now = self.ops.unix_now();
        let Some((group, waits)) = self
            .ops
            .read(|state| decide_maintenance::turn_group(state, fleet, id, unix_now))
        else {
            return Next::Free;
        };
        let policy = self.group_policy(&group);
        let groups: BTreeMap<String, String> = if waits {
            match self.memberships() {
                Ok(all) => all
                    .into_iter()
                    .map(|(host, membership)| (host, membership.host_group))
                    .collect(),
                Err(_) => {
                    tracing::warn!(node_id = %id, "the hosts' groups cannot be read: no maintenance turn starts at this heartbeat");
                    return Next::Free;
                }
            }
        } else {
            BTreeMap::new()
        };
        let verdict = self.ops.read(|state| {
            let view = View {
                state,
                fleet,
                groups: &groups,
                policy: &policy.maintenance,
                bake_beats: policy.bake_beats,
                defer_backoff: policy.defer_backoff,
                unix_now,
                now,
            };
            decide_maintenance::decide(&view, id)
        });
        let sends = orders && fleet.announced(id, FEATURE);
        match verdict {
            Verdict::Free => Next::Free,
            Verdict::Waits(hold) => {
                tracing::debug!(node_id = %id, ?hold, "a host's maintenance turn waits");
                Next::Free
            }
            Verdict::InTurn { resend: None } => Next::Busy,
            Verdict::InTurn {
                resend: Some(ordered),
            } if sends => Next::Order(order_of(ordered)),
            Verdict::InTurn { resend: Some(_) } => Next::Busy,
            Verdict::Start(ordered) if sends => self.start_turn(id, ordered, unix_now),
            Verdict::Start(_) => Next::Free,
            Verdict::Skip(skipping) => {
                self.skip_macos(id, &skipping, unix_now);
                Next::Free
            }
        }
    }

    /// Stores the `macos` of the host `id`'s turn that waits, or deferred and is offered again,
    /// left out at `unix_now`, with why ([`Verdict::Skip`]), audited `maintenance_step`: the turn
    /// goes on without `macos` at its step, or, when `macos` was all it held, ends `skipped` with
    /// its kinds as they were. Nothing is stored when the turn no longer waits, or is deferred,
    /// with `macos` in a run that is not cancelled (an operator's `Cancel` in between); a change the store refuses is logged, and the host's next heartbeat
    /// decides it again.
    pub(crate) fn skip_macos(&self, id: &HostId, skipping: &Skipping, unix_now: u64) {
        let run_id = skipping.run_id.as_str();
        let why = skipping.why;
        let stored = self.ops.decide_change(Write::BeforeMemory, |state| {
            let held = state
                .maintenance_runs
                .iter_mut()
                .filter(|run| run.id == run_id && run.cancelled_unix.is_none())
                .flat_map(|run| run.hosts.iter_mut())
                .find(|host| {
                    host.host_id == id.as_str()
                        && host.step.may_start()
                        && host.kinds.contains(&Kind::Macos)
                })
                .ok_or(Unstarted::Gone)?;
            held.skipped_because = Some(why);
            if held.kinds == [Kind::Macos] {
                held.step = Step::Skipped;
                held.since_unix = unix_now;
            } else {
                held.kinds.retain(|kind| *kind != Kind::Macos);
            }
            let then = format!(" {} {}", held.step.as_str(), why.as_str());
            Ok::<_, Unstarted>(entry(
                MAINTENANCE_STEP,
                id,
                &skipping.host_group,
                run_id,
                &then,
            ))
        });
        if let Err(Unstarted::Store(why)) = stored {
            tracing::warn!(%why, node_id = %id, run = %run_id, "a host's macOS skip could not be stored: its next heartbeat decides it again");
        }
    }

    /// Stores the start of the host `id`'s turn decided as `ordered` at `unix_now` (its step
    /// staging, audited `maintenance_start`; a deferred host's offer counted, its reason and the
    /// kinds it applied cleared) and answers its order. Nothing is stored or sent when the store
    /// refuses the change, or when the turn no longer waits, or is deferred, under the order id
    /// decided, in a run that is not cancelled by the time it is stored (an operator's `Cancel`, or
    /// another coordinator's start, in between).
    pub(super) fn start_turn(&self, id: &HostId, ordered: Ordered, unix_now: u64) -> Next {
        let run_id = ordered.run_id.clone();
        let audit = entry(MAINTENANCE_START, id, &ordered.host_group, &run_id, "");
        let stored = self.ops.try_change(Write::BeforeMemory, audit, |state| {
            let held = state
                .maintenance_runs
                .iter_mut()
                .filter(|run| run.id == run_id && run.cancelled_unix.is_none())
                .flat_map(|run| run.hosts.iter_mut())
                .find(|host| {
                    host.host_id == id.as_str()
                        && host.step.may_start()
                        && host.next_order_id(&run_id) == ordered.order_id
                })
                .ok_or(Unstarted::Gone)?;
            if held.step == Step::Deferred {
                held.offered_again = held.offered_again.saturating_add(1);
                held.reason.clear();
                held.applied.clear();
            }
            held.step = Step::Staging;
            held.since_unix = unix_now;
            held.baked_beats = 0;
            Ok(())
        });
        match stored {
            Ok(_) => Next::Order(order_of(ordered)),
            Err(Unstarted::Gone) => Next::Busy,
            Err(Unstarted::Store(why)) => {
                tracing::warn!(%why, node_id = %id, run = %run_id, "a host's maintenance turn could not be stored: it is not started at this heartbeat");
                Next::Busy
            }
        }
    }
}

/// Why a start, or a skip of `macos`, was not stored.
enum Unstarted {
    /// The turn no longer waits in a run that is not cancelled (with `macos`, for a skip).
    Gone,
    /// The store refused the change.
    Store(OpsError),
}

impl From<OpsError> for Unstarted {
    fn from(why: OpsError) -> Self {
        Self::Store(why)
    }
}

/// The typed order of `ordered`: its first try, which a resend keeps.
fn order_of(ordered: Ordered) -> Order {
    Order::Maintain {
        maintain: ordered.maintain,
        order_id: ordered.order_id,
        order_try: 1,
    }
}

#[cfg(test)]
#[path = "service_maintain_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "maintenance_turns_tests.rs"]
mod turns_tests;

#[cfg(test)]
#[path = "maintenance_packages_tests.rs"]
mod packages_tests;
