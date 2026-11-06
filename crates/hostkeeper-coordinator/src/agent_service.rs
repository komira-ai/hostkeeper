//! The service an agent is: the service whose name is the name the agent gives itself in a
//! heartbeat's `agent` (in practice `hostkeeper`, whose daemon is the agent of every host).
//!
//! The daemon a deb or a pkg installed runs, but no package of it is installed, so its heartbeat
//! lists no entry of its own service; once the coordinator installs the package, the entry's
//! version is the package's, which the running binary is not until hostkeeper's self-update has
//! restarted into it (or after the self-update fell back). What runs is the agent, at the version
//! its heartbeat's `agent` names. So, for an agent that announced
//! [`ORDER_FEATURE`] (its top level is the agent itself) and whose name is a service the
//! coordinator manages:
//!
//! - an entry of that service in the heartbeat is kept, at the agent's version ([`own_beat`]);
//! - with no such entry, the host still runs the service, at the agent's version: it is noted as
//!   the service's heartbeat of the host, healthy ([`own_beat`] returns it), and the host is a
//!   member of the service as if it had announced it ([`runs_itself`],
//!   [`crate::membership::membership`]). It is not in the node's announced services, so the
//!   order path ([`crate::decide`]) still reads the service as not installed and orders the
//!   install of the desired release first, which the daemon refuses an update for until the
//!   package is there; once it is, the entry is listed and the update that runs the self-update
//!   is ordered, since the agent's version is not the desired one.

use hostkeeper_wire::wire::{Agent, Health, Heartbeat, ORDER_FEATURE, ServiceBeat};

use crate::fleet::Node;
use crate::services::Services;

/// The agent of `beat` when it is the service of that name `services` defines: it announced
/// [`ORDER_FEATURE`] and names itself.
fn own_agent<'a>(services: &Services, beat: &'a Heartbeat) -> Option<&'a Agent> {
    beat.agent
        .as_ref()
        .filter(|_| beat.agent_features.iter().any(|f| f == ORDER_FEATURE))
        .filter(|agent| services.get(agent.name.as_str()).is_some())
}

/// Sets the version of the entry of the agent's own service in `beat` to the agent's version,
/// and returns the entry to note for the host when `beat` lists none: the service at the agent's
/// version, healthy, with no update running or reported. `None` for a heartbeat whose agent is
/// no service of `services` (or that announced no [`ORDER_FEATURE`]), and for one that lists the
/// service.
pub(crate) fn own_beat(services: &Services, beat: &mut Heartbeat) -> Option<ServiceBeat> {
    let agent = own_agent(services, beat)?.clone();
    let listed = beat
        .services
        .iter_mut()
        .flatten()
        .find(|one| one.name == agent.name.as_str());
    match listed {
        Some(entry) => {
            entry.version = agent.version;
            None
        }
        None => Some(ServiceBeat {
            name: agent.name.as_str().to_owned(),
            version: agent.version,
            health: Health::Healthy,
            last_update: None,
            updating: None,
            step: None,
            commit: None,
            step_secs: Default::default(),
            promotion_pending: false,
            deferred: None,
        }),
    }
}

/// Whether the node runs `service` as its agent: its agent announced [`ORDER_FEATURE`] and
/// names itself `service`.
#[must_use]
pub fn runs_itself(node: &Node, service: &str) -> bool {
    node.features.iter().any(|f| f == ORDER_FEATURE)
        && node
            .agent
            .as_ref()
            .is_some_and(|agent| agent.name.as_str() == service)
}
