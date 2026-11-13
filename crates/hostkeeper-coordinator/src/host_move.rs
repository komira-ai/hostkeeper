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
//! The audit entry raises the host's facts generation ([`crate::facts`]), once it is committed:
//! the facts the host is sent carry the new group, and the host is ordered nothing after its
//! admission until it reports that generation. A commit that fails raises nothing, and the
//! fleet's lock, held from the first write to the end, keeps the new group out of the facts
//! until the call has ended.
//!
//! The entry names the host (`node_id`) and the new group (`host_group`); its reason is
//! `from <old> to <new>: <reason>`, or `into <new>: <reason>` for a host that was in no group.
//! An empty `host_group` in the request moves the host out of its specific group into none: it
//! then runs the packages of `all` alone. The record is written with no `moving_from` (there is
//! nothing to install), the uninstall intents are those of the packages the old group lists and
//! `all` does not, the audit entry names no group and its reason is `from <old> to no group:
//! <reason>`; a host already in no group is refused as `same_host_group`.
//! Each intent cancelled is an `uninstall_cancelled` entry after it, naming the new group. The sequence that follows
//! the move (admission, the uninstalls, the installs) is [`crate::moving`].

use hostkeeper_enrollment::enroll::{Membership, StoreError};
use hostkeeper_enrollment::names::HostId as EnrolledId;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::HostGroupsRefusal as Refusal;
use hostkeeper_wire::HostId;

use crate::group_uninstall::{cancel_uninstalls, lost_by, order_uninstalls, runs};
use crate::holds::marks_of;
use crate::host_groups::GroupName;
use crate::inventory::unreadable;
use crate::operator::{Caller, entry_proto, valid_reason};
use crate::ops::{AuditEntry, OpsError, Write};
use crate::service::Coordinator;

/// The audit action of a move; it raises the facts generation of the host it names
/// ([`crate::facts`]).
pub(crate) const ACTION_MOVE: &str = "host_move";

/// What to answer, and log, when a host's group cannot be written.
fn unwritten(why: StoreError) -> Refusal {
    tracing::error!(%why, "a host's group could not be written: nothing changed");
    Refusal::StoreFailed
}

impl Coordinator {
    /// `HostGroups.Move`: puts the enrolled host in the group, or in none when the group is empty, as the module says. Audited as
    /// `host_move`.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (the node id, the group's name or the
    /// reason), `unknown_host` (not enrolled, or denied), `unknown_host_group`,
    /// `same_host_group`, `store_failed`, `audit_unavailable`.
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
        let groups = self.desired.host_groups();
        if let Some(to) = &to {
            if to.is_all() {
                return Err(Refusal::ReservedHostGroup);
            }
            if !groups.0.contains_key(to) {
                return Err(Refusal::UnknownHostGroup);
            }
        }
        let target = to.as_ref().map_or("", GroupName::as_str);
        if before.host_group == target {
            return Err(Refusal::SameHostGroup);
        }
        let from = before.host_group.parse::<GroupName>().ok();
        let wanted = lost_by(
            &fleet,
            &groups,
            &groups,
            &[(host.clone(), from, to.clone())],
        );
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
                    // A host in no specific group has nothing to install: it does not move.
                    moving_from: to.as_ref().map(|_| before.host_group.clone()),
                },
            )
            .map_err(unwritten)?;
        let done = self
            .ops
            .decide_changes(Write::BeforeMemory, |state| -> Result<_, OpsError> {
                let mut entries = vec![audit];
                let own = to.clone().unwrap_or_else(GroupName::all);
                entries.extend(cancel_uninstalls(
                    state,
                    &own,
                    &runs(&groups, to.as_ref()),
                    &nodes,
                ));
                order_uninstalls(state, &wanted);
                Ok(entries)
            })
            .map_err(|refused| {
                if let Err(why) = store.set_membership(&enrolled, previous) {
                    tracing::error!(%why, node_id = %host, "a host's group could not be written back: it stays in the group it was moved to");
                }
                Refusal::from(refused)
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
