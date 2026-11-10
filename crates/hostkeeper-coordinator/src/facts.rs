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
//! `enroll_approve`, `enroll_replace` and `enrollment.auto_approved`, each for the entry's
//! node. All are storage-first, and the generation is raised only once the change is committed
//! and in memory ([`crate::ops::OpsStore`]), so a number a host was ever given is durable and the
//! next start's base is at least it: the generation never goes back, across restarts either.
//!
//! A restart raises every host's generation to the base, so each host is resent its facts once;
//! its daemon rewrites the file only on a change, and writes nothing. An approval whose audit
//! entry fails to land raises nothing until the next start (`audit_to` audits it again).
//!
//! The generation is read before the content. A change writes its content to memory before it
//! raises the generation, so a beat never pairs a raised generation with the content before it.
//!
//! Until the coordinator holds host groups (a later build item), the host group is the role the
//! host was approved for: the name the migration gives its group.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use hostkeeper_proto::hostkeeper::agent::v1 as agent;

use crate::enrollment::{ACTION_APPROVE, ACTION_AUTO_APPROVED, ACTION_REPLACE};
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
    ]
    .contains(&action)
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
    /// The facts the answer to a heartbeat over gRPC from `who` carries, if it carries them:
    /// when the daemon announced [`FEATURE`], this coordinator advertises it, and the generation
    /// the daemon reported is not the current one.
    pub(crate) fn host_facts(
        &self,
        who: &Identity,
        request: &agent::HeartbeatRequest,
    ) -> Option<agent::HostFacts> {
        let asked = request.features.iter().any(|f| f == FEATURE);
        if !asked || !self.features.iter().any(|f| f == FEATURE) {
            return None;
        }
        let host = who.host_id.as_str();
        // First: see the module documentation.
        let generation = self.ops.facts_generation(host);
        if request.facts_generation == Some(generation) {
            return None;
        }
        let host_group = self.approved_role(who)?;
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

    /// The role the enrollment record of the host `who` holds; `who`'s own role when the
    /// coordinator has no enrollment or the store holds no record of the host (a certificate of
    /// the farm's CA). `None` when the store cannot be read: the next beat tries again.
    fn approved_role(&self, who: &Identity) -> Option<String> {
        let own = || Some(who.role.to_string());
        let Some(enrollment) = self.enrollment() else {
            return own();
        };
        // A wire host id is always an enrollment host id; the parse is only a conversion.
        let record = hostkeeper_enrollment::names::HostId::parse(who.host_id.as_str())
            .ok()
            .map(|host| enrollment.store().host_role(&host))
            .transpose();
        match record {
            Ok(Some(Some(role))) => Some(role.as_str().to_owned()),
            Ok(_) => own(),
            Err(why) => {
                tracing::warn!(host = %who.host_id, %why, "the host's role cannot be read; its facts wait for the next heartbeat");
                None
            }
        }
    }
}
