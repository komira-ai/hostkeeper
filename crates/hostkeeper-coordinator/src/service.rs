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
use hostkeeper_wire::wire::{Answer, Heartbeat, ORDER_FEATURE, ServiceReply, VerdictAck};
use serde::Serialize;

use crate::accounts::AccountStore;
use crate::desired::{Desired, DesiredError, DesiredStore, StoreError};
use crate::enrollment::Enrollment;
use crate::fleet::{Fleet, Node, QUIET_START};
use crate::group_scope::Basis;
use crate::halt;
use crate::identity::Identity;
use crate::layout::LayoutStore;
use crate::membership::{Standing, standing};
use crate::ops::{AuditEntry, OperateError, OpsError, OpsStore};
use crate::order_terms::Terms;
use crate::orders::Order;
use crate::reconcile::reconcile;
use crate::services::{ServiceDef, Services};
use crate::status::host_status;
use crate::tokens::OperatorTokens;
use crate::ui::UiDir;
use crate::verdict_file::{self, VerdictFile};
use enrollment_calls::Turn;
use gate_orders::Asked;

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
    /// The allow-list entry that admitted the token does not name this service, or names
    /// `host_groups` that exclude where the set would deploy.
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
    /// The set's targets break a rule of the release ([`hostkeeper_release::target`]).
    #[error("target_invalid: {0}")]
    TargetInvalid(hostkeeper_release::TargetError),
    /// The version was set before as another release ([`crate::releases`]).
    #[error("release_exists: the version was set before as another release")]
    ReleaseExists,
    /// A repository entry names a repository the registry does not hold ([`crate::repositories`]).
    #[error("repository_unknown: a repository entry names a repository the registry lacks")]
    RepositoryUnknown,
    /// The set names a host group that does not exist or does not list the package, or names none
    /// and no group lists it.
    #[error("not_in_host_group: no such group lists the package")]
    NotInHostGroup,
    /// The set names `if_generation` and no group, and more than one group lists the package.
    #[error("ambiguous_host_group: more than one group lists the package")]
    AmbiguousHostGroup,
    /// The set carries `max_updating` or `min_up`, which are a group's policy.
    #[error("limits_in_host_group: the limits are the group's policy")]
    LimitsInHostGroup,
}

