//! The real coordinator in process, and a host whose heartbeat loop talks to it: the
//! coordinator's heartbeat with typed orders (`Coordinator::heartbeat_with_orders`, what its
//! gRPC route answers) behind a link that calls it as the host `host-1` of the role `daemon`,
//! and the loop's beats (with what each says of the daemon's trusted root) and orders mapped to
//! and from the coordinator's types word for word.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_coordinator::desired::DesiredStore;
use hostkeeper_coordinator::operator::Caller;
use hostkeeper_coordinator::ops::OpsStore;
use hostkeeper_coordinator::orders::Order as Ordered;
use hostkeeper_coordinator::service::{Clock, Coordinator};
use hostkeeper_coordinator::services::Services;
use hostkeeper_coordinator::trusted_root::TrustedRootSaid;
use hostkeeper_coordinator::verdict_file::{VerdictFile, verdicts_path};
use hostkeeper_daemon::Daemon;
use hostkeeper_daemon::links::Links;
use hostkeeper_daemon::start;
use hostkeeper_hook::{Class, Phase, Step};
use hostkeeper_link::beat::AGENT_NAME;
use hostkeeper_link::testing::{
    FakeCerts, FakeClock, ManualDispatch, RecordingLog, ScriptedSleeper, identity,
};
use hostkeeper_link::{
    Acknowledged, BeatOutcome, CertState, ClientIdentity, CoordinatorLink, DEFAULT_HEARTBEAT_SECS,
    Feature, Health, Heartbeat, Heartbeater, LinkError, Order, OrderId, OrderRef, OrderResult,
    Outcome, PackageDigest, Reason, Reply, RootReason, ServiceFacts, StopFlag, Target, Verb,
};
use hostkeeper_oidc::testkit::{
    STATIC_ISSUER, StaticKeys, clock as oidc_clock, config_text, good_claims, jwks_of, keys, mint,
    spent,
};
use hostkeeper_oidc::{OidcConfig, Verifier};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_proto::hostkeeper::operator::v1::{GetHostRequest, HostService};
use hostkeeper_steps::Settled;
use hostkeeper_wire::identity::Identity;
use hostkeeper_wire::wire::{self, ServiceBeat, UpdateResult};

use super::selfupdate::{HK, SelfRig};
use super::{OWN, Rig, d};

/// The operator token of the coordinator.
pub const OPERATOR: &str = "operator-token-0123456789abcdef0123456789";

