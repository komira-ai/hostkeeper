//! The coordinator: the fleet the heartbeats build, the desired state, and who may change
//! or read either. No sockets here; [`crate::http`] maps requests onto these calls.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use hostkeeper_oidc::{Refusal, Verified, Verifier};
use hostkeeper_wire::identity::SanPrefixes;
use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::{Agent, Answer, Heartbeat, LEGACY_AGENT_NAME, ServiceReply};
use serde::Serialize;

use crate::acks::{clear_bad, release_ack};
use crate::converge::{Convergence, RoleCounts, converge_service, role_counts};
use crate::decide::{Decision, Why};
use crate::desired::{Desired, DesiredError, DesiredStore, StoreError};
use crate::fleet::{Fleet, Host, Node};
use crate::halt;
use crate::identity::Identity;
use crate::membership::{Member, Standing, account, deferred_count, members, standing};
use crate::ops::{self, AuditEntry, OperateError, OpsError, OpsState, OpsStore};
use crate::reconcile::reconcile;
use crate::services::{ServiceDef, Services};
use crate::tokens::{OperatorTokens, TokenName};

/// The time since the coordinator started.
pub type Clock = Arc<dyn Fn() -> Duration + Send + Sync>;

/// The most bytes of a bearer token looked at.
pub const MAX_BEARER_BYTES: usize = 8192;

/// Why a heartbeat was refused: it lists no services and no legacy service is set, so it
/// says nothing the coordinator can attribute to a service.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("it lists no services and no legacy service is set")]
pub struct HeartbeatRefused;

/// Why the desired state was not set.
#[derive(Debug, thiserror::Error)]
pub enum SetError {
    /// The caller has no valid CI token: no bearer, or the verifier refused it (the name is
    /// the verifier's short reason; the operator token is refused like any bad token).
    #[error("unauthorized: {0}")]
    Unauthorized(&'static str),
    /// The token's issuer keys could not be fetched.
    #[error("the issuer's keys are unavailable")]
    Unavailable,
    /// The body is no desired state.
    #[error(transparent)]
    Invalid(#[from] DesiredError),
    /// The state file could not be written.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The ops file could not be written (the bad mark of the version could not be cleared).
    #[error(transparent)]
    Ops(#[from] OpsError),
    /// The allow-list entry that admitted the token does not name this service.
    #[error("the token may not set this service")]
    Forbidden,
}

/// Why a CI token was not accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CiRefused {
    /// No bearer, or the verifier refused it (the name is the verifier's short reason; the
    /// operator token is refused like any bad token).
    #[error("unauthorized: {0}")]
    Unauthorized(&'static str),
    /// The token's issuer keys could not be fetched.
    #[error("the issuer's keys are unavailable")]
    Unavailable,
}

impl From<CiRefused> for SetError {
    fn from(refused: CiRefused) -> Self {
        match refused {
            CiRefused::Unauthorized(why) => Self::Unauthorized(why),
            CiRefused::Unavailable => Self::Unavailable,
        }
    }
}

/// The caller of a read has no valid operator token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("unauthorized")]
pub struct Unauthorized;

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
}

/// Why the convergence was not read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConvergenceError {
    /// The caller's CI token was not accepted.
    #[error(transparent)]
    Refused(#[from] CiRefused),
    /// The coordinator manages no such service.
    #[error("no such service")]
    UnknownService,
}

/// What `POST /v1/desired` answers: the state as stored, and, only when there are any, the
/// roles whose limits were dropped because the service does not have them.
#[derive(Debug, Serialize)]
pub struct Set {
    /// The state as stored.
    #[serde(flatten)]
    pub desired: Desired,
    /// The dropped roles, sorted.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dropped_roles: Vec<String>,
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
}

/// The features this coordinator advertises to an agent that announces `agent_features`.
///
/// Each name is added in the change that implements what it names (`halt-v1`; the others
/// follow), so a name is never advertised for behaviour that is not there. `deferred-v1`: a
/// heartbeat's `deferred` report ends the order, holds no slot and backs the host off
/// ([`crate::defer`]). `halt-v1`: the clear generations of [`crate::halt`]. `heartbeat-v2`:
/// selector membership, hosts that are `missing`, `absent` or `unmanaged`
/// ([`crate::membership`]).
pub const FEATURES: &[&str] = &[
    "deferred-v1",
    halt::FEATURE,
    crate::membership::HEARTBEAT_V2,
];

/// How long a host's warn about one dropped fact is silent after it was logged.
const EXTRAS_LOG_EVERY: Duration = Duration::from_secs(3600);

/// The dropped optional facts: a count per fact name, and when each host's fact was last
/// logged.
#[derive(Default)]
struct ExtrasDropped {
    counts: BTreeMap<&'static str, u64>,
    logged: HashMap<(String, &'static str), Duration>,
}

/// The coordinator.
pub struct Coordinator {
    fleet: Mutex<Fleet>,
    desired: DesiredStore,
    services: Services,
    legacy: Option<String>,
    prefixes: SanPrefixes,
    ops: OpsStore,
    verifier: Arc<Verifier>,
    tokens: OperatorTokens,
    clock: Clock,
    services_dropped: AtomicU64,
    extras: Mutex<ExtrasDropped>,
    features: Vec<String>,
    /// Told when CI's set of a desired state finds the fleet's lock held and waits for it: a
    /// test's progress event, so a race is made to happen rather than waited for.
    #[cfg(test)]
    set_waits: Mutex<Option<std::sync::mpsc::Sender<SetProgress>>>,
}

/// What a test hears of CI's set of a desired state while it races another change.
#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SetProgress {
    /// The set found the fleet's lock held, and waits for it.
    Waits,
    /// The set returned.
    Returned,
}

impl std::fmt::Debug for Coordinator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Coordinator").finish_non_exhaustive()
    }
}