impl SetError {
    /// The closed reason token of a refusal of the version itself (`below_min_version`,
    /// `unparseable_version`, `bad_version`, `digest_conflict`, `release_exists`,
    /// `repository_unknown`), of its release index (`index_invalid`, `index_digest_mismatch`), of
    /// its targets (`target_invalid`) or of its host group (`not_in_host_group`,
    /// `ambiguous_host_group`, `limits_in_host_group`), which the answer says; `None` for every
    /// other error.
    #[must_use]
    pub const fn reason(&self) -> Option<&'static str> {
        match self {
            Self::Invalid(why) => why.reason(),
            Self::BelowMinVersion => Some("below_min_version"),
            Self::UnparseableVersion => Some("unparseable_version"),
            Self::BadVersion => Some("bad_version"),
            Self::DigestConflict => Some("digest_conflict"),
            Self::TargetInvalid(_) => Some("target_invalid"),
            Self::ReleaseExists => Some("release_exists"),
            Self::RepositoryUnknown => Some("repository_unknown"),
            Self::NotInHostGroup => Some("not_in_host_group"),
            Self::AmbiguousHostGroup => Some("ambiguous_host_group"),
            Self::LimitsInHostGroup => Some("limits_in_host_group"),
            Self::Unauthorized(_)
            | Self::Unavailable
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
    /// The host group named is not held or does not list the service.
    #[error("the service is not in the host group")]
    NotInHostGroup,
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

pub use crate::features::FEATURES;

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
    ("acks", verdict_file::FEATURE),
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
    /// Shared with enrollment, which places each approved host by its host groups.
    pub(crate) desired: Arc<DesiredStore>,
    pub(crate) services: Services,
    legacy: Option<String>,
    prefixes: SanPrefixes,
    /// Shared with enrollment, which audits its approvals there.
    pub(crate) ops: Arc<OpsStore>,
    pub(crate) verdicts: VerdictFile,
    accounts: AccountStore,
    layout: LayoutStore,
    /// The initial-admin file of `--initial-admin-file` ([`crate::initial_admin`]).
    pub(crate) initial_admin: Option<crate::initial_admin::InitialAdmin>,
    /// The sessions, the limiter and the passwords of the sign-in ([`crate::login`]); none: the
    /// sign-in routes are not served and no cookie is read.
    login: Option<crate::login::Login>,
    verifier: Arc<Verifier>,
    /// The trusted publishers the allow-list file stands for, per package
    /// ([`crate::trusted_publishers`]): a package's entries until the state holds its own.
    pub(crate) listed_publishers:
        std::collections::BTreeMap<String, Vec<crate::trusted_publishers::TrustedPublisher>>,
    pub(crate) tokens: OperatorTokens,
    pub(crate) clock: Clock,
    services_dropped: AtomicU64,
    extras: Mutex<ExtrasDropped>,
    pub(crate) features: Vec<String>,
    /// Enrollment: the store, the CA and the poll nonces; `None` without `--enrollment-dir`.
    enrollment: Option<Arc<Enrollment>>,
    /// The MDM policy gate; `None` without `--mdm-api-url`.
    pub(crate) mdm: Option<Arc<crate::mdm::Mdm>>,
    /// The MDM server of the Macs' listener (`--mdm-listen`); `None` without it.
    pub(crate) mdm_server: Option<Arc<crate::mdm_server::MdmServer>>,
    /// The MDM setup: the sealed secrets and the settings ([`crate::mdm_setup`]).
    pub(crate) mdm_setup: Arc<crate::mdm_setup::MdmSetup>,
    /// The ABM client ([`crate::abm`]); `None` in a coordinator built without an MDM store.
    pub(crate) abm: Option<Arc<crate::abm::Abm>>,
    /// The package proxy: its cache (`--proxy-cache-dir`) and its reads of public origins.
    pub(crate) proxy: crate::proxy_origin::Proxy,
    /// Where a push certificate's request is posted to mdmcert.download ([`crate::mdm_push`]).
    pub(crate) mdmcert: Arc<dyn hostkeeper_mdm_push::mdmcert::MdmcertTransport>,
    ui: Option<UiDir>,
    /// The alerts the last observation found ([`crate::alerts`]).
    pub(crate) alerts: crate::alerts::Book,
    /// The held `Releases.WaitConverged` calls ([`crate::wait`]).
    pub(crate) waits: crate::wait::Waits,
    /// What each host's daemon last reported of its repositories ([`crate::repository_push`]).
    pub(crate) repository_reports: crate::repository_push::Reports,
    /// What each host heard runs ([`crate::running`]).
    pub(crate) running: crate::running::Kept,
    /// Reads private repositories' origins ([`crate::repository_serve`]); none: those reads are 502.
    pub(crate) repository_origin: Option<crate::repository_origin::RepositoryOrigin>,
    /// Held by `Services.SetRepository` from its check that the registry holds the repository to
    /// its write, and by `Repositories.Remove` from its check that no service names the
    /// repository to its commit, so neither decides on what the other is changing.
    pub(crate) repository_naming: Mutex<()>,
    /// Held by `Mdm.GetAbmPublicKey` from its read of the held pair to its write: one pair is made.
    pub(crate) abm_pairing: tokio::sync::Mutex<()>,
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
        Self {
            fleet: Mutex::new(Fleet::new(clock(), QUIET_START)),
            desired: Arc::new(desired),
            services: Services::default(),
            legacy: None,
            prefixes: SanPrefixes::default(),
            ops: Arc::new(OpsStore::volatile()),
            verdicts: VerdictFile::volatile(),
            accounts: AccountStore::volatile(),
            layout: LayoutStore::volatile(),
            mdm_server: None,
            mdm_setup: Arc::new(crate::mdm_setup::MdmSetup::volatile()),
            abm: None,
            proxy: crate::proxy_origin::Proxy::default(),
            mdmcert: Arc::new(crate::mdm_push::NoMdmcert),
            initial_admin: None,
            login: None,
            verifier,
            listed_publishers: std::collections::BTreeMap::new(),
            tokens: operator.into(),
            clock,
            services_dropped: AtomicU64::new(0),
            extras: Mutex::new(ExtrasDropped::default()),
            features: FEATURES.iter().map(|name| (*name).to_owned()).collect(),
            enrollment: None,
            mdm: None,
            ui: None,
            alerts: crate::alerts::Book::default(),
            waits: crate::wait::Waits::default(),
            repository_reports: crate::repository_push::Reports::default(),
            running: crate::running::Kept::default(),
            repository_origin: None,
            repository_naming: Mutex::new(()),
            abm_pairing: tokio::sync::Mutex::new(()),
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

    /// The same coordinator whose packages' trusted publishers are, until the state holds a
    /// package's own, those the allow-list `list` stands for (`legacy` for an entry that names no
    /// service): [`crate::trusted_publishers::import`].
    #[must_use]
    pub fn with_allow_list(
        mut self,
        list: &hostkeeper_oidc::AllowList,
        legacy: Option<&str>,
    ) -> Self {
        self.listed_publishers = crate::trusted_publishers::import(list, legacy);
        self
    }

    /// The same coordinator over `ops` instead of a store that keeps nothing.
    #[must_use]
    pub fn with_ops(mut self, ops: OpsStore) -> Self {
        self.ops = Arc::new(ops);
        if let Some(enrollment) = &self.enrollment {
            enrollment.audit_to(Arc::clone(&self.ops));
        }
        self
    }

    /// The same coordinator over the verdicts of `verdicts` instead of a store that keeps
    /// nothing: those it counted before its restart count again.
    #[must_use]
    pub fn with_verdicts(mut self, verdicts: VerdictFile) -> Self {
        let fleet = self.fleet.get_mut().unwrap_or_else(PoisonError::into_inner);
        for (service, host, report) in verdicts.counted() {
            fleet.restore_verdict(&service, host, report);
        }
        self.verdicts = verdicts;
        self
    }

    /// The same coordinator over the users and API tokens of `accounts` instead of a store that
    /// keeps nothing.
    #[must_use]
    pub fn with_accounts(mut self, accounts: AccountStore) -> Self {
        self.accounts = accounts;
        self
    }

    /// The same coordinator over the lab layout of `layout` instead of a store that keeps
    /// nothing.
    #[must_use]
    pub fn with_layout(mut self, layout: LayoutStore) -> Self {
        self.layout = layout;
        self
    }

    /// The same coordinator looking at `initial_admin` ([`Coordinator::look_at_initial_admin`]).
    #[must_use]
    pub fn with_initial_admin(mut self, initial_admin: crate::initial_admin::InitialAdmin) -> Self {
        self.initial_admin = Some(initial_admin);
        self
    }

    /// The same coordinator serving the sign-in routes and reading the session cookie of the
    /// JSON gateway, over `login`.
    #[must_use]
    pub fn with_login(mut self, login: crate::login::Login) -> Self {
        self.login = Some(login);
        self
    }

    /// The sign-in's sessions, limiter and passwords, if the coordinator has them.
    #[must_use]
    pub fn login(&self) -> Option<&crate::login::Login> {
        self.login.as_ref()
    }

    /// The operator's users and API tokens the store keeps.
    #[must_use]
    pub fn accounts(&self) -> &AccountStore {
        &self.accounts
    }

    /// The lab layout the store keeps.
    #[must_use]
    pub fn layout(&self) -> &LayoutStore {
        &self.layout
    }

    /// The same coordinator advertising `features` instead of [`FEATURES`].
    #[cfg(test)]
    #[must_use]
    pub fn with_features(mut self, features: &[&str]) -> Self {
        self.features = features.iter().map(|name| (*name).to_owned()).collect();
        self
    }

    /// The fleet's lock, which every decision and order is made under, with the operator's
    /// marks the decisions read refreshed from the ops store ([`crate::holds::marks_of`]). The
    /// lock order is the fleet's, then the ops store's.
    pub(crate) fn lock_fleet(&self) -> std::sync::MutexGuard<'_, Fleet> {
        let mut fleet = self.fleet.lock().unwrap_or_else(PoisonError::into_inner);
        self.refresh_marks(&mut fleet);
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
    /// top level is the agent itself and every update names its service. An update is ordered
    /// over a transport that carries typed orders alone, with the entry picked for the host
    /// ([`crate::picks`]) and the order terms of [`crate::order_terms`]; a heartbeat over the
    /// JSON route is never ordered one. Each entry of `services`
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
        self.heartbeat_from(who, beat, None)
    }

    /// [`Self::heartbeat`] for a transport that carries typed orders (the `Order` of
    /// `hostkeeper.agent.v1`): with the answer, the operator's order for the host when one is
    /// due ([`crate::orders`]: an `abort` queued for it, or the `force` of a standing force),
    /// which is sent instead of an update, and the answer's reply is then `nothing`. When the
    /// rollout gate orders an update with its terms (an entry picked for the host), the update is
    /// the typed order ([`Order::Update`]): the order id, try and package digest of the answer's
    /// terms, which the answer carries too, and `root` is kept.
    ///
    /// # Errors
    /// [`HeartbeatRefused`], as for [`Self::heartbeat`].
    pub fn heartbeat_with_orders(
        &self,
        who: Identity,
        beat: Heartbeat,
        root: crate::trusted_root::TrustedRootSaid,
    ) -> Result<(Answer, Option<Order>), HeartbeatRefused> {
        self.answer(who, beat, true, None, root)
    }

    pub(crate) fn answer(
        &self,
        who: Identity,
        beat: Heartbeat,
        orders: bool,
        machine_id: Option<&str>,
        root: crate::trusted_root::TrustedRootSaid,
    ) -> Result<(Answer, Option<Order>), HeartbeatRefused> {
        if self.legacy_for(&beat).is_none() && beat.services.is_none() {
            return Err(HeartbeatRefused);
        }
        let turn = self.turn_of(&who.host_id, machine_id);
        let asked = beat.agent_features.clone();
        // The fleet's lock is taken before the host's group is read: a `HostGroups.Move` of the
        // host runs under that lock, so the group this heartbeat is decided over is never one
        // the host has been moved out of since it was read.
        let fleet = self.lock_fleet();
        let basis = self.basis_of(&who.host_id);
        let (reply, order, terms, acks) =
            self.reply_to(fleet, who, beat, orders, (turn, root), &basis);
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
            halt::generations(&self.ops, &basis.services)
        } else {
            BTreeMap::new()
        };
        // The terms only to an agent that announced `order-v1` and a coordinator that
        // advertises it.
        let terms = terms.filter(|_| sends("order_id", features.as_deref()));
        let acks = if sends("acks", features.as_deref()) {
            acks
        } else {
            Vec::new()
        };
        let answer = Answer {
            reply,
            order_id: terms.as_ref().map(|t| t.order_id.clone()),
            order_try: terms.as_ref().map(|t| t.order_try),
            package_digest: terms.map(|t| t.package_digest),
            features,
            halts,
            acks,
        };
        Ok((answer, order))
    }

