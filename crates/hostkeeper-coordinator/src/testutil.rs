//! Builders for the unit tests.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_testkit::Scratch;
use hostkeeper_wire::identity::SanPrefixes;
use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::{Health, Heartbeat, Outcome, Reply, UpdateResult};
use hostkeeper_wire::{HostId, Platform, RoleName};

use crate::converge::Convergence;
use crate::desired::{Desired, RoleLimits};
use crate::fleet::Fleet;
use crate::marks_support::native_targets;
use crate::picks::{Heard, TARGET_FEATURE};
use crate::services::{ServiceDef, Services};

/// The legacy service of the unit tests: the one kbf's coordinator had built in, now plain data
/// in `tests/fixtures/recorded/kbf.services.toml`.
pub const LEGACY: &str = "kbf";

/// The role `name` ([`RoleName`] has no named constants: roles are open).
pub fn role(name: &str) -> RoleName {
    name.parse().expect("a role name")
}

/// The services of the recorded kbf definition alone.
pub fn kbf_services() -> Services {
    Services::parse(include_str!("../tests/fixtures/recorded/kbf.services.toml"))
        .expect("the recorded kbf definition")
}

/// The recorded kbf entry of a rollout file.
pub const KBF_ENTRY: &str = include_str!("../tests/fixtures/recorded/kbf.services.toml");

/// The services of the recorded kbf entry followed by the entries of `text`.
pub fn with_kbf(text: &str) -> Services {
    Services::parse(&format!("{KBF_ENTRY}\n{text}")).expect("services")
}

/// The recorded tierstore entry of a rollout file.
pub const TIERSTORE_ENTRY: &str =
    include_str!("../tests/fixtures/recorded/tierstore.services.toml");

/// The recorded kbf definition.
pub fn kbf_def() -> ServiceDef {
    kbf_services()
        .get(LEGACY)
        .cloned()
        .expect("the recorded file defines kbf")
}

/// What to tell the legacy-service host `id` at `now`, when the recorded kbf definition is the
/// only service. The hosts are daemons of [`native`].
pub fn decide(desired: &Desired, fleet: &Fleet, id: &HostId, now: Duration) -> Reply {
    let all = BTreeMap::from([(LEGACY.to_owned(), desired.clone())]);
    decide_in(&kbf_services(), &all, LEGACY, fleet, id, now)
}

/// Where the hosts of the recorded kbf definition stand at `now` against `desired`. The hosts are
/// daemons of [`native`].
pub fn converge(desired: Option<&Desired>, fleet: &Fleet, now: Duration) -> Convergence {
    converge_service(&kbf_def(), desired, fleet, now)
}

/// [`crate::decide::decide_in`] over the hosts of [`native`] for `desired`.
pub fn decide_in(
    services: &Services,
    desired: &BTreeMap<String, Desired>,
    service: &str,
    fleet: &Fleet,
    id: &HostId,
    now: Duration,
) -> Reply {
    crate::decide::decide_in(services, desired, service, &native(fleet, desired), id, now)
}

/// [`crate::decide::decide_why`] over the hosts of [`native`] for `desired`.
pub fn decide_why(
    services: &Services,
    desired: &BTreeMap<String, Desired>,
    service: &str,
    fleet: &Fleet,
    id: &HostId,
    now: Duration,
) -> crate::decide::Decision {
    crate::decide::decide_why(services, desired, service, &native(fleet, desired), id, now)
}

/// [`crate::decide::decide_marked`] over the hosts of [`native`] for `desired`.
pub fn decide_marked(
    services: &Services,
    desired: &BTreeMap<String, Desired>,
    bad: &[Version],
    service: &str,
    fleet: &Fleet,
    id: &HostId,
    now: Duration,
) -> crate::decide::Decision {
    let fleet = native(fleet, desired);
    crate::decide::decide_marked(services, desired, bad, service, &fleet, id, now)
}

