//! `HostGroups.Move`: an enrolled host into another group, in one call.
//!
//! The host's record in the enrollment store is written first: its group is the new one from
//! that write, and it names the group the host moves from (empty when it was in none), which
//! marks the host as moving. Then the operations state is committed with the audit entry
//! `host_move`: an uninstall intent for each package of the old group that the host listed at
//! its last heartbeat and the new group does not list, in the reverse of the old list's order
//! ([`crate::group_uninstall`]), and the cancelling of the host's intents for the packages the
//! new group lists. The enrollment store and the operations store are two stores: a commit of the
//! second that fails writes the host's old record back, so a refused call changes nothing. When
//! that write back fails too, the call is still refused with the commit's token, and the host
//! stays in the group it was moved to, moving from the old one by its record, with no audit
//! entry, no intent and no new facts generation: its next heartbeat begins the move from the
//! record ([`crate::moving`]). After a commit, every record the fleet keeps of the host is placed
//! in the new group, whose name is the role the fleet records for a host, and the host's move
//! begins afresh: a move under way is replaced, and the admission it had is dropped
//! ([`crate::moving`]).
//!
//! A host in its turn of a maintenance run ([`crate::maintenance_runs`]: it has been ordered the
//! first step and its turn has not ended, halted and not back included) is refused as
//! `host_in_maintenance`, before anything is written and again on the state the change is applied
//! to, since heartbeats start turns under the same lock: a host that only waits for its turn, or
//! whose turn is done, deferred, skipped or not reached, is moved.
//!
//! The audit entry raises the host's facts generation ([`crate::facts`]), once it is committed:
//! the facts the host is sent carry the new group, and the host is ordered nothing after its
//! admission until it reports that generation. A commit that fails raises nothing, and the
//! fleet's lock, held from the first write to the end, keeps the new group out of the facts
//! until the call has ended.
//!
//! The entry names the host (`node_id`) and the new group (`host_group`); its reason is
//! `from <old> to <new>: <reason>`, or `into <new>: <reason>` for a host that was in no group.
//! An empty `host_group` in the request moves the host out of its group into none: it
//! is unassigned and is ordered no package from then on, keeps what it runs, and no uninstall is
//! ordered for the packages of its old group. The record is written with no `moving_from`
//! (there is nothing to install), the audit entry names no group and its reason is `from <old>
//! to no group: <reason>`; a host already in no group is refused as `same_host_group`.
//! Each intent cancelled is an `uninstall_cancelled` entry after it, naming the new group. The sequence that follows
//! the move (admission, the uninstalls, the installs) is [`crate::moving`].

use hostkeeper_enrollment::enroll::{Membership, StoreError};
use hostkeeper_enrollment::names::HostId as EnrolledId;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::HostGroupsRefusal as Refusal;
use hostkeeper_wire::HostId;

use crate::group_uninstall::{
    cancel_all_uninstalls, cancel_uninstalls, listed_by, order_uninstalls,
};
use crate::holds::marks_of;
use crate::host_groups::GroupName;
use crate::inventory::unreadable;
use crate::operator::{Caller, entry_proto, valid_reason};
use crate::ops::{AuditEntry, OpsState, Write};
use crate::service::Coordinator;

/// The audit action of a move; it raises the facts generation of the host it names
/// ([`crate::facts`]).
pub(crate) const ACTION_MOVE: &str = "host_move";

/// What to answer, and log, when a host's group cannot be written.
fn unwritten(why: StoreError) -> Refusal {
    tracing::error!(%why, "a host's group could not be written: nothing changed");
    Refusal::StoreFailed
}

/// Whether `host` is in its turn of a maintenance run: a row of a run holds it at a step that has
/// started and not ended.
fn in_turn(state: &OpsState, host: &str) -> bool {
    state
        .maintenance_runs
        .iter()
        .flat_map(|run| &run.hosts)
        .any(|row| row.host_id == host && row.step.in_turn())
}

