//! What an operator reads of the coordinator (`GET /v1/status`): every service with its desired
//! state, its hosts as it sees them and why each is not ordered, its convergence, the ops state
//! and, with enrollment, the server certificate; and the convergence `GET /v1/convergence`
//! answers ([`convergence_of`]).

use std::collections::BTreeMap;
use std::time::Duration;

use hostkeeper_wire::HostId;
use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::{Agent, Heartbeat, LEGACY_AGENT_NAME};
use serde::Serialize;

use crate::converge::{Convergence, RoleCounts, converge_service};
use crate::decide::Why;
use crate::desired::Desired;
use crate::enrollment::ServerCertificate;
use crate::fleet::{Fleet, Host};
use crate::halt;
use crate::membership::{Member, account, deferred_count, members};
use crate::ops::OpsState;
use crate::services::ServiceDef;

/// One host in the status.
#[derive(Debug, Serialize)]
pub struct HostStatus {
    /// The node id from its certificate.
    pub node_id: String,
    /// The agent that sent the heartbeat: its name and version, from the host's last
    /// heartbeat.
    pub agent: AgentStatus,
    /// The host role from its certificate.
    pub role: String,
    /// Heartbeated within the down limit.
    pub live: bool,
    /// Live, healthy and not updating.
    pub up: bool,
    /// Holds an update slot for the service (one is live at a time across every service).
    pub holds_slot: bool,
    /// Whole seconds since its last heartbeat.
    pub silent_secs: u64,
    /// What it last said.
    #[serde(flatten)]
    pub beat: Heartbeat,
}

/// The agent of a host in the status: what its heartbeat's `agent` said, and, for an agent
/// that predates the field, only the name it is counted under ([`LEGACY_AGENT_NAME`]).
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct AgentStatus {
    /// The agent's name.
    pub name: String,
    /// The agent's version; left out when the heartbeat did not say.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<Version>,
}

impl AgentStatus {
    /// The status of `agent`, if the heartbeat named one.
    #[must_use]
    pub fn of(agent: Option<&Agent>) -> Self {
        match agent {
            Some(agent) => Self {
                name: agent.name.as_str().to_owned(),
                version: Some(agent.version.clone()),
            },
            None => Self {
                name: LEGACY_AGENT_NAME.to_owned(),
                version: None,
            },
        }
    }
}

/// One host in the status of a service: the host as that service sees it, and why it is
/// not ordered.
#[derive(Debug, Serialize)]
pub struct ServiceHostStatus {
    /// The host, with `up` and `holds_slot` counted for the service.
    #[serde(flatten)]
    pub host: HostStatus,
    /// The rule that holds an order for the service back at this moment, or `None` when the
    /// host would be ordered at its next heartbeat. It is spelled in snake case: a gate such as
    /// `no_slot` or a skip such as `up_to_date`.
    pub hold: Option<Why>,
    /// An operator holds the pair and the host runs a version marked bad: an alert
    /// ([`crate::alerts`], `held_on_bad_version`). Left out when it does not.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub held_on_bad_version: bool,
}

/// One service in the status.
#[derive(Debug, Serialize)]
pub struct ServiceStatus {
    /// Its desired state, if one was ever set.
    pub desired: Option<Desired>,
    /// The counts of every role that has a host heartbeating the service, in role order.
    pub roles: Vec<RoleCounts>,
    /// Every host that heartbeated the service, by node id.
    pub hosts: Vec<ServiceHostStatus>,
    /// The nodes the service does not count, by why: excluded (with the operator's reason),
    /// absent, missing or unmanaged. Left out when there are none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<Member>,
    /// How many of `hosts` are up for the service.
    pub up: usize,
    /// How many of `hosts` hold one of the service's update slots.
    pub slots_held: usize,
    /// What `GET /v1/convergence` answers for the service.
    pub convergence: Convergence,
    /// Its update plan for the desired version ([`crate::stages`]), which `Releases.Status`
    /// answers; none without a desired version. The JSON status, whose bytes are frozen, leaves
    /// it out.
    #[serde(skip)]
    pub plan: Option<hostkeeper_proto::hostkeeper::operator::v1::RolloutPlan>,
}

/// What an operator reads.
#[derive(Debug, Serialize)]
pub struct Status {
    /// The desired state of the legacy service, if there is one and one was ever set.
    pub desired: Option<Desired>,
    /// The desired state of every service, by name.
    pub services: BTreeMap<String, Desired>,
    /// The coordinator is still within its quiet period and orders nothing.
    pub quiet: bool,
    /// Heartbeats since the coordinator started whose `services` list was bad and was
    /// dropped: the host was answered as a host that lists no services.
    pub services_dropped: u64,
    /// Heartbeats since the coordinator started whose optional fact was bad and was dropped,
    /// by the fact's name (`step`, `commit`, ...): one per beat per fact. Absent while none.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub extras_dropped: BTreeMap<&'static str, u64>,
    /// Every host of the legacy service heard from since the coordinator started, by node id
    /// (none without a legacy service).
    pub hosts: Vec<HostStatus>,
    /// Every service the coordinator manages, by name: its desired state, the
    /// counts of its hosts per role, its hosts, and its convergence.
    pub per_service: BTreeMap<String, ServiceStatus>,
    /// What operators changed ([`crate::ops`]): exclusions, labels, the audit ring. Left out
    /// until the first change, so a farm that never uses it reads the status it always did.
    #[serde(skip_serializing_if = "OpsState::is_empty")]
    pub ops: OpsState,
    /// The commit this coordinator was built from ([`crate::BUILD_COMMIT`]).
    pub commit: &'static str,
    /// With enrollment, the coordinator's server certificate: when it ends, and why the last
    /// renewal failed, if it did ([`crate::enrollment::Enrollment::renew_server`]). Left out
    /// without enrollment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_certificate: Option<ServerCertificate>,
}

/// The host `id` as the status shows it for `service`: whether it is up and holds a slot
/// counted for that service.
pub(crate) fn host_status(
    fleet: &Fleet,
    service: &str,
    id: &HostId,
    host: &Host,
    now: Duration,
) -> HostStatus {
    // The flattened heartbeat carries no `agent` of its own: the status says it as `agent`.
    let mut beat = host.beat.clone();
    beat.agent = None;
    HostStatus {
        node_id: id.to_string(),
        agent: AgentStatus::of(fleet.node(id).and_then(|node| node.agent.as_ref())),
        role: host.role_name().to_owned(),
        live: host.is_live(now),
        up: fleet.is_up(service, id, host, now),
        holds_slot: fleet.holds_slot(service, id, host, now),
        silent_secs: now.saturating_sub(host.seen).as_secs(),
        beat,
    }
}

/// Where the hosts of `def` stand: the one answer both `GET /v1/convergence` and the status
/// give. Every service is counted by [`converge_service`]: every live host at the desired
/// version and none updating. A halted service is `halted` whatever its hosts say.
pub(crate) fn convergence_of(
    def: &ServiceDef,
    desired: Option<&Desired>,
    fleet: &Fleet,
    ops: &OpsState,
    now: Duration,
) -> Convergence {
    let convergence = converge_service(def, desired, fleet, now);
    let deferred = deferred_count(def, fleet, convergence.desired.as_ref(), now);
    let convergence = account(
        convergence,
        def,
        &members(def, fleet, ops, desired, now),
        deferred,
    );
    match halt::standing(ops, &def.name) {
        Some(halt) => convergence.halted(halt),
        None => convergence,
    }
}
