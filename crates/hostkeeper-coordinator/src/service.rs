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
use hostkeeper_wire::wire::{Answer, Heartbeat, ORDER_FEATURE, ServiceReply};
use serde::Serialize;

use crate::acks::release_ack;
use crate::converge::{Convergence, role_counts};
use crate::decide::Decision;
use crate::desired::{Desired, DesiredError, DesiredStore, StoreError};
use crate::enrollment::Enrollment;
use crate::fleet::{Fleet, Node, QUIET_START};
use crate::halt;
use crate::holds::{alert_on_bad_version, marks_of, on_bad_version};
use crate::identity::Identity;
use crate::membership::{Standing, members, standing};
use crate::ops::{self, AuditEntry, OperateError, OpsError, OpsStore};
use crate::order_terms::{self, Terms};
use crate::orders::Order;
use crate::reconcile::reconcile;
use crate::services::{ServiceDef, Services};
use crate::status::host_status;
use crate::tokens::{OperatorTokens, TokenName};
use crate::ui::UiDir;
use enrollment_calls::Turn;

pub use crate::status::{AgentStatus, HostStatus, ServiceHostStatus, ServiceStatus, Status};

pub(crate) use crate::status::convergence_of;

/// The time since the coordinator started.
pub type Clock = Arc<dyn Fn() -> Duration + Send + Sync>;

/// The most bytes of a bearer token looked at.
pub const MAX_BEARER_BYTES: usize = 8192;

/// Why a heartbeat was refused: it lists no services and no legacy service is set, so it
/// says nothing the coordinator can attribute to a service.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("it lists no services and no legacy service is set")]
pub struct HeartbeatRefused;

/// Why the desired state was not set. Every refusal is decided before anything changes: a
/// refused set leaves the state file, the ops file and memory as they were.
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
    /// The ops file could not be written: the desired state is set, and the halt the updater
    /// raised, which the set lifts, stands. Setting the same state again is safe and tries the
    /// lift again.
    #[error(transparent)]
    Ops(#[from] OpsError),
    /// The allow-list entry that admitted the token does not name this service.
    #[error("the token may not set this service")]
    Forbidden,
    /// The version is below the service's `min_version`.
    #[error("below_min_version: the version is below the service's min_version")]
    BelowMinVersion,
    /// The service's `version_scheme` cannot order the version.
    #[error("unparseable_version: the service's version scheme cannot read the version")]
    UnparseableVersion,
    /// The version is marked bad: only the operator's unmark or a force lifts the mark.
    #[error("bad_version: the version is marked bad")]
    BadVersion,
    /// CI set the version before with another package digest: a changed artifact for a version.
    #[error("digest_conflict: the version was set before with another package digest")]
    DigestConflict,
}

impl SetError {
    /// The closed reason token of a refusal of the version itself (`below_min_version`,
    /// `unparseable_version`, `bad_version`, `digest_conflict`), which the answer says; `None`
    /// for every other error.
    #[must_use]
    pub const fn reason(&self) -> Option<&'static str> {
        match self {
            Self::BelowMinVersion => Some("below_min_version"),
            Self::UnparseableVersion => Some("unparseable_version"),
            Self::BadVersion => Some("bad_version"),
            Self::DigestConflict => Some("digest_conflict"),
            Self::Unauthorized(_)
            | Self::Unavailable
            | Self::Invalid(_)
            | Self::Store(_)
            | Self::Ops(_)
            | Self::Forbidden => None,
        }
    }
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

/// The features this coordinator advertises to an agent that announces `agent_features`.
///
/// Each name is added in the change that implements what it names (`halt-v1`; the others
/// follow), so a name is never advertised for behaviour that is not there. `deferred-v1`: a
/// heartbeat's `deferred` report ends the order, holds no slot and backs the host off
/// ([`crate::defer`]). `halt-v1`: the clear generations of [`crate::halt`]. `heartbeat-v2`:
/// selector membership, hosts that are `missing`, `absent` or `unmanaged`
/// ([`crate::membership`]). `order-v1`: an update answer carries the order terms of
/// [`crate::order_terms`], and the heartbeat's top level is the agent itself, never the legacy
/// service. `grpc-v1`: the coordinator serves `hostkeeper.agent.v1.Coordinator` on the heartbeat
/// listener ([`crate::grpc`]), so an agent that announced it may send its next heartbeat there.
pub const FEATURES: &[&str] = &[
    "deferred-v1",
    halt::FEATURE,
    crate::membership::HEARTBEAT_V2,
    ORDER_FEATURE,
    crate::grpc::GRPC_FEATURE,
];