    fn reply_to(
        &self,
        mut fleet: std::sync::MutexGuard<'_, Fleet>,
        who: Identity,
        mut beat: Heartbeat,
        orders: bool,
        (turn, trusted_root): (Turn, crate::trusted_root::TrustedRootSaid),
        basis: &Basis<'_>,
    ) -> (ServiceReply, Option<Order>, Option<Terms>, Vec<VerdictAck>) {
        let now = (self.clock)();
        let own = crate::agent_service::own_beat(&basis.services, &mut beat);
        let role = basis.role_of(&who);
        let legacy = self.legacy_for(&beat);
        let announced = beat.services.clone().unwrap_or_default();
        let node = Node {
            role: role.clone(),
            features: beat.agent_features.clone(),
            agent: beat.agent.clone(),
            platform: beat.platform,
            seen: now,
            announced: legacy
                .map(|name| (name.to_owned(), beat.health))
                .into_iter()
                .chain(announced.iter().map(|one| (one.name.clone(), one.health)))
                .collect(),
            sent_unix: beat.sent_unix,
            cert_not_after_unix: beat.cert_not_after_unix,
            last_renew_error: beat.last_renew_error.clone(),
            trusted_root,
            skewed_beats: 0,
        };
        // `fleet` is the fleet's lock, taken by the caller before the basis was read: every
        // decision and order below is made under it, so a halt is never raised between another
        // heartbeat's decision to order and its order. The lock order is the fleet's, then the
        // ops store's, as everywhere.
        // An agent acknowledged its verdicts is read as still reporting the last one, so every
        // rule below reads what it read while the agent resent it.
        let acking =
            verdict_file::asks(&beat) && self.features.iter().any(|f| f == verdict_file::FEATURE);
        // The verdict of a `clear_record` is no word on a release: no rule below reads it.
        let cleared = fleet
            .clear_records
            .take_verdicts(&who.host_id, legacy, &mut beat);
        let mut heard = if acking {
            let wanted = |name: &str| self.desired_in(basis, name).map(|want| want.version);
            let (heard, superseded) =
                verdict_file::fill(&self.verdicts, legacy, &who.host_id, &mut beat, &wanted);
            for name in superseded {
                if let Err(error) = self.verdicts.supersede(&name, &who.host_id) {
                    tracing::warn!(
                        service = %name,
                        node_id = %who.host_id,
                        %error,
                        "a superseded verdict is not forgotten; it is not read, and is forgotten at a later heartbeat"
                    );
                }
            }
            heard.into_iter().chain(cleared).collect()
        } else {
            Vec::new()
        };
        let announced = beat.services.clone().unwrap_or_default();
        self.tend_store(now);
        let held = halt::note(&self.ops, &basis.services, legacy, &who, &beat, &node, now);
        let (platform, ops) = (beat.platform, self.ops.state());
        self.note_serving(&basis.services, &who.host_id, &node, &beat, &ops, now);
        self.note_running(&fleet, &who, &beat, &announced, legacy, turn == Turn::Order);
        let counted =
            |def: &ServiceDef| standing(def, &who.host_id, &node, &ops, now) == Standing::Counted;
        for def in basis.services.defs() {
            if standing(def, &who.host_id, &node, &ops, now) == Standing::Dropped {
                fleet.forget_in(&def.name, &who.host_id);
            }
        }
        // A host a service does not count still says what it runs of it: kept as what it
        // occupies, so an update it was ordered before it left holds its slot until it ends.
        let dropped =
            |def: &ServiceDef| standing(def, &who.host_id, &node, &ops, now) == Standing::Dropped;
        // A host ordered to uninstall a service it is no longer assigned says how the uninstall
        // goes: kept as what it occupies, as a dropped host's word is.
        let uninstalling = |def: &ServiceDef| {
            ops.assignments
                .get(&def.name)
                .is_some_and(|entries| entries.uninstall.contains_key(who.host_id.as_str()))
        };
        // Only a verdict the fleet takes in is stored and acknowledged.
        heard.retain(|(name, _)| match basis.services.get(name) {
            Some(def) => counted(def) || dropped(def),
            None => Some(name.as_str()) == legacy,
        });
        if let Some(name) = legacy {
            match basis.services.get(name) {
                Some(def) if dropped(def) => {
                    fleet.record_outside_in(name, who.host_id.clone(), &role, beat, now);
                }
                top if top.is_none_or(counted) => {
                    fleet.record_named_in(name, who.host_id.clone(), &role, beat, now);
                }
                _ => {}
            }
        }
        for one in announced.iter().chain(&own) {
            match basis.services.get(&one.name) {
                Some(def) if counted(def) => fleet.record_named_in(
                    &one.name,
                    who.host_id.clone(),
                    &role,
                    one.as_heartbeat(platform),
                    now,
                ),
                // A host taken off the service while it ran an update of it says when that ends.
                Some(def)
                    if dropped(def)
                        || uninstalling(def)
                        || (fleet.get_in(&one.name, &who.host_id).is_none()
                            && fleet.occupant(&one.name, &who.host_id).is_some()) =>
                {
                    fleet.record_outside_in(
                        &one.name,
                        who.host_id.clone(),
                        &role,
                        one.as_heartbeat(platform),
                        now,
                    );
                }
                _ => {}
            }
        }
        fleet.note_node(who.host_id.clone(), node, self.ops.unix_now());
        if turn == Turn::Hold {
            return (ServiceReply::Nothing, None, None, Vec::new());
        }
        crate::intents::settle(&self.ops, &basis.services, &mut fleet, &who.host_id);
        let mut names: Vec<&str> = legacy
            .into_iter()
            .chain(announced.iter().map(|one| one.name.as_str()))
            .collect();
        // Stable: services of one priority keep the order above.
        names.sort_by_key(|name| Reverse(basis.services.get(name).map_or(0, |def| def.priority)));
        // The hosts' words are in: record the last good versions and roll back what failed,
        // before the orders below read the desired states.
        for def in names.iter().filter_map(|name| basis.services.get(name)) {
            // Only the verdicts about the desired version count, and not those an override
            // acknowledged; the others are forgotten.
            if let Some(want) = self.desired_in(basis, &def.name) {
                let acked = self.ops.marks(&def.name).failure_acked;
                fleet.keep_verdicts_in(def, &want.version, &acked);
            }
            reconcile(def, &self.ops, &self.desired, &mut fleet, now);
        }
        self.step_move(&mut fleet, basis, &who.host_id, now);
        // The verdicts are durable before the answer acknowledges them, and before any order.
        let acks = verdict_file::acknowledge(&self.verdicts, &fleet, &who.host_id, &heard, &names);
        // The operator's orders go first, past the gate, to a transport that carries them.
        if orders
            && !fleet.store_lost
            && let Some(order) = self.ops.read(|state| {
                crate::orders::next(&mut fleet, state, &who.host_id, &basis.services, &names)
            })
        {
            return (ServiceReply::Nothing, Some(order), None, acks);
        }
        let asked = Asked {
            who: &who,
            legacy,
            held: &held,
            basis,
            orders,
            now,
        };
        let (reply, order, terms) = self.gate_order(&mut fleet, &asked, names).unwrap_or((
            ServiceReply::Nothing,
            None,
            None,
        ));
        (reply, order, terms, acks)
    }