impl Coordinator {
    /// A coordinator that starts now, by `clock`, with the desired state of `desired`. CI's
    /// token is checked by `verifier`; an operator's by `operator`: the tokens of a tokens file
    /// ([`OperatorTokens::from_file`]) or one token, a `String`, named `operator`.
    #[must_use]
    pub fn new(
        desired: DesiredStore,
        verifier: Arc<Verifier>,
        operator: impl Into<OperatorTokens>,
        clock: Clock,
    ) -> Self {
        let started = clock();
        Self {
            fleet: Mutex::new(Fleet::new(started)),
            desired,
            services: Services::default(),
            legacy: None,
            prefixes: SanPrefixes::default(),
            ops: OpsStore::volatile(),
            verifier,
            tokens: operator.into(),
            clock,
            services_dropped: AtomicU64::new(0),
            extras: Mutex::new(ExtrasDropped::default()),
            features: FEATURES.iter().map(|name| (*name).to_owned()).collect(),
            #[cfg(test)]
            set_waits: Mutex::new(None),
        }
    }

    /// The same coordinator over `services` instead of none.
    #[must_use]
    pub fn with_services(mut self, services: Services) -> Self {
        self.services = services;
        self
    }

    /// The same coordinator with `legacy` as its legacy service: the service whose heartbeat
    /// is the top-level fields of a heartbeat, and whose desired state a body without
    /// `service` sets. With none, only the entries of a heartbeat's `services` count.
    #[must_use]
    pub fn with_legacy_service(mut self, legacy: Option<String>) -> Self {
        self.legacy = legacy;
        self
    }

    /// The same coordinator reading certificates under `prefixes` instead of the default
    /// namespaces.
    #[must_use]
    pub fn with_prefixes(mut self, prefixes: SanPrefixes) -> Self {
        self.prefixes = prefixes;
        self
    }

    /// The certificate namespaces callers are identified under.
    #[must_use]
    pub fn prefixes(&self) -> &SanPrefixes {
        &self.prefixes
    }

    /// The legacy service, if there is one.
    #[must_use]
    pub fn legacy_service(&self) -> Option<&str> {
        self.legacy.as_deref()
    }

    /// The operator's tokens, which the tick that looks at the TLS files re-reads.
    #[must_use]
    pub fn tokens(&self) -> &OperatorTokens {
        &self.tokens
    }

    /// The same coordinator over `ops` instead of a store that keeps nothing.
    #[must_use]
    pub fn with_ops(mut self, ops: OpsStore) -> Self {
        self.ops = ops;
        self
    }

    /// The same coordinator advertising `features` instead of [`FEATURES`].
    #[cfg(test)]
    #[must_use]
    pub fn with_features(mut self, features: &[&str]) -> Self {
        self.features = features.iter().map(|name| (*name).to_owned()).collect();
        self
    }