/// [`crate::reconcile::plan`] over the hosts of [`native`] for `want`.
pub fn plan(
    def: &ServiceDef,
    want: &Desired,
    marks: &crate::ops::ServiceOps,
    fleet: &Fleet,
    state: &crate::ops::OpsState,
    now: Duration,
) -> Vec<crate::reconcile::Change> {
    let wants = BTreeMap::from([(def.name.clone(), want.clone())]);
    crate::reconcile::plan(def, want, marks, &native(fleet, &wants), state, now)
}

/// [`crate::converge::converge_service`] over the hosts of [`native`] for `desired`.
pub fn converge_service(
    def: &ServiceDef,
    desired: Option<&Desired>,
    fleet: &Fleet,
    now: Duration,
) -> Convergence {
    let wants: BTreeMap<String, Desired> = desired
        .map(|desired| (def.name.clone(), desired.clone()))
        .into_iter()
        .collect();
    crate::converge::converge_service(def, desired, &native(fleet, &wants), now)
}

/// [`crate::converge::role_counts`] over the hosts of [`native`] for `desired`.
pub fn role_counts(
    def: &ServiceDef,
    desired: Option<&Desired>,
    fleet: &Fleet,
    now: Duration,
) -> Vec<crate::converge::RoleCounts> {
    let wants: BTreeMap<String, Desired> = desired
        .map(|desired| (def.name.clone(), desired.clone()))
        .into_iter()
        .collect();
    crate::converge::role_counts(def, desired, &native(fleet, &wants), now)
}

/// A copy of `fleet` that is picked by native releases: each host it holds a word of, for the
/// services of `wants` and the legacy service, is a node whose daemon announces `target-v1`, lists
/// `apt` and runs each of those services with it, and each desired version of `wants` has a
/// release that lists one `apt` entry per platform. What the unit tests of the decision use to
/// have a version ordered at all: a version with no release kept orders nothing
/// ([`crate::picks`]).
pub fn native(fleet: &Fleet, wants: &BTreeMap<String, Desired>) -> Fleet {
    let mut fleet = fleet.clone();
    let mut services: Vec<&str> = wants.keys().map(String::as_str).collect();
    if !services.contains(&LEGACY) {
        services.push(LEGACY);
    }
    let mut heard: BTreeMap<HostId, (String, Platform, Duration, Vec<String>)> = BTreeMap::new();
    for service in &services {
        for (host, record) in fleet.hosts_of(service) {
            let entry = heard.entry(host.clone()).or_insert_with(|| {
                (
                    record.role_name().to_owned(),
                    record.beat.platform,
                    record.seen,
                    Vec::new(),
                )
            });
            entry.3.push((*service).to_owned());
        }
    }
    for (service, want) in wants {
        let release = crate::releases::Release::new(native_targets(service, 1), None);
        fleet
            .picks
            .keep_one(service, &want.version, Arc::new(release));
    }
    for (host, (role, platform, seen, names)) in heard {
        let mut node = fleet.node(&host).cloned().unwrap_or(crate::fleet::Node {
            role,
            features: Vec::new(),
            agent: None,
            platform,
            seen,
            announced: names
                .iter()
                .map(|name| (name.clone(), Health::Healthy))
                .collect(),
            sent_unix: None,
            cert_not_after_unix: None,
            last_renew_error: None,
            trusted_root: crate::trusted_root::TrustedRootSaid::default(),
            skewed_beats: 0,
        });
        node.features.retain(|f| f != TARGET_FEATURE);
        node.features.push(TARGET_FEATURE.to_owned());
        let request = agent::HeartbeatRequest {
            features: vec![TARGET_FEATURE.to_owned()],
            installers: vec!["apt".to_owned()],
            services: names
                .iter()
                .map(|name| agent::ServiceBeat {
                    name: name.clone(),
                    installer: Some("apt".to_owned()),
                    ..agent::ServiceBeat::default()
                })
                .collect(),
            ..agent::HeartbeatRequest::default()
        };
        let heard = Heard::of(&request).expect("the request lists apt");
        fleet.picks.hear(host.clone(), heard);
        fleet.note_node(host, node, 0);
    }
    fleet
}

