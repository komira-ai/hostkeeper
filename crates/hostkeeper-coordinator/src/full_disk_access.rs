//! What a Mac's last gRPC heartbeat said of its Full Disk Access grant
//! (`HeartbeatRequest.full_disk_access`): whether its daemon could list the directory only the
//! grant allows. Kept per host in memory with the fleet, and shown by `Hosts.Get` and
//! `Hosts.List` as `Host.full_disk_access` ([`show`]).
//!
//! A host that reports `NOT_GRANTED` or `UNDETERMINED` is shown as that: nothing is hidden or
//! turned into an alert. A heartbeat from a daemon that predates the field says `UNSPECIFIED`,
//! and so does one that carries a state this coordinator does not know (a value a later daemon
//! sends); both are shown as unknown, never as granted. The JSON route is frozen and carries
//! none, so a host that beats only over it is unknown. Each gRPC heartbeat replaces what the
//! one before said. Like the rest of a host's last heartbeat, the state is not stored: a
//! restarted coordinator shows none until the host's next heartbeat.

use std::collections::BTreeMap;
use std::sync::PoisonError;

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as operator;
use hostkeeper_wire::HostId;

use crate::fleet::Fleet;
use crate::identity::Identity;
use crate::service::Coordinator;

/// The state each host's last gRPC heartbeat said; a host with none kept is unknown.
#[derive(Clone, Debug, Default)]
pub(crate) struct Reported(BTreeMap<HostId, agent::FullDiskAccess>);

impl Reported {
    /// Forgets the host `id`'s state, when the fleet forgets the host.
    pub(crate) fn forget(&mut self, id: &HostId) {
        self.0.remove(id);
    }
}

impl Coordinator {
    /// Notes the state a heartbeat over gRPC from `who` carries, replacing the one before: a
    /// heartbeat that says nothing (or a state this coordinator does not know) leaves the host
    /// unknown.
    pub(crate) fn hear_full_disk_access(&self, who: &Identity, request: &agent::HeartbeatRequest) {
        let state = request.full_disk_access();
        let mut fleet = self.fleet.lock().unwrap_or_else(PoisonError::into_inner);
        if state == agent::FullDiskAccess::Unspecified {
            fleet.full_disk_access.forget(&who.host_id);
        } else {
            fleet.full_disk_access.0.insert(who.host_id.clone(), state);
        }
    }
}

/// Fills in `host`, as `Hosts.List` and `Hosts.Get` made it for the host `id`, with the state its
/// last gRPC heartbeat said; unknown (`UNSPECIFIED`) when it said none.
pub(crate) fn show(fleet: &Fleet, id: &HostId, host: &mut operator::Host) {
    host.full_disk_access = fleet
        .full_disk_access
        .0
        .get(id)
        .copied()
        .unwrap_or(agent::FullDiskAccess::Unspecified)
        .into();
}

#[cfg(test)]
#[path = "full_disk_access_tests.rs"]
mod tests;