/// The fields of the answer to a heartbeat that go only to an agent that announced a feature,
/// each with the feature: the one place that says which. An older agent reads the answer
/// strictly and refuses a field it does not know, so a field listed here is left out of the
/// answer to an agent that did not announce its feature, and to every agent while this
/// coordinator does not advertise it ([`FEATURES`]). A field not listed is sent to every agent.
pub const GATED_FIELDS: &[(&str, &str)] = &[
    ("halts", halt::FEATURE),
    ("order_id", ORDER_FEATURE),
    ("order_try", ORDER_FEATURE),
    ("package_digest", ORDER_FEATURE),
];

/// Whether the answer field `field` goes to an agent whose announced features this coordinator
/// also advertises are `agreed` (none for an agent that announced none): yes for a field
/// [`GATED_FIELDS`] does not list, and for one it lists only when its feature is agreed.
#[must_use]
pub fn sends(field: &str, agreed: Option<&[String]>) -> bool {
    GATED_FIELDS
        .iter()
        .find(|(name, _)| *name == field)
        .is_none_or(|(_, feature)| agreed.is_some_and(|names| names.iter().any(|n| n == feature)))
}

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
    pub(crate) fleet: Mutex<Fleet>,
    pub(crate) desired: DesiredStore,
    pub(crate) services: Services,
    legacy: Option<String>,
    prefixes: SanPrefixes,
    pub(crate) ops: OpsStore,
    verifier: Arc<Verifier>,
    tokens: OperatorTokens,
    pub(crate) clock: Clock,
    services_dropped: AtomicU64,
    extras: Mutex<ExtrasDropped>,
    features: Vec<String>,
    /// Enrollment: the store, the CA and the poll nonces; `None` without `--enrollment-dir`.
    enrollment: Option<Arc<Enrollment>>,
    ui: Option<UiDir>,
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
            fleet: Mutex::new(Fleet::new(started, QUIET_START)),
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
            enrollment: None,
            ui: None,
            #[cfg(test)]
            set_waits: Mutex::new(None),
        }
    }

    /// The same coordinator ordering nothing for `quiet` after its start instead of
    /// [`QUIET_START`] (`--quiet-secs`, whose floor [`crate::config::build`] checks).
    #[must_use]
    pub fn with_quiet(mut self, quiet: Duration) -> Self {
        let fleet = self.fleet.get_mut().unwrap_or_else(PoisonError::into_inner);
        *fleet = Fleet::new(fleet.started(), quiet);
        self
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

    /// The same coordinator serving the static files of `ui` on the private surface, or none.
    #[must_use]
    pub fn with_ui(mut self, ui: Option<UiDir>) -> Self {
        self.ui = ui;
        self
    }

    /// The directory whose files the private surface serves under `/ui/`, if there is one.
    #[must_use]
    pub fn ui(&self) -> Option<&UiDir> {
        self.ui.as_ref()
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
        self.lock_fleet()
    }

    /// The fleet's lock, which every decision and order is made under, with the operator's
    /// marks the decisions read refreshed from the ops store ([`crate::holds::marks_of`]). The
    /// lock order is the fleet's, then the ops store's.
    pub(crate) fn lock_fleet(&self) -> std::sync::MutexGuard<'_, Fleet> {
        let mut fleet = self.fleet.lock().unwrap_or_else(PoisonError::into_inner);
        fleet.set_marks(self.ops.read(marks_of));
        fleet
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

    /// The legacy service for the heartbeat `beat`: the coordinator's, except for an agent that
    /// announced [`ORDER_FEATURE`], whose top level is the agent itself and which names every
    /// service in `services`.
    fn legacy_for(&self, beat: &Heartbeat) -> Option<&str> {
        if beat.agent_features.iter().any(|name| name == ORDER_FEATURE) {
            None
        } else {
            self.legacy.as_deref()
        }
    }

    /// Notes a heartbeat from `who` and answers it. With no desired state set, nothing.
    ///
    /// The top-level fields are the legacy service's, when one is set; without one they
    /// belong to no service and only the entries of `services` count. For an agent that
    /// announced [`ORDER_FEATURE`] no legacy service is set, whatever the coordinator's: its
    /// top level is the agent itself and every update names its service. Such an agent is
    /// ordered a version only if CI registered a package digest for it, and the update then
    /// carries the order terms of [`crate::order_terms`]; a version with no digest is not
    /// ordered to it, and takes no slot. Each entry of `services`
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
        self.answer(who, beat, false, None)
            .map(|(answer, _)| answer)
    }

    /// [`Self::heartbeat`] for a transport that carries typed orders (the `Order` of
    /// `hostkeeper.agent.v1`): with the answer, the operator's order for the host when one is
    /// due ([`crate::orders`]: an `abort` queued for it, or the `force` of a standing force),
    /// which is sent instead of an update, and the answer's reply is then `nothing`. When the
    /// rollout gate orders an update with its terms (an agent that announced `order-v1`, a
    /// registered digest), the update is the typed order ([`Order::Update`]): the order id, try
    /// and package digest of the answer's terms, which the answer carries too.
    ///
    /// # Errors
    /// [`HeartbeatRefused`], as for [`Self::heartbeat`].
    pub fn heartbeat_with_orders(
        &self,
        who: Identity,
        beat: Heartbeat,
    ) -> Result<(Answer, Option<Order>), HeartbeatRefused> {
        self.answer(who, beat, true, None)
    }

    fn answer(
        &self,
        who: Identity,
        beat: Heartbeat,
        orders: bool,
        machine_id: Option<&str>,
    ) -> Result<(Answer, Option<Order>), HeartbeatRefused> {
        if self.legacy_for(&beat).is_none() && beat.services.is_none() {
            return Err(HeartbeatRefused);
        }
        let turn = self.turn_of(&who.host_id, machine_id);
        let asked = beat.agent_features.clone();
        let (reply, order, terms) = self.reply_to(who, beat, orders, turn);
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
        let halts = if sends("halts", features.as_deref()) {
            halt::generations(&self.ops, &self.services)
        } else {
            BTreeMap::new()
        };
        // The terms only to an agent that announced `order-v1` and a coordinator that
        // advertises it.
        let terms = terms.filter(|_| sends("order_id", features.as_deref()));
        // On a transport that carries typed orders, the update is one too, with the same terms.
        let order = order.or_else(|| order_terms::typed(&reply, terms.as_ref()).filter(|_| orders));
        let answer = Answer {
            reply,
            order_id: terms.as_ref().map(|t| t.order_id.clone()),
            order_try: terms.as_ref().map(|t| t.order_try),
            package_digest: terms.map(|t| t.package_digest),
            features,
            halts,
        };
        Ok((answer, order))
    }

    fn reply_to(
        &self,
        who: Identity,
        beat: Heartbeat,
        orders: bool,
        turn: Turn,
    ) -> (ServiceReply, Option<Order>, Option<Terms>) {
        let now = (self.clock)();
        let legacy = self.legacy_for(&beat);
        let order_v1 = self.features.iter().any(|name| name == ORDER_FEATURE)
            && beat.agent_features.iter().any(|name| name == ORDER_FEATURE);
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
        let mut fleet = self.lock_fleet();
        // A halt whose write failed is written again at every heartbeat until it lands.
        self.ops.retry_unwritten(now);
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
        if turn == Turn::Hold {
            return (ServiceReply::Nothing, None, None);
        }
        let mut names: Vec<&str> = legacy
            .into_iter()
            .chain(announced.iter().map(|one| one.name.as_str()))
            .collect();
        // Stable: services of one priority keep the order above.
        names.sort_by_key(|name| Reverse(self.services.get(name).map_or(0, |def| def.priority)));
        // The hosts' words are in: record the last good versions and roll back what failed,
        // before the orders below read the desired states.
        for def in names.iter().filter_map(|name| self.services.get(name)) {
            // Only the verdicts about the desired version count, and not those an override
            // acknowledged; the others are forgotten.
            if let Some(want) = self.desired.get(&def.name) {
                let acked = self.ops.marks(&def.name).failure_acked;
                fleet.keep_verdicts_of(&def.name, &want.version, &acked);
            }
            reconcile(def, &self.ops, &self.desired, &fleet, now);
            self.ops.read(|state| {
                alert_on_bad_version(&mut fleet, state, &def.name, &who.host_id);
            });
        }
        // The operator's orders go first, past the gate, to a transport that carries them.
        if orders
            && let Some(order) = self
                .ops
                .read(|state| crate::orders::next(&mut fleet, state, &who.host_id, &names))
        {
            return (ServiceReply::Nothing, Some(order), None);
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
                // An agent that reads order terms is ordered only a version with a digest.
                let terms = order_v1.then(|| {
                    let digests = self.desired.digests(name);
                    order_terms::of_beat(&digests, name, &version, &announced, &who.host_id)
                });
                let Some(terms) = terms.map_or(Some(None), |made| made.map(Some)) else {
                    continue;
                };
                // The host's ack goes from the file before the order exists.
                if !release_ack(&self.ops, name, &who.host_id) {
                    continue;
                }
                fleet.order(name, who.host_id.clone(), version.clone(), now);
                let service = (Some(name) != legacy).then(|| name.to_owned());
                return (ServiceReply::Update { service, version }, None, terms);
            }
        }
        (ServiceReply::Nothing, None, None)
    }

    /// The CI token in `bearer`, checked by the verifier.
    pub(crate) async fn ci(&self, bearer: Option<&str>) -> Result<Verified, CiRefused> {
        let token = bearer
            .filter(|t| t.len() <= MAX_BEARER_BYTES)
            .ok_or(CiRefused::Unauthorized("missing"))?;
        match self.verifier.verify(token).await {
            Ok(verified) => Ok(verified),
            Err(Refusal::KeysUnavailable | Refusal::ReplayCacheUnavailable) => {
                Err(CiRefused::Unavailable)
            }
            Err(refusal) => Err(CiRefused::Unauthorized(refusal.name())),
        }
    }

    /// Sets the desired state from `body`, if `bearer` is a GitHub Actions OIDC token the
    /// allow-list accepts. The token is checked before the body is parsed or any state is touched
    /// (the transport has already read the body, at most [`crate::http::MAX_DESIRED_BYTES`]).
    ///
    /// The version is refused, before anything changes, when it is below the service's
    /// `min_version`, when the service's `version_scheme` cannot read it, when it is marked bad
    /// (only the operator's unmark or a force lifts a mark; CI does not), and when CI set it
    /// before with another package digest. A body that names a digest (`release_digest`) records
    /// the version and the digest in the service's registry in the same write as the state
    /// ([`DesiredStore::record`]); setting the same version with the same digest again changes
    /// nothing. A set of a version other than the target of a halt the updater raised lifts that
    /// halt ([`halt::lift_by_ci`]); a hook's halt stays.
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
        self.set_desired_verified(&verified, body)
    }

    /// Why CI may not set `desired` for `service`, if it may not: the refusals of
    /// [`Self::set_desired`], in that order.
    fn refuse(&self, def: &ServiceDef, desired: &Desired) -> Result<(), SetError> {
        let (service, version) = (def.name.as_str(), &desired.version);
        if !def.version_scheme.parses(version) {
            return Err(SetError::UnparseableVersion);
        }
        if def
            .min_version
            .as_ref()
            .is_some_and(|floor| !version.at_least(floor))
        {
            return Err(SetError::BelowMinVersion);
        }
        if self.ops.marks(service).bad.contains(version) {
            return Err(SetError::BadVersion);
        }
        let registered = self.desired.digests(service).get(version).cloned();
        if let (Some(before), Some(now)) = (registered, desired.release_digest.as_ref())
            && before != *now
        {
            return Err(SetError::DigestConflict);
        }
        Ok(())
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
        let mut fleet = self.lock_fleet();
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
        let fleet = self.lock_fleet();
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
                        held_on_bad_version: on_bad_version(&fleet, &ops, &def.name, id),
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
        self.convergence_verified(service)
    }
}

/// Whether the registry of digests keeps `version` at its cap, whatever its age: the service's
/// last good version and the versions marked bad, which `marks` holds ([`crate::registry`]).
pub(crate) fn kept(marks: &crate::ops::ServiceOps, version: &Version) -> bool {
    marks.last_good.as_ref() == Some(version) || marks.bad.contains(version)
}

/// The calls of the heartbeat path that ask enrollment.
#[path = "service_enrollment.rs"]
mod enrollment_calls;

/// The calls that act for a CI token already checked.
#[path = "service_ci.rs"]
mod ci_calls;