/// A new scratch directory for a unit test, under the test binary's own directory.
pub fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary's path");
    let base: PathBuf = exe
        .parent()
        .expect("a directory")
        .join("hostkeeper-coordinator-unit");
    Scratch::under(&base, name).expect("a scratch directory")
}

pub fn version(text: &str) -> Version {
    Version::parse(text).expect("a version")
}

pub fn id(text: &str) -> HostId {
    text.parse().expect("a node id")
}

pub fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

/// A healthy heartbeat of a host running `running`, not updating, with no update behind it.
pub fn beat(running: &str) -> Heartbeat {
    Heartbeat {
        platform: Platform::LinuxX86_64,
        version: version(running),
        health: Health::Healthy,
        last_update: None,
        updating: None,
        services: None,
        agent_features: Vec::new(),
        step: None,
        commit: None,
        step_secs: Default::default(),
        promotion_pending: false,
        deferred: None,
        agent: None,
        sent_unix: None,
        cert_not_after_unix: None,
        last_renew_error: None,
        record_try: None,
    }
}

pub fn unhealthy(mut b: Heartbeat) -> Heartbeat {
    b.health = Health::Unhealthy;
    b
}

pub fn updating(mut b: Heartbeat, target: &str) -> Heartbeat {
    b.updating = Some(version(target));
    b
}

pub fn last(mut b: Heartbeat, target: &str, outcome: Outcome) -> Heartbeat {
    b.last_update = Some(UpdateResult::new(version(target), outcome));
    b
}

/// A desired state: version `target`, the default limits, `storage` and `frontend` hosts
/// that must stay up.
pub fn desired(target: &str, storage: u32, frontend: u32) -> Desired {
    Desired {
        version: version(target),
        max_updating: RoleLimits::max_updating_default(),
        min_up: RoleLimits::from([("storage", storage), ("frontend", frontend)]),
        release_digest: None,
        release_index: None,
    }
}

/// A fleet started at time zero whose hosts all heartbeated at `seen`.
pub fn fleet(seen: Duration, hosts: &[(&str, RoleName, Heartbeat)]) -> Fleet {
    let mut fleet = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    for (name, role, b) in hosts {
        fleet.record_in(LEGACY, id(name), role.clone(), b.clone(), seen);
    }
    fleet
}

// ---- a coordinator on a settable clock -------------------------------------------------

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures::future::BoxFuture;
use hostkeeper_oidc::testkit::{
    STATIC_ISSUER, config_text, good_claims, keys, mint, static_verifier,
};
use hostkeeper_oidc::testkit::{StaticKeys, clock as oidc_clock, jwks_of};
use hostkeeper_oidc::{AllowList, FetchCause, FetchError, KeySource, OidcConfig, Verifier};

use crate::desired::DesiredStore;
use crate::ops::OpsStore;
use crate::service::{Clock, Coordinator};
use crate::verdict_file::{VerdictFile, verdicts_path};

/// The operator token of the tests.
pub const OPERATOR: &str = "operator-token-0123456789abcdef0123456789";

/// The ops file of a rig's directory: the default of `--ops-file` beside `desired.json`.
pub fn ops_of(dir: &Scratch) -> OpsStore {
    OpsStore::open(&dir.path().join("desired.json.ops")).expect("an ops store")
}

/// The file of verdicts of a rig's directory, beside `desired.json`.
pub fn verdicts_of(dir: &Scratch) -> VerdictFile {
    VerdictFile::open(&verdicts_path(&dir.path().join("desired.json"))).expect("a verdict file")
}

/// A coordinator whose clock the test sets, over a state file in a directory of its own.
pub struct Rig {
    pub coordinator: Coordinator,
    /// Seconds since the coordinator started.
    pub clock: Arc<AtomicU64>,
    pub dir: Scratch,
    _log: tracing::subscriber::DefaultGuard,
}

