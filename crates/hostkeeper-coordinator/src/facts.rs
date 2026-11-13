//! The facts file (docs/design/hostgroups.md, sections 6.4 and 9): what the coordinator tells a
//! host about itself in the answer to a heartbeat over gRPC, so that its daemon can write them
//! to the host's facts file: the host's id, its host group and its labels.
//!
//! A daemon that announces [`FEATURE`] reports, in `facts_generation`, the generation of the
//! facts it last wrote. The coordinator sends the facts, whole, when its generation of the host
//! is not that one ([`Coordinator::host_facts`]). A daemon that does not announce the feature,
//! one that beats over the JSON route and a coordinator that does not advertise the feature
//! send and read nothing.
//!
//! # The generation
//!
//! Nothing new is stored. A host's generation ([`Generations`]) is the audit number of the
//! newest change that raised it, else the number of the last audit entry when the ops store was
//! opened. The changes that raise it are the ones that change what the facts say: `set_labels`,
//! `enroll_approve`, `enroll_replace`, `enrollment.auto_approved` and `host_move` (a host moved
//! into another group, [`crate::host_move`]), each for the entry's node. All are storage-first, and the generation is raised only once the change is committed
//! and in memory ([`crate::ops::OpsStore`]), so a number a host was ever given is durable and the
//! next start's base is at least it: the generation never goes back, across restarts either.
//!
//! A restart raises every host's generation to the base, so each host is resent its facts once;
//! its daemon rewrites the file only on a change, and writes nothing. An approval whose audit
//! entry fails to land raises nothing until the next start (`audit_to` audits it again).
//!
//! The generation is read before the content. A change writes its content to memory before it
//! raises the generation, so a beat never pairs a raised generation with the content before it.
//! A move writes the host's group to its record before its audit entry is committed, and takes
//! the fleet's lock for both; the facts of a host that has not reported the generation are
//! made under that lock ([`Coordinator::host_facts`]), so they never carry the group of a move
//! that is then refused.
//!
//! # The host group
//!
//! The group the facts name is the one the host's record holds ([`Coordinator::approved_group`]),
//! and, for a host the record places in none, the role it was approved for: the name the
//! migration gives its group.
//!
//! # A move waits for the facts
//!
//! A host that is moving into another group is ordered nothing, after its admission to the new
//! group's plans, until its heartbeat reports the generation its move raised
//! ([`crate::moving`]). The generation a daemon that announced [`FEATURE`] reported is kept by
//! the fleet ([`Reported`]), from the heartbeat over gRPC that said it
//! ([`Coordinator::hear_facts`]). A daemon that did not announce the feature, one that beats
//! over the JSON route and a coordinator that does not advertise the feature have no facts and
//! are not held for them.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use hostkeeper_proto::hostkeeper::agent::v1 as agent;

use hostkeeper_wire::HostId;

use crate::enrollment::{ACTION_APPROVE, ACTION_AUTO_APPROVED, ACTION_REPLACE, UNGROUPED_ROLE};
use crate::host_move::ACTION_MOVE;
use crate::identity::Identity;
use crate::ops::AuditEntry;
use crate::service::Coordinator;

/// The feature a daemon announces to be sent the facts file's content and to report the
/// generation of what it wrote; the coordinator advertises it ([`crate::service::FEATURES`]).
pub const FEATURE: &str = "facts-v1";

/// The audit action of an operator's change of a host's labels.
const ACTION_SET_LABELS: &str = "set_labels";

/// Whether an audit entry of `action` changes what a host's facts say.
fn raises(action: &str) -> bool {
    [
        ACTION_SET_LABELS,
        ACTION_APPROVE,
        ACTION_REPLACE,
        ACTION_AUTO_APPROVED,
        ACTION_MOVE,
    ]
    .contains(&action)
}

/// The generation each host that announced [`FEATURE`] over gRPC reported in its last
/// heartbeat: `None` while it has reported none. A host that did not announce it has no entry.
#[derive(Clone, Debug, Default)]
pub(crate) struct Reported(BTreeMap<HostId, Option<u64>>);

impl Reported {
    /// Notes what the last heartbeat of the host `id` said: the generation it reported, or, when
    /// it did not announce the feature (`announced` is false), nothing.
    fn hear(&mut self, id: &HostId, announced: bool, generation: Option<u64>) {
        if announced {
            self.0.insert(id.clone(), generation);
        } else {
            self.0.remove(id);
        }
    }

    /// Whether the host `id` announced the feature and has not reported `current`.
    pub(crate) fn awaits(&self, id: &HostId, current: u64) -> bool {
        self.0.get(id).is_some_and(|said| *said != Some(current))
    }
}

