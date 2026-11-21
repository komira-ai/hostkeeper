//! The alerts of the maintenance runs (`docs/design/maintenance-updates.md`, sections 5.4 and
//! 5.7): `maintenance_halted` and `maintenance_not_back`, one each for a host whose turn is stored
//! at that step; the host a turn is silent for, which `silent_host` leaves to them; and the
//! coordinator's note that a rebooting host did not come back ([`Coordinator::note_not_back`]).
//!
//! Both alerts are derived from the ops state's runs alone, so every coordinator over one database
//! derives the same, the book logs each once, and each clears when the stored step moves on: a
//! halted turn when its daemon reports another step (after an operator's `RetryHost` or
//! `PutBack`), a turn not back when the host reports again.

use std::time::Duration;

use hostkeeper_wire::{HostId, Platform};

use super::{Alert, Kind, Subject};
use crate::audit_actions::MAINTENANCE_STEP;
use crate::fleet::Fleet;
use crate::maintenance_effective::effective_of;
use crate::maintenance_runs::{Act, Run, RunHost, Step};
use crate::ops::{AuditEntry, OpsError, OpsState, Write};
use crate::reconcile::COORDINATOR;
use crate::service::Coordinator;

/// The turns of `ops` stored at `step`, each with its run.
fn turns_at(ops: &OpsState, step: Step) -> impl Iterator<Item = (&Run, &RunHost)> {
    ops.maintenance_runs
        .iter()
        .flat_map(|run| run.hosts.iter().map(move |host| (run, host)))
        .filter(move |(_, host)| host.step == step)
}

/// Where the run took the host of the turn `host`, in the words of an alert.
fn place(host: &RunHost) -> String {
    if host.host_group.is_empty() {
        "in no host group".to_owned()
    } else {
        format!("in host group {}", host.host_group)
    }
}

/// `maintenance_halted`: each host whose turn is stored halted, with the run, the group and the
/// reason its daemon gave, and the act queued for it, if one is.
pub(super) fn halted(ops: &OpsState) -> Vec<Alert> {
    turns_at(ops, Step::Halted)
        .map(|(run, turn)| {
            let host = &turn.host_id;
            let said = if turn.reason.is_empty() {
                "its daemon gave no reason".to_owned()
            } else {
                format!("its daemon said {:?}", turn.reason)
            };
            let queued = match turn.queued {
                None => "",
                Some(Act::Retry) => "; a retry is sent to its daemon",
                Some(Act::PutBack) => "; a put back is sent to its daemon",
            };
            Alert {
                subject: Subject {
                    host: host.clone(),
                    group: turn.host_group.clone(),
                    ..Subject::of(Kind::MaintenanceHalted)
                },
                message: format!(
                    "{host} halted in its turn of maintenance run {}, {}: {said}; the host stays \
                     cordoned and no further host of its group starts in the run until an \
                     operator acts with Maintenance.RetryHost or Maintenance.PutBack \
                     (Maintenance.Cancel ends the run but leaves the host halted){queued}",
                    run.id,
                    place(turn)
                ),
            }
        })
        .collect()
}

/// `maintenance_not_back`: each host whose turn is stored not back, with the run, the group and
/// since when.
pub(super) fn not_back(ops: &OpsState) -> Vec<Alert> {
    turns_at(ops, Step::NotBack)
        .map(|(run, turn)| {
            let host = &turn.host_id;
            Alert {
                subject: Subject {
                    host: host.clone(),
                    group: turn.host_group.clone(),
                    ..Subject::of(Kind::MaintenanceNotBack)
                },
                message: format!(
                    "{host} did not come back in its turn of maintenance run {}, {}: it was \
                     rebooting and stayed silent past its group's return_secs, as noted at unix \
                     {}; it stays cordoned (its hold file keeps it so), no further host of its \
                     group starts in the run, and its turn goes on when it reports again",
                    run.id,
                    place(turn),
                    turn.since_unix
                ),
            }
        })
        .collect()
}

/// Whether the host `host` has a turn of any run stored rebooting or not back.
fn away(ops: &OpsState, host: &str) -> bool {
    ops.maintenance_runs
        .iter()
        .flat_map(|run| run.hosts.iter())
        .any(|turn| turn.host_id == host && matches!(turn.step, Step::Rebooting | Step::NotBack))
}