/// The certificate namespaces kbf's farm used, which the recorded certificates carry.
pub fn kbf_prefixes() -> SanPrefixes {
    SanPrefixes::new("urn:kbf:role:", "urn:kbf:host-role:").expect("prefixes")
}

/// An issuer that cannot be reached.
pub struct Unreachable;

impl KeySource for Unreachable {
    fn jwks(&self) -> BoxFuture<'_, Result<String, FetchError>> {
        Box::pin(async {
            Err(FetchError::Failed {
                url: STATIC_ISSUER.to_owned(),
                cause: FetchCause::Connect(std::io::Error::from(
                    std::io::ErrorKind::HostUnreachable,
                )),
            })
        })
    }
}

/// A verifier whose issuer cannot be reached.
pub fn unreachable_verifier() -> Arc<Verifier> {
    let config = OidcConfig::parse(&config_text(STATIC_ISSUER)).expect("a config");
    let (_, issuer_clock) = static_verifier();
    let issuer_now = Arc::new(move || issuer_clock.load(Ordering::SeqCst));
    Arc::new(Verifier::new(
        config,
        Arc::new(Unreachable),
        Duration::from_secs(30),
        issuer_now,
        hostkeeper_oidc::testkit::spent(),
    ))
}

pub fn clock_of(seconds: &Arc<AtomicU64>) -> Clock {
    let seconds = Arc::clone(seconds);
    Arc::new(move || Duration::from_secs(seconds.load(Ordering::SeqCst)))
}

impl Rig {
    pub fn new(name: &str) -> Self {
        let dir = scratch(name);
        let clock = Arc::new(AtomicU64::new(1_000));
        let coordinator = Self::start(&dir, &clock);
        Self::of(coordinator, clock, dir)
    }

    /// A rig whose CI issuer cannot be reached.
    pub fn with_unreachable_issuer(name: &str) -> Self {
        let dir = scratch(name);
        let clock = Arc::new(AtomicU64::new(1_000));
        let store =
            DesiredStore::open(&dir.path().join("desired.json"), Some(LEGACY)).expect("a store");
        let coordinator = Coordinator::new(
            store,
            unreachable_verifier(),
            OPERATOR.to_owned(),
            clock_of(&clock),
        )
        .with_services(kbf_services())
        .with_legacy_service(Some(LEGACY.to_owned()))
        .with_prefixes(kbf_prefixes())
        .with_ops(ops_of(&dir));
        Self::of(coordinator, clock, dir)
    }

    fn of(coordinator: Coordinator, clock: Arc<AtomicU64>, dir: Scratch) -> Self {
        // A tracing macro evaluates its fields only under a subscriber: the logs go nowhere.
        let log = tracing_subscriber::fmt()
            .with_writer(std::io::sink)
            .finish();
        Self {
            coordinator,
            clock,
            dir,
            _log: tracing::subscriber::set_default(log),
        }
    }

    /// A coordinator on the same state file that starts now: what a restart makes.
    pub fn start(dir: &Scratch, clock: &Arc<AtomicU64>) -> Coordinator {
        let store =
            DesiredStore::open(&dir.path().join("desired.json"), Some(LEGACY)).expect("a store");
        let (verifier, _) = static_verifier();
        Coordinator::new(store, verifier, OPERATOR.to_owned(), clock_of(clock))
            .with_services(kbf_services())
            .with_legacy_service(Some(LEGACY.to_owned()))
            .with_prefixes(kbf_prefixes())
            .with_ops(ops_of(dir))
            .with_verdicts(verdicts_of(dir))
    }

    /// A coordinator on the same state file whose CI tokens are checked against the
    /// allow-list `text` (the testkit's issuer and its first key).
    pub fn start_with_allow_list(dir: &Scratch, clock: &Arc<AtomicU64>, text: &str) -> Coordinator {
        let store =
            DesiredStore::open(&dir.path().join("desired.json"), Some(LEGACY)).expect("a store");
        Self::start_over(store, clock, text)
            .with_ops(ops_of(dir))
            .with_verdicts(verdicts_of(dir))
    }

