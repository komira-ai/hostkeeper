//! What a daemon reports of the certificates of its services with an identity
//! (`HeartbeatRequest.service_identities`), kept in memory with the fleet and shown in the host's
//! heartbeat by `Hosts.List` and `Hosts.Get` ([`show`]). Nothing decides on it.
//!
//! The list is read from every heartbeat over gRPC, whatever features it announces (a daemon that
//! predates it never sets it, so it is empty); a heartbeat over the legacy path leaves what is kept
//! as it is. A list
//! is good when it names at most [`MAX_SERVICES`] services, each a service name, none twice; each
//! `last_error` is cut to [`MAX_ERROR`] bytes at a character boundary. A list that breaks a rule is
//! counted and logged as a dropped fact ([`FACT`]), and the one read before is kept. A good list
//! replaces the one kept, so a daemon that no longer has a service with an identity (an empty list)
//! shows none. After a restart the coordinator holds none until the host's next heartbeat, which
//! carries it again.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::PoisonError;

use hostkeeper_enrollment::names::ServiceName;
use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as operator;
use hostkeeper_wire::HostId;

use crate::fleet::Fleet;
use crate::identity::Identity;
use crate::repository_push::cut;
use crate::service::Coordinator;

/// The name a list that breaks a rule is counted under.
pub(crate) const FACT: &str = "service_identities";

/// The most services a heartbeat reports.
pub(crate) const MAX_SERVICES: usize = 16;

/// The most bytes of a `last_error` kept.
pub(crate) const MAX_ERROR: usize = 256;

/// The last good list of each host that reported one with a service in it.
#[derive(Clone, Debug, Default)]
pub(crate) struct Identities(BTreeMap<HostId, Vec<agent::ServiceIdentityReport>>);

impl Identities {
    /// Forgets the host `id`'s list, when the fleet forgets the host.
    pub(crate) fn forget(&mut self, id: &HostId) {
        self.0.remove(id);
    }
}

/// `said` as the list the coordinator keeps: each error cut to [`MAX_ERROR`] bytes; `None` when
/// it names more than [`MAX_SERVICES`] services, a name that is no service name, or a service
/// twice.
fn kept(said: &[agent::ServiceIdentityReport]) -> Option<Vec<agent::ServiceIdentityReport>> {
    let mut seen = BTreeSet::new();
    let fits = said.len() <= MAX_SERVICES
        && said.iter().all(|one| {
            ServiceName::parse(&one.service).is_ok() && seen.insert(one.service.as_str())
        });
    fits.then(|| {
        said.iter()
            .map(|one| agent::ServiceIdentityReport {
                last_error: one
                    .last_error
                    .as_deref()
                    .map(|text| cut(text, MAX_ERROR).to_owned()),
                ..one.clone()
            })
            .collect()
    })
}

impl Coordinator {
    /// Notes the list a heartbeat over gRPC from `who` carries: kept when it is good, else counted
    /// and logged as a dropped fact, the one read before kept. An empty list (also the one of a
    /// daemon that predates the field) clears what was kept.
    pub(crate) fn hear_service_identities(
        &self,
        who: &Identity,
        request: &agent::HeartbeatRequest,
    ) {
        let Some(list) = kept(&request.service_identities) else {
            self.note_extras_dropped(who, "the service identities list breaks a rule", &[FACT]);
            return;
        };
        let mut fleet = self.fleet.lock().unwrap_or_else(PoisonError::into_inner);
        if list.is_empty() {
            fleet.service_identities.0.remove(&who.host_id);
        } else {
            fleet.service_identities.0.insert(who.host_id.clone(), list);
        }
    }
}

/// Fills in `host`, as `Hosts.List` and `Hosts.Get` made it for the host `id`, with the last list
/// its daemon reported, in its heartbeat.
pub(crate) fn show(fleet: &Fleet, id: &HostId, host: &mut operator::Host) {
    if let Some(list) = fleet.service_identities.0.get(id) {
        host.heartbeat.get_or_insert_default().service_identities = list.clone();
    }
}

#[cfg(test)]
#[path = "service_identity_report_tests.rs"]
mod tests;