impl Coordinator {
    /// `HostGroups.Move`: puts the enrolled host in the group, or in none when the group is empty, as the module says. Audited as
    /// `host_move`.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (the node id, the group's name or the
    /// reason), `unknown_host` (not enrolled, or denied), `unknown_host_group`,
    /// `same_host_group`, `host_in_maintenance`, `store_failed`, `audit_unavailable`.
    pub fn host_groups_move(
        &self,
        caller: &Caller,
        request: proto::MoveHostRequest,
    ) -> Result<proto::MoveHostResponse, Refusal> {
        let by = caller.operator()?;
        let host: HostId = request
            .node_id
            .parse()
            .map_err(|_| Refusal::InvalidRequest)?;
        let to: Option<GroupName> = match request.host_group.as_str() {
            "" => None,
            name => Some(name.parse().map_err(|_| Refusal::InvalidRequest)?),
        };
        let why = valid_reason(&request.reason).ok_or(Refusal::InvalidRequest)?;
        let mut fleet = self.lock_fleet();
        let (enrollment, enrolled) = self
            .enrollment()
            .zip(EnrolledId::parse(host.as_str()).ok())
            .ok_or(Refusal::UnknownHost)?;
        let store = enrollment.store();
        let before = store
            .membership(&enrolled)
            .map_err(unreadable(Refusal::StoreFailed))?
            .ok_or(Refusal::UnknownHost)?;
        let into = match &to {
            Some(to) => Some(
                self.desired
                    .host_group(to)
                    .ok_or(Refusal::UnknownHostGroup)?,
            ),
            None => None,
        };
        let target = to.as_ref().map_or("", GroupName::as_str);
        if before.host_group == target {
            return Err(Refusal::SameHostGroup);
        }
        if self.ops.read(|state| in_turn(state, host.as_str())) {
            return Err(Refusal::HostInMaintenance);
        }
        let from = before
            .host_group
            .parse::<GroupName>()
            .ok()
            .and_then(|name| self.desired.host_group(&name));
        // A host put in a group is ordered the uninstall of what its old group lists and the new
        // one does not; a host put in none is ordered nothing and keeps what it runs.
        let wanted = match &into {
            Some(into) => {
                let leaving = from
                    .iter()
                    .flat_map(|group| group.packages.iter().rev())
                    .filter(|package| !into.packages.contains(package));
                listed_by(&fleet, leaving, std::slice::from_ref(&host))
            }
            None => Vec::new(),
        };
        let nodes = [host.to_string()];
        let mut audit = AuditEntry::new(by, ACTION_MOVE);
        audit.node_id = Some(host.to_string());
        audit.host_group = to.as_ref().map(ToString::to_string);
        audit.reason = Some(match (before.host_group.as_str(), &to) {
            ("", Some(to)) => format!("into {to}: {why}"),
            (from, Some(to)) => format!("from {from} to {to}: {why}"),
            (from, None) => format!("from {from} to no group: {why}"),
        });
        let previous = store
            .set_membership(
                &enrolled,
                Membership {
                    host_group: target.to_owned(),
                    // A host in no group has nothing to install: it does not move.
                    moving_from: to.as_ref().map(|_| before.host_group.clone()),
                },
            )
            .map_err(unwritten)?;
        let done = self
            .ops
            .decide_changes(Write::BeforeMemory, |state| -> Result<_, Refusal> {
                // Another coordinator may have started the host's turn since the read above.
                if in_turn(state, host.as_str()) {
                    return Err(Refusal::HostInMaintenance);
                }
                let mut entries = vec![audit];
                match (&to, &into) {
                    (Some(to), Some(into)) => {
                        entries.extend(cancel_uninstalls(state, to, &into.packages, &nodes));
                    }
                    // An unassigned host is ordered nothing: its pending uninstalls go too.
                    _ => entries.extend(cancel_all_uninstalls(state, &nodes)),
                }
                order_uninstalls(state, &wanted);
                Ok(entries)
            })
            .inspect_err(|_: &Refusal| {
                if let Err(why) = store.set_membership(&enrolled, previous) {
                    tracing::error!(%why, node_id = %host, "a host's group could not be written back: it stays in the group it was moved to");
                }
            })?;
        fleet.set_marks(self.ops.read(marks_of));
        fleet.regroup(&host, target);
        match to {
            Some(to) => fleet.begin_move(host, to),
            None => fleet.drop_move(&host),
        }
        Ok(proto::MoveHostResponse {
            entry: done.first().map(entry_proto),
            from_host_group: before.host_group,
        })
    }
}