    /// A coordinator over the desired state of `store`, whose CI tokens are checked against the
    /// allow-list `text`; its ops store and verdicts are the caller's to give.
    pub fn start_over(store: DesiredStore, clock: &Arc<AtomicU64>, text: &str) -> Coordinator {
        let (oidc_now, _) = oidc_clock();
        let verifier = Verifier::new(
            AllowList::parse(text).expect("an allow-list"),
            Arc::new(StaticKeys(jwks_of(&[&keys()[0]]))),
            Duration::from_secs(30),
            oidc_now,
            hostkeeper_oidc::testkit::spent(),
        );
        Coordinator::new(
            store,
            Arc::new(verifier),
            OPERATOR.to_owned(),
            clock_of(clock),
        )
        .with_services(kbf_services())
        .with_legacy_service(Some(LEGACY.to_owned()))
        .with_prefixes(kbf_prefixes())
    }

    pub fn restart(&mut self) {
        self.coordinator = Self::start(&self.dir, &self.clock);
    }

    pub fn at(&self, seconds: u64) {
        self.clock.store(seconds, Ordering::SeqCst);
    }

    /// Sets the desired state as CI would, with a token no one has used, and with the release
    /// [`native_targets`] for the service the body names (the legacy service when it names none):
    /// its `release_digest`, if the body has one, only says which number
    /// [`crate::marks_support::digest`] pins the release's entry, and is not set.
    pub async fn set(&self, body: &str) {
        set_native(&self.coordinator, body).await;
    }

    /// Sets the desired state as CI would from `body` alone, with no release: a version no host
    /// is picked an entry of.
    pub async fn set_plain(&self, body: &str) {
        self.coordinator
            .set_desired(Some(&ci_token()), body.as_bytes())
            .await
            .expect("set");
    }
}

/// CI's set of `body` to `coordinator` with a token no one has used, with the release
/// [`native_targets`] for the service the body names (the legacy service when it names none): its
/// `release_digest`, if the body has one, only says which number
/// [`crate::marks_support::digest`] pins the release's entry, and is not set.
pub async fn set_native(coordinator: &Coordinator, body: &str) {
    let mut body: serde_json::Value = serde_json::from_str(body).expect("a JSON body");
    let service = body
        .get("service")
        .and_then(|service| service.as_str())
        .unwrap_or(LEGACY)
        .to_owned();
    let n = body
        .as_object_mut()
        .and_then(|fields| fields.remove("release_digest"))
        .and_then(|digest| u8::from_str_radix(digest.as_str()?.get(..2)?, 16).ok())
        .unwrap_or(1);
    let verified = coordinator.ci(Some(&ci_token())).await.expect("a token");
    coordinator.keep_komira();
    coordinator
        .set_desired_with(
            &crate::service::ci_calls::Setter::Ci(&verified),
            body.to_string().as_bytes(),
            &crate::write_record::WriteAsk::default(),
            Some(native_targets(&service, n)),
        )
        .await
        .expect("set");
}

impl Coordinator {
    /// Puts the repository `komira` (the one the rollout file's services name by default) in the
    /// registry when it is not there, without an audit entry: a set with the `apt` entries of
    /// [`crate::marks_support::native_targets`] is refused until the registry holds it.
    pub(crate) fn keep_komira(&self) {
        let komira = crate::repositories_support::komira(1);
        let missing = self
            .desired
            .with_repositories(|registry| registry.get(&komira.name).is_none());
        if missing {
            let staged = self
                .desired
                .stage_repositories(|held| held.put(komira))
                .expect("the registry takes komira");
            self.ops
                .decide_changes_with(staged, |_| Vec::new())
                .expect("the registry is stored");
        }
    }