    /// The CI token in `bearer`, checked by the verifier: its issuer, audience, times and run
    /// rules as the allow-list file says them, and then that some package trusts the run
    /// ([`crate::trusted_publishers`]; refused as the rule that fails says, `repository`,
    /// `workflow`, `ref` or `environment`), before its `jti` is spent. Which package it may set is
    /// [`Self::ci_admits`].
    pub(crate) async fn ci(&self, bearer: Option<&str>) -> Result<Verified, CiRefused> {
        let token = bearer
            .filter(|t| t.len() <= MAX_BEARER_BYTES)
            .ok_or(CiRefused::Unauthorized("missing"))?;
        let trusted = |who: &Verified| self.ops.read(|state| self.judge_publishers(state, who));
        match self.verifier.verify_open(token, &trusted).await {
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
    /// `min_version`, when the service's `version_scheme` cannot
    /// read it, when it is marked bad (only the operator's unmark or a force lifts a mark; CI
    /// does not), and when CI set it before with another package digest. A body that names a digest (`release_digest`) records
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
        self.set_desired_verified(&verified, body).await
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
        self.operate_as(by.as_str(), body)
    }

    /// [`Self::operate`] for the operator `by`, whom the transport has already identified: by a
    /// bearer, or by the session of a signed-in user.
    ///
    /// # Errors
    /// [`OperateError`].
    pub(crate) fn operate_as(&self, by: &str, body: &[u8]) -> Result<AuditEntry, OperateError> {
        let groups = self.desired.host_groups();
        let entry = crate::operate::operate(&self.ops, &self.services, &groups, by, body)?;
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
        Ok(self.status_now())
    }

    /// The status as it is now, for an operator the transport has already identified.
    pub(crate) fn status_now(&self) -> Status {
        let now = (self.clock)();
        let fleet = self.lock_fleet();
        self.status_in(&fleet, now)
    }

    /// The status of `fleet` at `now`, which the caller holds locked: what [`Self::status`]
    /// answers, and what `Hosts.List` and `Hosts.Get` read ([`crate::hosts`]).
    pub(crate) fn status_in(&self, fleet: &Fleet, now: Duration) -> Status {
        let _plans = crate::stages::Memo::begin();
        let hosts = self
            .legacy
            .iter()
            .flat_map(|name| {
                fleet
                    .hosts_of(name)
                    .map(|(id, host)| host_status(fleet, name, id, host, now))
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
                let status = self.status_of(def, &self.services, &all, desired, fleet, &ops, now);
                (def.name.clone(), status)
            })
            .collect();
        let groups = self
            .desired
            .in_group_mode()
            .then(|| self.group_statuses(fleet, &ops, now));
        Status {
            per_service,
            groups,
            ops: ops.without_defaults(),
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
            server_certificate: self
                .enrollment
                .as_deref()
                .map(Enrollment::server_certificate),
        }
    }
}

/// Whether the registry of digests keeps `version` at its cap, whatever its age: the service's
/// last good version and the versions marked bad, which `marks` holds ([`crate::registry`]).
pub(crate) fn kept(marks: &crate::ops::ServiceOps, version: &Version) -> bool {
    marks.last_good.as_ref() == Some(version) || marks.bad.contains(version)
}

#[path = "service_ci.rs"]
pub(crate) mod ci_calls;
/// The calls of the heartbeat path that ask enrollment, those that act for a CI token already
/// checked, the rollout gate's orders for a heartbeat, and the care of the store's link.
#[path = "service_enrollment.rs"]
mod enrollment_calls;
#[path = "service_orders.rs"]
mod gate_orders;
#[path = "service_groups.rs"]
mod group_calls;
#[path = "service_store.rs"]
mod store_calls;