/// The generation of each host's facts, in memory, under a lock of its own that holds nothing
/// else.
#[derive(Debug)]
pub(crate) struct Generations {
    /// The audit number of the last entry when the store was opened: every host's generation
    /// until a change raises it.
    base: u64,
    raised: Mutex<BTreeMap<String, u64>>,
}

impl Generations {
    /// Generations that start at `base` for every host.
    pub(crate) fn from(base: u64) -> Self {
        Self {
            base,
            raised: Mutex::new(BTreeMap::new()),
        }
    }

    /// Notes the committed entries `entries`: each that changes what a host's facts say raises
    /// that host's generation to its number.
    pub(crate) fn raise(&self, entries: &[AuditEntry]) {
        let mut raised = self.raised.lock().unwrap_or_else(PoisonError::into_inner);
        for entry in entries.iter().filter(|e| raises(&e.action)) {
            if let Some(node) = &entry.node_id {
                let at = raised.entry(node.clone()).or_insert(0);
                *at = (*at).max(entry.seq);
            }
        }
    }

    /// The generation of the facts of the host `node_id`.
    pub(crate) fn of(&self, node_id: &str) -> u64 {
        let raised = self.raised.lock().unwrap_or_else(PoisonError::into_inner);
        raised.get(node_id).copied().unwrap_or(0).max(self.base)
    }
}

impl Coordinator {
    /// Whether a heartbeat over gRPC announces [`FEATURE`] and this coordinator advertises it.
    fn facts_asked(&self, request: &agent::HeartbeatRequest) -> bool {
        request.features.iter().any(|f| f == FEATURE) && self.features.iter().any(|f| f == FEATURE)
    }

    /// Notes what a heartbeat over gRPC from `who` said of the host's facts, before the
    /// heartbeat is decided: whether its daemon announced [`FEATURE`] and the generation it
    /// reported. A move waits for it ([`crate::moving`]).
    pub(crate) fn hear_facts(&self, who: &Identity, request: &agent::HeartbeatRequest) {
        let announced = self.facts_asked(request);
        self.fleet
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .facts
            .hear(&who.host_id, announced, request.facts_generation);
    }

    /// The facts the answer to a heartbeat over gRPC from `who` carries, if it carries them:
    /// when the daemon announced [`FEATURE`], this coordinator advertises it, and the generation
    /// the daemon reported is not the current one.
    pub(crate) fn host_facts(
        &self,
        who: &Identity,
        request: &agent::HeartbeatRequest,
    ) -> Option<agent::HostFacts> {
        if !self.facts_asked(request) {
            return None;
        }
        let host = who.host_id.as_str();
        if request.facts_generation == Some(self.ops.facts_generation(host)) {
            return None;
        }
        // Under the fleet's lock, which a move holds from the write of the host's group to the
        // end of its commit: see the module documentation.
        let _fleet = self.fleet.lock().unwrap_or_else(PoisonError::into_inner);
        // First: see the module documentation.
        let generation = self.ops.facts_generation(host);
        let host_group = self.approved_group(who)?;
        let labels = self.ops.read(|state| {
            state
                .hosts
                .get(host)
                .map(|ops| ops.labels.clone().into_iter().collect())
                .unwrap_or_default()
        });
        Some(agent::HostFacts {
            generation,
            host_id: host.to_owned(),
            host_group,
            labels,
        })
    }

    /// The group the enrollment record of the host `who` holds; else the role it holds; `who`'s
    /// own role when the coordinator has no enrollment or the store holds no record of the host
    /// (a certificate of the farm's CA). `None` when the store cannot be read: the next beat
    /// tries again.
    fn approved_group(&self, who: &Identity) -> Option<String> {
        let own = || Some(who.role.to_string());
        // A wire host id is always an enrollment host id; the parse is only a conversion.
        let enrolled = hostkeeper_enrollment::names::HostId::parse(who.host_id.as_str()).ok();
        let Some((enrollment, host)) = self.enrollment().zip(enrolled) else {
            return own();
        };
        let store = enrollment.store();
        let record = store.membership(&host).and_then(|held| match held {
            Some(held) if !held.host_group.is_empty() => Ok(Some(held.host_group)),
            // A host approved in no group holds the placeholder role: it is in no group.
            _ => store.host_role(&host).map(|role| {
                role.map(|role| match role.as_str() {
                    UNGROUPED_ROLE => String::new(),
                    named => named.to_owned(),
                })
            }),
        });
        match record {
            Ok(Some(group)) => Some(group),
            Ok(None) => own(),
            Err(why) => {
                tracing::warn!(host = %who.host_id, %why, "the host's group cannot be read; its facts wait for the next heartbeat");
                None
            }
        }
    }
}