/// The closing JSON line of each step of a conforming hook: `prepare` changes nothing it cannot
/// undo, `cutover` commits, `verify` asks for no promotion, `cleanup` says nothing more.
pub const HOOK_LINES: [(Step, &str); 4] = [
    (Step::Prepare, r#"{"reversible":true}"#),
    (Step::Cutover, r#"{"reversible":false}"#),
    (Step::Verify, r#"{"promotion_pending":false}"#),
    (Step::Cleanup, "{}"),
];

/// The real coordinator on the state files of `dir`: `service` runs on the role `daemon` and is
/// wanted at `version`, whose package digest CI registered as [`d`], unless a coordinator before
/// it on `dir` left another state; the verdicts it acknowledges are kept in the verdicts file
/// beside the state file, as `serve` keeps them; it orders from its start (no quiet period), on a
/// clock that stays at second 1000.
pub fn coordinator_of(dir: &Path, service: &str, version: &str) -> Arc<Coordinator> {
    coordinator_with(dir, service, version, "")
}

/// [`coordinator_of`], the rollout of `service` also given the keys of `rollout` (TOML lines of
/// its `[[service]]` table, such as `bake_beats = 2`).
pub fn coordinator_with(
    dir: &Path,
    service: &str,
    version: &str,
    rollout: &str,
) -> Arc<Coordinator> {
    let state = dir.join("desired.json");
    // A coordinator started again on the same directory reads the state the first one left.
    if !state.exists() {
        std::fs::write(
            &state,
            format!(
                r#"{{"services":{{"{service}":{{"version":"{version}","min_up":{{}},"release_digest":"{0}","digests":{{"{version}":"{0}"}}}}}}}}"#,
                d()
            ),
        )
        .expect("the desired state");
    }
    let verdicts = VerdictFile::open(&verdicts_path(&state)).expect("the verdicts file");
    let desired = DesiredStore::open(&dir.join("desired.json"), None).expect("the desired state");
    let ops = OpsStore::open(&dir.join("desired.json.ops")).expect("the ops store");
    // CI's allow-list admits the service, so a scenario can set its next version.
    let allow = format!("{}services = [\"{service}\"]\n", config_text(STATIC_ISSUER));
    let (oidc_now, _) = oidc_clock();
    let verifier = Verifier::new(
        OidcConfig::parse(&allow).expect("the allow-list"),
        Arc::new(StaticKeys(jwks_of(&[&keys()[0]]))),
        Duration::from_secs(30),
        oidc_now,
        spent(),
    );
    let clock: Clock = Arc::new(|| Duration::from_secs(1_000));
    let rollout = format!("[[service]]\nname = \"{service}\"\nroles = [\"daemon\"]\n{rollout}\n");
    Arc::new(
        Coordinator::new(desired, Arc::new(verifier), OPERATOR.to_owned(), clock)
            .with_services(Services::parse(&rollout).expect("the rollout file"))
            .with_quiet(Duration::ZERO)
            .with_ops(ops)
            .with_verdicts(verdicts),
    )
}

/// CI sets `service` to `version`, with the package digest [`d`].
pub fn set_desired_of(coordinator: &Coordinator, service: &str, version: &str) {
    let token = mint(&keys()[0], &good_claims(STATIC_ISSUER));
    let body = format!(
        r#"{{"service":"{service}","version":"{version}","release_digest":"{}"}}"#,
        d()
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a runtime");
    runtime
        .block_on(coordinator.set_desired(Some(&token), body.as_bytes()))
        .expect("CI sets the version");
}

/// The convergence the coordinator's status answers for `service`, as JSON.
pub fn convergence_of(coordinator: &Coordinator, service: &str) -> serde_json::Value {
    let status = coordinator.status(Some(OPERATOR)).expect("the status");
    serde_json::to_value(&status.per_service[service].convergence).expect("JSON")
}

pub fn wire_version(text: &str) -> hostkeeper_wire::version::Version {
    hostkeeper_wire::version::Version::parse(text).expect("a version the wire carries")
}

pub fn wire_health(health: Health) -> wire::Health {
    match health {
        Health::Healthy => wire::Health::Healthy,
        Health::Unhealthy => wire::Health::Unhealthy,
        Health::Absent => wire::Health::Absent,
    }
}

pub fn wire_phase(phase: Phase) -> wire::Phase {
    match phase {
        Phase::BeforeCommit => wire::Phase::BeforeCommit,
        Phase::GateRelease => wire::Phase::GateRelease,
        Phase::GateEnvironment => wire::Phase::GateEnvironment,
        Phase::AfterCommit => wire::Phase::AfterCommit,
        Phase::Halt => wire::Phase::Halt,
    }
}

pub fn wire_class(class: Class) -> wire::Class {
    match class {
        Class::Release => wire::Class::Release,
        Class::Environment => wire::Class::Environment,
    }
}

/// A verdict as the coordinator reads it: its order, outcome, phase, class and reason.
pub fn update_result(result: &OrderResult) -> UpdateResult {
    let outcome = match result.outcome {
        Outcome::Succeeded => wire::Outcome::Succeeded,
        Outcome::Failed | Outcome::Deferred { .. } => wire::Outcome::Failed,
    };
    UpdateResult {
        order_id: wire::OrderId::parse(result.order.id.as_str()),
        order_try: Some(result.order.order_try),
        phase: result.phase.map(wire_phase),
        class: result.class.map(wire_class),
        reason: result
            .reason
            .as_ref()
            .and_then(|reason| wire::Token::parse(reason.as_str())),
        ..UpdateResult::new(wire_version(result.target.as_str()), outcome)
    }
}

pub fn service_beat(facts: &ServiceFacts) -> ServiceBeat {
    ServiceBeat {
        name: facts.name.as_str().to_owned(),
        version: wire_version(facts.version.as_str()),
        health: wire_health(facts.health),
        last_update: facts.last_update.as_ref().map(update_result),
        updating: facts
            .updating
            .as_ref()
            .map(|version| wire_version(version.as_str())),
        step: None,
        commit: None,
        step_secs: BTreeMap::new(),
        promotion_pending: false,
        deferred: None,
        record_try: facts.record_try,
    }
}

/// The loop's heartbeat as the coordinator reads it: every service, and the features announced.
pub fn wire_beat(beat: &Heartbeat) -> wire::Heartbeat {
    wire::Heartbeat {
        platform: hostkeeper_wire::Platform::parse(beat.platform.as_str()).expect("a platform"),
        version: wire_version(beat.agent_version.as_str()),
        health: wire::Health::Healthy,
        last_update: None,
        updating: None,
        services: Some(beat.services.iter().map(service_beat).collect()),
        agent_features: beat
            .features
            .iter()
            .map(|feature| feature.as_str().to_owned())
            .collect(),
        step: None,
        commit: None,
        step_secs: BTreeMap::new(),
        promotion_pending: false,
        deferred: None,
        // The agent, as the coordinator's gRPC route reads it from the loop's `agent`.
        agent: Some(wire::Agent {
            name: wire::AgentName::parse(AGENT_NAME).expect("an agent name"),
            version: wire_version(beat.agent_version.as_str()),
        }),
        sent_unix: None,
        cert_not_after_unix: None,
        last_renew_error: None,
        record_try: None,
    }
}

/// What the loop's heartbeat says of the daemon's trusted root, as the coordinator's gRPC route
/// reads it from the request: no report is no age and an unknown reason.
pub fn root_said(beat: &Heartbeat) -> TrustedRootSaid {
    let Some(root) = beat.trusted_root else {
        return TrustedRootSaid::default();
    };
    TrustedRootSaid {
        age_secs: root.age_secs,
        reason: match root.reason {
            None => pb::TrustedRootReason::None,
            Some(RootReason::Stale) => pb::TrustedRootReason::Stale,
            Some(RootReason::Missing) => pb::TrustedRootReason::Missing,
            Some(RootReason::Unparseable) => pb::TrustedRootReason::Unparseable,
        },
    }
}

/// The coordinator's typed order as the loop reads it: the protobuf order it sends, word for
/// word in the loop's types.
pub fn link_order(ordered: &Ordered) -> Order {
    let order = ordered.to_proto();
    let target = |service: &str, version: &str, digest: &str, index: &str| Target {
        service: ServiceName::new(service).expect("a service"),
        version: Version::new(version).expect("a version"),
        digest: PackageDigest::new(digest).expect("a digest"),
        index_digest: (!index.is_empty())
            .then(|| PackageDigest::new(index).expect("an index digest")),
        entry: None,
    };
    let named = |service: &str| ServiceName::new(service).expect("a service");
    let verb = match order.verb.expect("a verb") {
        pb::order::Verb::Update(a) => Verb::Update(target(
            &a.service,
            &a.version,
            &a.package_digest,
            &a.index_digest,
        )),
        pb::order::Verb::Install(a) => Verb::Install(target(
            &a.service,
            &a.version,
            &a.package_digest,
            &a.index_digest,
        )),
        pb::order::Verb::Uninstall(a) => Verb::Uninstall(named(&a.service)),
        pb::order::Verb::Abort(a) => Verb::Abort(named(&a.service)),
        pb::order::Verb::ClearRecord(a) => Verb::ClearRecord(named(&a.service)),
        other => panic!("a verb these scenarios do not order: {other:?}"),
    };
    Order {
        id: OrderId::new(&order.order_id).expect("an order id"),
        order_try: order.r#try,
        verb,
    }
}

/// The network transport, in process: each heartbeat goes to the coordinator's heartbeat with
/// typed orders as the host `host-1` of the role `daemon`. It keeps the order of each answer.
pub struct InProcess {
    coordinator: Arc<Coordinator>,
    ordered: Arc<Mutex<Vec<Option<Order>>>>,
}

impl CoordinatorLink for InProcess {
    fn heartbeat(&self, _identity: &ClientIdentity, beat: &Heartbeat) -> Result<Reply, LinkError> {
        let who = Identity {
            host_id: "host-1".parse().expect("a host id"),
            role: "daemon".parse().expect("a role"),
        };
        let (answer, ordered) = self
            .coordinator
            .heartbeat_with_orders(who, wire_beat(beat), root_said(beat))
            .map_err(|_| LinkError::Refused(Reason::new("forbidden").expect("a token")))?;
        let order = ordered.as_ref().map(link_order);
        self.ordered
            .lock()
            .expect("the log is not poisoned")
            .push(order.clone());
        Ok(Reply {
            order,
            features: answer
                .features
                .iter()
                .flatten()
                .filter_map(|name| Feature::parse(name))
                .collect(),
            halt_generations: answer
                .halts
                .iter()
                .map(|(service, generation)| {
                    (ServiceName::new(service).expect("a service"), *generation)
                })
                .collect(),
            acknowledged: answer
                .acks
                .iter()
                .map(|ack| Acknowledged {
                    service: ServiceName::new(&ack.service).expect("a service"),
                    order: OrderRef {
                        id: OrderId::new(ack.order_id.as_str()).expect("an id"),
                        order_try: ack.order_try,
                    },
                })
                .collect(),
            repositories: None,
        })
    }
}

/// A host whose daemon started on `rig`, its loop over the in-process link to `coordinator`
/// announcing `features`, the jobs of its orders kept until the test runs them.
pub struct Host {
    pub heartbeater: Heartbeater,
    pub dispatch: Arc<ManualDispatch>,
    pub log: Arc<RecordingLog>,
    pub ordered: Arc<Mutex<Vec<Option<Order>>>>,
    _started: Option<start::Started>,
}

impl Host {
    pub fn new(rig: &Rig, coordinator: &Arc<Coordinator>) -> Host {
        let started = start::startup(&rig.settings(), rig.ports(), OWN).expect("the daemon starts");
        assert_eq!(
            started.boot_report().ready,
            Some(Ok(())),
            "the boot pass ran"
        );
        let daemon = Arc::clone(started.daemon());
        Host::of(daemon, coordinator, Some(started))
    }

    /// The host of `daemon`, its loop over the in-process link to `coordinator`; `started` is
    /// kept while the host lives.
    pub fn of(
        daemon: Arc<Daemon>,
        coordinator: &Arc<Coordinator>,
        started: Option<start::Started>,
    ) -> Host {
        let ordered = Arc::new(Mutex::new(Vec::new()));
        let dispatch = Arc::new(ManualDispatch::default());
        let log = Arc::new(RecordingLog::default());
        let links = Links {
            enroller: Box::new(|| Err("the scenario has no enrollment".to_owned())),
            link: Arc::new(InProcess {
                coordinator: Arc::clone(coordinator),
                ordered: Arc::clone(&ordered),
            }),
            certs: Arc::new(FakeCerts::new(CertState::Enrolled(identity("host-1")))),
            clock: Arc::new(FakeClock::new(2_000_000_000, 7)),
            log: log.clone(),
            sleeper: Box::new(ScriptedSleeper::new(0)),
            period: DEFAULT_HEARTBEAT_SECS,
            dispatch: dispatch.clone(),
            stop: StopFlag::default(),
            enroll_sleeper: Box::new(ScriptedSleeper::new(0)),
            renewal: hostkeeper_enroll::RenewalState::default(),
            watchdog: Box::new(hostkeeper_daemon::NoWatchdog),
            console: None,
        };
        Host {
            heartbeater: links.heartbeater(&daemon),
            dispatch,
            log,
            ordered,
            _started: started,
        }
    }

    /// One heartbeat, answered.
    pub fn beat(&mut self) {
        assert_eq!(self.heartbeater.beat_once(), BeatOutcome::Sent);
    }

    /// The order of each answer, in order.
    pub fn ordered(&self) -> Vec<Option<Order>> {
        self.ordered
            .lock()
            .expect("the log is not poisoned")
            .clone()
    }
}

/// The order of the answer `n` (from 0) and its try, as the loop read it.
pub fn order_and_try(host: &Host, n: usize) -> (Verb, u64) {
    let order = host.ordered()[n].clone().expect("an order");
    (order.verb, order.order_try)
}

/// A daemon of `version` on the self-update rig `s`, its update of
/// hostkeeper settled as a start of that binary settles it.
pub fn hostkeeper_daemon(s: &SelfRig, version: &str) -> (Arc<Daemon>, Settled) {
    let daemon = s.daemon_at(version);
    let settled = daemon.settle_self_update().expect("the start settles");
    (Arc::new(daemon), settled)
}

/// hostkeeper at `version`, as an update or an install of the package [`d`] names it.
pub fn hostkeeper_at(version: &str) -> Target {
    Target {
        service: ServiceName::new(HK).expect("a service"),
        version: Version::new(version).expect("a version"),
        digest: PackageDigest::new(&d()).expect("a digest"),
        index_digest: None,
        entry: None,
    }
}

/// What `Hosts.Get` says of `service` on `host-1`.
pub fn on_host(coordinator: &Coordinator, service: &str) -> Option<HostService> {
    let operator = Caller::Operator("operator".parse().expect("a token name"));
    let request = GetHostRequest {
        node_id: "host-1".to_owned(),
    };
    let host = coordinator.get_host(&operator, &request);
    let services = host.expect("Hosts.Get").host.expect("a host").services;
    services.into_iter().find(|one| one.service == service)
}

pub fn hk_on_host(coordinator: &Coordinator) -> HostService {
    on_host(coordinator, HK).expect("hostkeeper on the host")
}
