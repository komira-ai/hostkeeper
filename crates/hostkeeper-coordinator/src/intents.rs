//! The uninstall intents at a host's heartbeat: an intent an operator's `Unassign` recorded
//! ([`crate::assignment`]) stands until the host reports the service gone from the services it
//! lists, or until the service is assigned to the host again. Either way the coordinator clears
//! it, as its own change in the ops file and the audit (`uninstalled`, `uninstall_cancelled`).
//! Only an agent that lists every service it runs (`heartbeat-v2`) can report a service gone.
//!
//! An intent whose clear cannot be written stands, and is cleared at the host's next heartbeat;
//! the rollout gate orders no uninstall meanwhile, since the host no longer lists the service or
//! is assigned it again.

use hostkeeper_wire::HostId;

use crate::fleet::Fleet;
use crate::holds::marks_of;
use crate::membership::HEARTBEAT_V2;
use crate::ops::{AuditEntry, OpsStore, Write};
use crate::reconcile::COORDINATOR;
use crate::services::Services;

/// The audit action of an intent cleared because the host reported the service gone.
pub const UNINSTALLED: &str = "uninstalled";

/// The audit action of an intent cleared because the service is assigned to the host again.
pub const CANCELLED: &str = "uninstall_cancelled";

/// Clears the uninstall intents of the host `id` that its heartbeat, just noted in `fleet`, ends,
/// and forgets the host for each service it reported gone ([`Fleet::drop_in`]), so that nothing
/// of it holds a slot. Called under the fleet's lock, after the heartbeat is noted and before
/// any decision; the fleet's marks are refreshed after.
pub(crate) fn settle(ops: &OpsStore, services: &Services, fleet: &mut Fleet, id: &HostId) {
    let lists_all = fleet.announced(id, HEARTBEAT_V2);
    let ended: Vec<(String, &'static str)> = ops.read(|state| {
        state
            .assignments
            .iter()
            .filter(|(_, entries)| entries.uninstall.contains_key(id.as_str()))
            .filter_map(|(service, _)| {
                if services
                    .get(service)
                    .is_some_and(|def| fleet.is_assigned(def, id))
                {
                    Some((service.clone(), CANCELLED))
                } else if lists_all && !fleet.lists(id, service) {
                    Some((service.clone(), UNINSTALLED))
                } else {
                    None
                }
            })
            .collect()
    });
    for (service, action) in ended {
        let mut audit = AuditEntry::new(COORDINATOR, action);
        audit.service = Some(service.clone());
        audit.node_id = Some(id.to_string());
        audit.host_group = services.group().map(ToString::to_string);
        let cleared = ops.change(Write::BeforeMemory, audit, |state| {
            state
                .assignments
                .entry(service.clone())
                .or_default()
                .uninstall
                .remove(id.as_str());
            state.assignments.retain(|_, entries| !entries.is_empty());
        });
        match cleared {
            Ok(_) if action == UNINSTALLED => fleet.drop_in(&service, id),
            Ok(_) => {}
            Err(why) => tracing::error!(
                %why,
                service = %service,
                node_id = %id,
                "an uninstall intent could not be cleared: it stands, and is cleared at the host's next heartbeat"
            ),
        }
    }
    fleet.set_marks(ops.read(marks_of));
}