/// `silent_host`: each host heard since the start whose last heartbeat is `silent_after` old
/// or older at `now`, but a host whose turn of a run is stored rebooting or not back: its silence
/// is its reboot, which `maintenance_not_back` follows once it lasts past its group's
/// `return_secs`.
pub(super) fn silent_hosts(
    fleet: &Fleet,
    ops: &OpsState,
    now: Duration,
    silent_after: Duration,
) -> Vec<Alert> {
    fleet
        .nodes()
        .filter(|(id, _)| !away(ops, id.as_str()))
        .filter_map(|(id, node)| {
            let silent = now.saturating_sub(node.seen);
            (silent >= silent_after).then(|| Alert {
                subject: Subject {
                    host: id.to_string(),
                    ..Subject::of(Kind::SilentHost)
                },
                message: format!(
                    "{id} has not heartbeated for {} s (the threshold is {} s)",
                    silent.as_secs(),
                    silent_after.as_secs()
                ),
            })
        })
        .collect()
}

/// An audit entry of the coordinator's, `maintenance_step`, about the host `host`'s turn of the
/// run `run_id` in the group `group` (empty: none), as `service_maintain` words one.
fn not_back_entry(host: &str, group: &str, run_id: &str) -> AuditEntry {
    let mut entry = AuditEntry::new(COORDINATOR, MAINTENANCE_STEP);
    entry.node_id = Some(host.to_owned());
    entry.host_group = Some(group.to_owned()).filter(|group| !group.is_empty());
    entry.reason = Some(format!("run {run_id} {}", Step::NotBack.as_str()));
    entry
}

/// A turn stored rebooting: its run's id, its host's id and its group.
type Rebooting = (String, String, String);

impl Coordinator {
    /// Stores each turn stored rebooting whose host has been silent for its group's `return_secs`
    /// as not back, with the time now, audited `maintenance_step` by the coordinator, all in one
    /// change; each observation of the alerts makes it first, so a turn is marked within one
    /// observation of `return_secs` passing. A host's silence is read on the coordinator's clock:
    /// from its last heartbeat when the fleet holds it, else from the fleet's start, since a
    /// coordinator that has not heard a host since it started knows nothing later of it. A host
    /// whose platform is unknown (not in the fleet) is taken for the platform with the larger
    /// `return_secs`, so it is never marked early. A turn that is no longer rebooting by the time
    /// the change is made is left alone; a change the store refuses is logged, and the next
    /// observation makes it again.
    pub(super) fn note_not_back(&self) {
        let rebooting: Vec<Rebooting> = self.ops.read(|state| {
            turns_at(state, Step::Rebooting)
                .map(|(run, turn)| (run.id.clone(), turn.host_id.clone(), turn.host_group.clone()))
                .collect()
        });
        if rebooting.is_empty() {
            return;
        }
        let now = (self.clock)();
        let fleet = self.lock_fleet();
        let gone: Vec<Rebooting> = rebooting
            .into_iter()
            .filter(|(_, host, group)| {
                let node = host.parse::<HostId>().ok().and_then(|id| fleet.node(&id));
                let table = self.group_policy(group).maintenance;
                let (since, return_secs) = match node {
                    Some(node) => (node.seen, effective_of(&table, node.platform).return_secs),
                    None => (
                        fleet.started(),
                        effective_of(&table, Platform::LinuxX86_64)
                            .return_secs
                            .max(effective_of(&table, Platform::DarwinArm64).return_secs),
                    ),
                };
                now.saturating_sub(since) >= Duration::from_secs(return_secs)
            })
            .collect();
        drop(fleet);
        if gone.is_empty() {
            return;
        }
        let unix = self.ops.unix_now();
        let stored = self.ops.decide_changes(Write::BeforeMemory, |state| {
            let mut audit = Vec::new();
            for (run_id, host, group) in &gone {
                if let Some(held) = state
                    .maintenance_runs
                    .iter_mut()
                    .find(|run| run.id == *run_id)
                    .and_then(|run| run.hosts.iter_mut().find(|turn| turn.host_id == *host))
                    .filter(|turn| turn.step == Step::Rebooting)
                {
                    held.step = Step::NotBack;
                    held.since_unix = unix;
                    audit.push(not_back_entry(host, group, run_id));
                }
            }
            Ok::<_, OpsError>(audit)
        });
        if let Err(why) = stored {
            tracing::warn!(%why, "a host not back in its maintenance turn could not be stored: the next observation notes it again");
        }
    }
}

#[cfg(test)]
#[path = "alerts_maintenance_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "alerts_not_back_tests.rs"]
mod not_back_tests;
