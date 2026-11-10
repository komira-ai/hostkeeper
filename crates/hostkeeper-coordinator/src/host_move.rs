//! `HostGroups.Move`: an enrolled host into another group, in one call.
//!
//! The host's record in the enrollment store is written first: its group is the new one from
//! that write, and it names the group the host moves from (empty when it was in none), which
//! marks the host as moving. Then the operations state is committed with the audit entry
//! `host_move`: an uninstall intent for each package of the old group that the host listed at
//! its last heartbeat and the new group does not list, in the reverse of the old list's order
//! ([`crate::group_uninstall`]), and the cancelling of the host's intents for the packages the
//! new group lists. The enrollment store and the operations store are two stores: a commit of the
//! second that fails writes the host's old record back, so a refused call changes nothing. The
//! host's node in the fleet is placed in the new group, whose name is the role the fleet records
//! for a host.
//!
//! The entry names the host (`node_id`) and the new group (`host_group`); its reason is
//! `from <old> to <new>: <reason>`, or `into <new>: <reason>` for a host that was in no group.
//! Each intent cancelled is an `uninstall_cancelled` entry after it. The sequence that follows
//! the move (admission, the uninstalls, the installs) is not here.

use hostkeeper_enrollment::enroll::{Membership, StoreError};
use hostkeeper_enrollment::names::HostId as EnrolledId;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::HostGroupsRefusal as Refusal;
use hostkeeper_wire::HostId;

use crate::fleet::Node;
use crate::group_uninstall::{cancel_uninstalls, listed_by, order_uninstalls};
use crate::holds::marks_of;
use crate::host_groups::GroupName;
use crate::inventory::unreadable;
use crate::operator::{Caller, entry_proto, valid_reason};
use crate::ops::{AuditEntry, OpsError, Write};
use crate::service::Coordinator;

/// What to answer, and log, when a host's group cannot be written.
fn unwritten(why: StoreError) -> Refusal {
    tracing::error!(%why, "a host's group could not be written: nothing changed");
    Refusal::StoreFailed
}

impl Coordinator {
    /// `HostGroups.Move`: puts the enrolled host in the group, as the module says. Audited as
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
        let to: GroupName = request
            .host_group
            .parse()
            .map_err(|_| Refusal::InvalidRequest)?;
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
        let into = self
            .desired
            .host_group(&to)
            .ok_or(Refusal::UnknownHostGroup)?;
        if before.host_group == to.as_str() {
            return Err(Refusal::SameHostGroup);
        }
        let from = before
            .host_group
            .parse::<GroupName>()
            .ok()
            .and_then(|name| self.desired.host_group(&name));
        let leaving = from
            .iter()
            .flat_map(|group| group.packages.iter().rev())
            .filter(|package| !into.packages.contains(package));
        let wanted = listed_by(&fleet, leaving, std::slice::from_ref(&host));
        let nodes = [host.to_string()];
        let mut audit = AuditEntry::new(by, "host_move");
        audit.node_id = Some(host.to_string());
        audit.host_group = Some(to.to_string());
        audit.reason = Some(if before.host_group.is_empty() {
            format!("into {to}: {why}")
        } else {
            format!("from {} to {to}: {why}", before.host_group)
        });
        let previous = store
            .set_membership(
                &enrolled,
                Membership {
                    host_group: to.to_string(),
                    moving_from: Some(before.host_group.clone()),
                },
            )
            .map_err(unwritten)?;
        let done = self
            .ops
            .decide_changes(Write::BeforeMemory, |state| -> Result<_, OpsError> {
                let mut entries = vec![audit];
                entries.extend(cancel_uninstalls(state, &into.packages, &nodes));
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
        if let Some(node) = fleet.node(&host).cloned() {
            let at = node.sent_unix.unwrap_or_default();
            let moved = Node {
                role: to.to_string(),
                ..node
            };
            fleet.note_node(host, moved, at);
        }
        Ok(proto::MoveHostResponse {
            entry: done.first().map(entry_proto),
            from_host_group: before.host_group,
        })
    }
}