    /// Notes that the daemon of `who` announces `target-v1`, lists `apt` and runs each service the
    /// coordinator manages, and each package a host group lists, with it, as its heartbeat over
    /// gRPC says it.
    pub(crate) fn hear_apt(&self, who: &crate::identity::Identity) {
        let listed: std::collections::BTreeSet<String> = self
            .services
            .names()
            .map(str::to_owned)
            .chain(
                self.desired
                    .host_groups()
                    .0
                    .values()
                    .flat_map(|group| group.packages.clone()),
            )
            .collect();
        let request = agent::HeartbeatRequest {
            features: vec![TARGET_FEATURE.to_owned()],
            installers: vec!["apt".to_owned()],
            services: listed
                .into_iter()
                .map(|name| agent::ServiceBeat {
                    name,
                    installer: Some("apt".to_owned()),
                    ..agent::ServiceBeat::default()
                })
                .collect(),
            ..agent::HeartbeatRequest::default()
        };
        self.hear_installers(who, &request);
    }

    /// [`Coordinator::heartbeat_with_orders`] for a daemon that lists `apt` ([`Self::hear_apt`]).
    pub(crate) fn heartbeat_with_orders_native(
        &self,
        who: crate::identity::Identity,
        beat: Heartbeat,
        root: crate::trusted_root::TrustedRootSaid,
    ) -> Result<
        (hostkeeper_wire::wire::Answer, Option<crate::orders::Order>),
        crate::service::HeartbeatRefused,
    > {
        self.hear_apt(&who);
        self.heartbeat_with_orders(who, beat, root)
    }

    /// [`Coordinator::heartbeat_from`] as a daemon that announces `target-v1` and lists `apt` makes
    /// it over a transport that carries typed orders, from the machine `machine_id`.
    pub(crate) fn heartbeat_from_native(
        &self,
        who: crate::identity::Identity,
        mut beat: Heartbeat,
        machine_id: Option<&str>,
    ) -> Result<hostkeeper_wire::wire::Answer, crate::service::HeartbeatRefused> {
        beat.agent_features.retain(|f| f != TARGET_FEATURE);
        beat.agent_features.push(TARGET_FEATURE.to_owned());
        self.hear_apt(&who);
        self.answer(
            who,
            beat,
            true,
            machine_id,
            crate::trusted_root::TrustedRootSaid::default(),
        )
        .map(|(answer, _)| answer)
    }

    /// [`Coordinator::heartbeat`] as a daemon that announces `target-v1` and lists `apt` makes it
    /// over a transport that carries typed orders: the JSON route carries no order. The answer
    /// has the update the gate orders, as the reply; the typed order is dropped.
    pub(crate) fn heartbeat_native(
        &self,
        who: crate::identity::Identity,
        mut beat: Heartbeat,
    ) -> Result<hostkeeper_wire::wire::Answer, crate::service::HeartbeatRefused> {
        if !beat.agent_features.iter().any(|f| f == TARGET_FEATURE) {
            beat.agent_features.push(TARGET_FEATURE.to_owned());
        }
        self.heartbeat_with_orders_native(
            who,
            beat,
            crate::trusted_root::TrustedRootSaid::default(),
        )
        .map(|(answer, _)| answer)
    }
}

/// A token CI would send, valid for the allow-list of the testkit, signed by its first key.
pub fn ci_token() -> String {
    mint(&keys()[0], &good_claims(STATIC_ISSUER))
}

/// What a subscriber wrote.
#[derive(Clone, Default)]
pub struct Log(Arc<std::sync::Mutex<Vec<u8>>>);

impl Log {
    pub fn text(&self) -> String {
        String::from_utf8(self.0.lock().expect("lock").clone()).expect("UTF-8")
    }

    /// Installs a subscriber that writes here, for as long as the guard lives.
    pub fn install(&self) -> tracing::subscriber::DefaultGuard {
        tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .with_writer(self.clone())
                .with_ansi(false)
                .finish(),
        )
    }
}

impl std::io::Write for Log {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("lock").extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Log {
    type Writer = Self;

    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}
