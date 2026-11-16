//! The Remote Login probe a Mac's daemon runs at its start and reports in every heartbeat over
//! gRPC after it (`HeartbeatRequest.remote_login_probe`): the commands it ran as root under
//! launchd to learn whether it can turn on Remote Login without Full Disk Access, each with its
//! exit code and output.
//!
//! The coordinator keeps the last probe of each host that keeps the rules of
//! `RemoteLoginProbe` ([`keeps_rules`]), in memory with the fleet, and `Hosts.List` and
//! `Hosts.Get` show it in the host's heartbeat ([`show`]). A probe that breaks a rule is counted
//! and logged as a dropped fact, and the one read before is kept; a heartbeat with none leaves
//! the one kept as it is (a daemon sends none until its probe is done). Nothing decides on it.
//! After a restart the coordinator holds none until the host's next heartbeat, which carries it
//! again.

use std::collections::BTreeMap;
use std::sync::PoisonError;

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as operator;
use hostkeeper_proto::{MAX_PROBE_ARGS, MAX_PROBE_STEPS, MAX_PROBE_TEXT};
use hostkeeper_wire::HostId;

use crate::fleet::Fleet;
use crate::identity::Identity;
use crate::service::Coordinator;

/// The name a probe that breaks a rule is counted under.
pub(crate) const FACT: &str = "remote_login_probe";

/// The last probe of each host that reported one.
#[derive(Clone, Debug, Default)]
pub(crate) struct Probes(BTreeMap<HostId, agent::RemoteLoginProbe>);

impl Probes {
    /// Forgets the host `id`'s probe, when the fleet forgets the host.
    pub(crate) fn forget(&mut self, id: &HostId) {
        self.0.remove(id);
    }
}

/// Whether `probe` keeps the rules of `RemoteLoginProbe`: at most [`MAX_PROBE_STEPS`] steps, each
/// named by 1 to 64 bytes of `[a-z0-9_]`, with at most [`MAX_PROBE_ARGS`] arguments, and every
/// text at most [`MAX_PROBE_TEXT`] bytes.
pub(crate) fn keeps_rules(probe: &agent::RemoteLoginProbe) -> bool {
    probe.steps.len() <= MAX_PROBE_STEPS && probe.steps.iter().all(step_keeps_rules)
}

fn step_keeps_rules(step: &agent::ProbeStep) -> bool {
    let name_ok = (1..=64).contains(&step.name.len())
        && step
            .name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    name_ok
        && step.argv.len() <= MAX_PROBE_ARGS
        && step
            .argv
            .iter()
            .chain([&step.stdout, &step.stderr, &step.error])
            .all(|text| text.len() <= MAX_PROBE_TEXT)
}

impl Coordinator {
    /// Notes the probe a heartbeat over gRPC from `who` carries, if it carries one: kept when it
    /// keeps the rules ([`keeps_rules`]), else counted and logged as a dropped fact, the one read
    /// before kept.
    pub(crate) fn hear_remote_login_probe(
        &self,
        who: &Identity,
        request: &agent::HeartbeatRequest,
    ) {
        let Some(probe) = &request.remote_login_probe else {
            return;
        };
        if !keeps_rules(probe) {
            self.note_extras_dropped(who, "the remote login probe breaks a rule", &[FACT]);
            return;
        }
        self.fleet
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remote_login_probes
            .0
            .insert(who.host_id.clone(), probe.clone());
    }
}

/// Fills in `host`, as `Hosts.List` and `Hosts.Get` made it for the host `id`, with the last probe
/// its daemon reported, in its heartbeat.
pub(crate) fn show(fleet: &Fleet, id: &HostId, host: &mut operator::Host) {
    if let Some(probe) = fleet.remote_login_probes.0.get(id) {
        host.heartbeat.get_or_insert_default().remote_login_probe = Some(probe.clone());
    }
}

#[cfg(test)]
#[path = "remote_login_probe_tests.rs"]
mod tests;