    /// Whether the fleet's lock, which every decision and order is made under, is held now.
    #[cfg(test)]
    pub(crate) fn fleet_held(&self) -> bool {
        matches!(
            self.fleet.try_lock(),
            Err(std::sync::TryLockError::WouldBlock)
        )
    }

    /// Tells `waits` [`SetProgress::Waits`] each time CI's set of a desired state finds the
    /// fleet's lock held and waits for it.
    #[cfg(test)]
    pub(crate) fn tell_when_set_waits(&self, waits: std::sync::mpsc::Sender<SetProgress>) {
        *self
            .set_waits
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(waits);
    }

    /// The fleet's lock, for CI's set of a desired state. Under test, a set that finds the
    /// lock held first says so to the test that asked ([`Self::tell_when_set_waits`]); then it
    /// waits for the lock, as every build does.
    fn lock_fleet_for_set(&self) -> std::sync::MutexGuard<'_, Fleet> {
        #[cfg(test)]
        self.set_waits
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|_| self.fleet_held())
            .for_each(|waits| {
                let _ = waits.send(SetProgress::Waits);
            });
        self.fleet.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The ops store.
    #[cfg(test)]
    pub(crate) fn ops(&self) -> &OpsStore {
        &self.ops
    }

    /// Counts, by name, the optional `facts` of a heartbeat from `who` that were bad and were
    /// dropped (`why` is the reader's reason), and logs it unless every one of them was logged
    /// for this host within the last hour. Returns whether it logged. The heartbeat is still
    /// answered, without those facts.
    pub fn note_extras_dropped(&self, who: &Identity, why: &str, facts: &[&'static str]) -> bool {
        let now = (self.clock)();
        let node = who.host_id.to_string();
        let mut extras = self.extras.lock().unwrap_or_else(PoisonError::into_inner);
        let mut due = false;
        for fact in facts {
            *extras.counts.entry(*fact).or_default() += 1;
            let last = extras.logged.get(&(node.clone(), *fact)).copied();
            if last.is_none_or(|at| now.saturating_sub(at) >= EXTRAS_LOG_EVERY) {
                extras.logged.insert((node.clone(), *fact), now);
                due = true;
            }
        }
        drop(extras);
        if due {
            tracing::warn!(node_id = %who.host_id, why, "dropped optional fields of a heartbeat");
        }
        due
    }

    /// Counts and logs a heartbeat from `who` whose `services` list was bad and was dropped
    /// (`why` is the reader's reason). The heartbeat itself is still answered, by
    /// [`Self::heartbeat`], as one that lists no services.
    pub fn note_services_dropped(&self, who: &Identity, why: &str) {
        self.services_dropped.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(node_id = %who.host_id, why, "dropped the services list of a heartbeat");
    }

    /// Notes a heartbeat from `who` and answers it. With no desired state set, nothing.
    ///
    /// The top-level fields are the legacy service's, when one is set; without one they
    /// belong to no service and only the entries of `services` count. Each entry of `services`
    /// is noted as that service's heartbeat of the host, if the coordinator manages the
    /// service and runs it on the host's role (any other entry is ignored). The services are
    /// decided by priority, the highest first ([`crate::services::ServiceDef::priority`], 0
    /// unless a definition sets it); services of one priority go the legacy service first,
    /// then in the order the host listed them. The first order is the reply: one update per
    /// reply. A host that sent no `services` is answered about the legacy service alone, in
    /// the shape it has always read.
    ///
    /// # Errors
    /// [`HeartbeatRefused`]: the heartbeat lists no services (none, or a list that was bad and
    /// dropped) and no legacy service is set.
    pub fn heartbeat(&self, who: Identity, beat: Heartbeat) -> Result<Answer, HeartbeatRefused> {
        if self.legacy.is_none() && beat.services.is_none() {
            return Err(HeartbeatRefused);
        }
        let asked = beat.agent_features.clone();
        let reply = self.reply_to(who, beat);
        // Only an agent that announced features gets `features`: an older agent reads the
        // reply strictly and refuses a field it does not know.
        let features: Option<Vec<String>> = (!asked.is_empty()).then(|| {
            self.features
                .iter()
                .filter(|name| asked.contains(name))
                .cloned()
                .collect()
        });
        // The generations only to an agent that announced `halt-v1` and a coordinator that
        // advertises it: an older agent's strict reader refuses the field.
        let halts = features
            .as_ref()
            .filter(|names| names.iter().any(|name| name == halt::FEATURE))
            .map_or_else(BTreeMap::new, |_| {
                halt::generations(&self.ops, &self.services)
            });
        Ok(Answer {
            reply,
            features,
            halts,
        })
    }

    fn reply_to(&self, who: Identity, beat: Heartbeat) -> ServiceReply {
        let now = (self.clock)();
        let legacy = self.legacy.as_deref();
        let announced = beat.services.clone().unwrap_or_default();
        let node = Node {
            role: who.role.name().to_owned(),
            features: beat.agent_features.clone(),
            agent: beat.agent.clone(),
            seen: now,
            announced: legacy
                .map(|name| (name.to_owned(), beat.health))
                .into_iter()
                .chain(announced.iter().map(|one| (one.name.clone(), one.health)))
                .collect(),
        };
        // The halts are noted under the fleet's lock, which every decision and order below is
        // made under: a halt is never raised between another heartbeat's decision to order and
        // its order. The lock order is the fleet's, then the ops store's, as everywhere.
        let mut fleet = self.fleet.lock().unwrap_or_else(PoisonError::into_inner);
        let held = halt::note(&self.ops, &self.services, legacy, &who, &beat, &node, now);
        let platform = beat.platform;
        let ops = self.ops.state();
        let counted =
            |def: &ServiceDef| standing(def, &who.host_id, &node, &ops, now) == Standing::Counted;
        for def in self.services.defs() {
            if standing(def, &who.host_id, &node, &ops, now) == Standing::Dropped {
                fleet.forget_in(&def.name, &who.host_id);
            }
        }
        // A host a service does not count still says what it runs of it: kept as what it
        // occupies, so an update it was ordered before it left holds its slot until it ends.
        let dropped =
            |def: &ServiceDef| standing(def, &who.host_id, &node, &ops, now) == Standing::Dropped;
        if let Some(name) = legacy {
            match self.services.get(name) {
                Some(def) if dropped(def) => {
                    fleet.record_outside_in(name, who.host_id.clone(), &who.role, beat, now);
                }
                top if top.is_none_or(counted) => {
                    fleet.record_in(name, who.host_id.clone(), who.role.clone(), beat, now);
                }
                _ => {}
            }
        }
        for one in &announced {
            match self.services.get(&one.name) {
                Some(def) if counted(def) => fleet.record_in(
                    &one.name,
                    who.host_id.clone(),
                    who.role.clone(),
                    one.as_heartbeat(platform),
                    now,
                ),
                Some(def) if dropped(def) => fleet.record_outside_in(
                    &one.name,
                    who.host_id.clone(),
                    &who.role,
                    one.as_heartbeat(platform),
                    now,
                ),
                _ => {}
            }
        }
        fleet.note_node(who.host_id.clone(), node);
        let mut names: Vec<&str> = legacy
            .into_iter()
            .chain(announced.iter().map(|one| one.name.as_str()))
            .collect();
        // Stable: services of one priority keep the order above.
        names.sort_by_key(|name| Reverse(self.services.get(name).map_or(0, |def| def.priority)));
        // The hosts' words are in: record the last good versions and roll back what failed,
        // before the orders below read the desired states.
        for def in names.iter().filter_map(|name| self.services.get(name)) {
            reconcile(def, &self.ops, &self.desired, &fleet, now);
        }
        let desired = self.desired.all();
        for name in names {
            if held.iter().any(|service| service == name) {
                continue;
            }
            let decision = halt::decide(
                &self.ops,
                &self.services,
                &desired,
                name,
                &fleet,
                &who.host_id,
                now,
            );
            if let Decision::Update { version } = decision {
                // The host's ack goes from the file before the order exists.
                if !release_ack(&self.ops, name, &who.host_id) {
                    continue;
                }
                fleet.order(name, who.host_id.clone(), version.clone(), now);
                return ServiceReply::Update {
                    service: (Some(name) != legacy).then(|| name.to_owned()),
                    version,
                };
            }
        }
        ServiceReply::Nothing
    }

    /// The CI token in `bearer`, checked by the verifier.
    async fn ci(&self, bearer: Option<&str>) -> Result<Verified, CiRefused> {
        let token = bearer
            .filter(|t| t.len() <= MAX_BEARER_BYTES)
            .ok_or(CiRefused::Unauthorized("missing"))?;
        match self.verifier.verify(token).await {
            Ok(verified) => Ok(verified),
            Err(Refusal::KeysUnavailable) => Err(CiRefused::Unavailable),
            Err(refusal) => Err(CiRefused::Unauthorized(refusal.name())),
        }
    }

    /// Sets the desired state from `body`, if `bearer` is a GitHub Actions OIDC token the
    /// allow-list accepts. The token is checked before the body is parsed or any state is touched
    /// (the transport has already read the body, at most [`crate::http::MAX_DESIRED_BYTES`]).
    ///
    /// # Errors
    /// [`SetError`].
    pub async fn set_desired(
        &self,
        bearer: Option<&str>,
        body: &[u8],
    ) -> Result<Desired, SetError> {
        self.set_desired_noting(bearer, body)
            .await
            .map(|set| set.desired)
    }

    /// [`Self::set_desired`], also naming the roles whose limits the body carried and the
    /// service does not have: they are dropped, not refused, since one body serves every
    /// service.
    ///
    /// # Errors
    /// [`SetError`].
    pub async fn set_desired_noting(
        &self,
        bearer: Option<&str>,
        body: &[u8],
    ) -> Result<Set, SetError> {
        let verified = self.ci(bearer).await?;
        let (service, desired, dropped_roles) =
            Desired::parse_request_noting(body, &self.services, self.legacy.as_deref())?;
        // An allow-list entry that names no services stands for the legacy service.
        let admitted = match &verified.services {
            Some(names) => names.contains(&service),
            None => self.legacy.as_deref() == Some(service.as_str()),
        };
        if !admitted {
            tracing::warn!(
                workflow = %verified.job_workflow_ref,
                %service,
                "refused a desired state: the allow-list entry does not name the service"
            );
            return Err(SetError::Forbidden);
        }
        // CI naming a version by hand lifts its mark. The mark goes first: a set that fails
        // after it leaves nothing blocked, and the reverse would leave a wanted version that
        // the next beat rolls back. The fleet's lock is held to the end of the set: a heartbeat
        // plans a rollback and writes it under that lock, so it cannot write a rollback from
        // the state it read over the version CI stores meanwhile.
        {
            let fleet = self.lock_fleet_for_set();
            clear_bad(&self.ops, &fleet, &service, &desired.version)?;
            self.desired.set(&service, desired.clone())?;
        }
        tracing::info!(
            workflow = %verified.job_workflow_ref,
            sha = ?verified.sha,
            %service,
            "desired state set by CI"
        );
        Ok(Set {
            desired,
            dropped_roles,
        })
    }

    /// The name of the operator token in `bearer`: the token is compared by its SHA-256 in
    /// constant time ([`OperatorTokens::name_of`]). CI's token is no operator token and an
    /// operator token is no CI token.
    ///
    /// # Errors
    /// [`Unauthorized`]: no bearer, or not a token.
    pub fn operator(&self, bearer: Option<&str>) -> Result<TokenName, Unauthorized> {
        bearer
            .and_then(|presented| self.tokens.name_of(presented))
            .ok_or(Unauthorized)
    }

    /// Does the operator action in `body` (`docs/design/host-updates.md`, section 5), if
    /// `bearer` is an operator token, and answers the audit entry it made, whose `by` is the
    /// token's name. The token is checked here before the body is looked at; the transport
    /// checks it ([`Self::operator`]) before it reads the body at all.
    ///
    /// # Errors
    /// [`OperateError`].
    pub fn operate(&self, bearer: Option<&str>, body: &[u8]) -> Result<AuditEntry, OperateError> {
        let by = self.operator(bearer)?;
        let entry = ops::operate(&self.ops, &self.services, by.as_str(), body)?;
        self.reconcile();
        Ok(entry)
    }

    /// Stops counting the hosts of a service they no longer belong to (what they said is kept
    /// only as what they occupy, [`Fleet::forget_in`]), after an
    /// operator changed exclusions or labels ([`standing`]).
    fn reconcile(&self) {
        let now = (self.clock)();
        let ops = self.ops.state();
        let mut fleet = self.fleet.lock().unwrap_or_else(PoisonError::into_inner);
        let dropped: Vec<(String, hostkeeper_wire::HostId)> = self
            .services
            .defs()
            .flat_map(|def| {
                fleet
                    .nodes()
                    .filter(|(id, node)| standing(def, id, node, &ops, now) == Standing::Dropped)
                    .map(|(id, _)| (def.name.clone(), id.clone()))
                    .collect::<Vec<_>>()
            })
            .collect();
        for (service, id) in dropped {
            fleet.forget_in(&service, &id);
        }
    }

    /// The status, if `bearer` is the operator token.
    ///
    /// # Errors
    /// [`Unauthorized`].
    pub fn status(&self, bearer: Option<&str>) -> Result<Status, Unauthorized> {
        self.operator(bearer)?;
        let now = (self.clock)();
        let fleet = self.fleet.lock().unwrap_or_else(PoisonError::into_inner);
        let hosts = self
            .legacy
            .iter()
            .flat_map(|name| {
                fleet
                    .hosts_of(name)
                    .map(|(id, host)| host_status(&fleet, name, id, host, now))
                    .collect::<Vec<_>>()
            })
            .collect();
        let all = self.desired.all();
        let ops = self.ops.state();
        let per_service = self
            .services
            .defs()
            .map(|def| {
                let desired = self.desired.get(&def.name);
                let roles = role_counts(def, desired.as_ref(), &fleet, now);
                let hosts: Vec<ServiceHostStatus> = fleet
                    .hosts_of(&def.name)
                    .map(|(id, host)| ServiceHostStatus {
                        host: host_status(&fleet, &def.name, id, host, now),
                        hold: halt::decide(
                            &self.ops,
                            &self.services,
                            &all,
                            &def.name,
                            &fleet,
                            id,
                            now,
                        )
                        .why(),
                    })
                    .collect();
                let up = hosts.iter().filter(|h| h.host.up).count();
                let slots_held = hosts.iter().filter(|h| h.host.holds_slot).count();
                let members = members(def, &fleet, &ops, now);
                let convergence = convergence_of(def, desired.as_ref(), &fleet, &ops, now);
                let status = ServiceStatus {
                    desired,
                    roles,
                    hosts,
                    members,
                    up,
                    slots_held,
                    convergence,
                };
                (def.name.clone(), status)
            })
            .collect();
        Ok(Status {
            per_service,
            ops,
            desired: self
                .legacy
                .as_deref()
                .and_then(|name| self.desired.get(name)),
            services: self.desired.all(),
            quiet: fleet.is_quiet(now),
            services_dropped: self.services_dropped.load(Ordering::Relaxed),
            extras_dropped: self
                .extras
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .counts
                .clone(),
            hosts,
            commit: crate::BUILD_COMMIT,
        })
    }

    /// Where the hosts of `service` stand against its desired state, if `bearer` is a
    /// GitHub Actions OIDC token the same allow-list accepts as for [`Self::set_desired`]
    /// (any service may be read with any accepted token). The operator token is no CI token
    /// and is refused. Read-only, and each token is good once.
    ///
    /// # Errors
    /// [`ConvergenceError`].
    pub async fn convergence(
        &self,
        bearer: Option<&str>,
        service: &str,
    ) -> Result<Convergence, ConvergenceError> {
        self.ci(bearer).await?;
        let def = self
            .services
            .get(service)
            .ok_or(ConvergenceError::UnknownService)?;
        let now = (self.clock)();
        let fleet = self.fleet.lock().unwrap_or_else(PoisonError::into_inner);
        let desired = self.desired.get(service);
        let ops = self.ops.state();
        Ok(convergence_of(def, desired.as_ref(), &fleet, &ops, now))
    }
}

/// The host `id` as the status shows it for `service`: whether it is up and holds a slot
/// counted for that service.
fn host_status(
    fleet: &Fleet,
    service: &str,
    id: &hostkeeper_wire::HostId,
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
    let convergence = account(convergence, def, &members(def, fleet, ops, now), deferred);
    match halt::standing(ops, &def.name) {
        Some(halt) => convergence.halted(halt),
        None => convergence,
    }
}
