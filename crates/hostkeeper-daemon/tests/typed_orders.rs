//! The coordinator's typed orders end to end, in process: the real coordinator's heartbeat with
//! typed orders (`Coordinator::heartbeat_with_orders`, what its gRPC route answers), the real
//! heartbeat loop over the daemon (`hostkeeper_daemon::links`), and the real executor with its
//! step runner. The fakes are the network transport (a link that calls the coordinator as the
//! host `host-1` of the role `daemon`), the service manager and the package source.
//!
//! The coordinator orders an `install` and an `abort` only to a daemon that announced
//! `install-v1` and `abort-v1`; the loop announces what [`ANNOUNCED`] lists, so these tests also
//! prove the daemon's announcement reaches the coordinator's gates.

mod support;

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
use hostkeeper_coordinator::verdict_file::{VerdictFile, verdicts_path};
use hostkeeper_daemon::links::Links;
use hostkeeper_daemon::start;
use hostkeeper_daemon::testing::spawn;
use hostkeeper_hook::{Class, Phase, Step};
use hostkeeper_link::testing::{
    FakeCerts, FakeClock, ManualDispatch, RecordingLog, ScriptedSleeper, identity,
};
use hostkeeper_link::{
    ANNOUNCED, Acknowledged, BeatOutcome, CertState, ClientIdentity, CoordinatorLink,
    DEFAULT_HEARTBEAT_SECS, Feature, Health, Heartbeat, Heartbeater, LinkError, Order, OrderId,
    OrderRef, OrderResult, Outcome, PackageDigest, Reason, Reply, ServiceFacts, Sleeper, StopFlag,
    Target, Tick, Verb,
};
use hostkeeper_oidc::testkit::{
    STATIC_ISSUER, StaticKeys, clock as oidc_clock, config_text, good_claims, jwks_of, keys, mint,
    spent,
};
use hostkeeper_oidc::{OidcConfig, Verifier};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_proto::hostkeeper::operator::v1::{AbortRequest, GetHostRequest};
use hostkeeper_wire::identity::Identity;
use hostkeeper_wire::wire::{self, ServiceBeat, UpdateResult};
use support::{OWN, Rig, Scratch, d};

/// The service of the scenarios and the version the coordinator wants.
const SERVICE: &str = "web";
const TARGET: &str = "2.0.0";

/// The operator token of the coordinator.
const OPERATOR: &str = "operator-token-0123456789abcdef0123456789";

/// The closing JSON line of each step of a conforming hook: `prepare` changes nothing it cannot
/// undo, `cutover` commits, `verify` asks for no promotion, `cleanup` says nothing more.
const HOOK_LINES: [(Step, &str); 4] = [
    (Step::Prepare, r#"{"reversible":true}"#),
    (Step::Cutover, r#"{"reversible":false}"#),
    (Step::Verify, r#"{"promotion_pending":false}"#),
    (Step::Cleanup, "{}"),
];

/// The real coordinator on the state files of `dir`: `web` runs on the role `daemon` and is wanted
/// at [`TARGET`], whose package digest CI registered as [`d`], unless a coordinator before it on
/// `dir` left another state; the verdicts it acknowledges are kept in the verdicts file beside
/// the state file, as `serve` keeps them; it orders from its start (no quiet period), on a clock
/// that stays at second 1000.
fn coordinator(dir: &Path) -> Arc<Coordinator> {
    let state = dir.join("desired.json");
    // A coordinator started again on the same directory reads the state the first one left.
    if !state.exists() {
        std::fs::write(
            &state,
            format!(
                r#"{{"services":{{"{SERVICE}":{{"version":"{TARGET}","min_up":{{}},"release_digest":"{0}","digests":{{"{TARGET}":"{0}"}}}}}}}}"#,
                d()
            ),
        )
        .expect("the desired state");
    }
    let verdicts = VerdictFile::open(&verdicts_path(&state)).expect("the verdicts file");
    let desired = DesiredStore::open(&dir.join("desired.json"), None).expect("the desired state");
    let ops = OpsStore::open(&dir.join("desired.json.ops")).expect("the ops store");
    // CI's allow-list admits `web`, so a scenario can set its next version.
    let allow = format!("{}services = [\"{SERVICE}\"]\n", config_text(STATIC_ISSUER));
    let (oidc_now, _) = oidc_clock();
    let verifier = Verifier::new(
        OidcConfig::parse(&allow).expect("the allow-list"),
        Arc::new(StaticKeys(jwks_of(&[&keys()[0]]))),
        Duration::from_secs(30),
        oidc_now,
        spent(),
    );
    let clock: Clock = Arc::new(|| Duration::from_secs(1_000));
    let rollout = format!("[[service]]\nname = \"{SERVICE}\"\nroles = [\"daemon\"]\n");
    Arc::new(
        Coordinator::new(desired, Arc::new(verifier), OPERATOR.to_owned(), clock)
            .with_services(Services::parse(&rollout).expect("the rollout file"))
            .with_quiet(Duration::ZERO)
            .with_ops(ops)
            .with_verdicts(verdicts),
    )
}

/// CI sets `web` to `version`, with the package digest [`d`].
fn set_desired(coordinator: &Coordinator, version: &str) {
    let token = mint(&keys()[0], &good_claims(STATIC_ISSUER));
    let body = format!(
        r#"{{"service":"{SERVICE}","version":"{version}","release_digest":"{}"}}"#,
        d()
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a runtime");
    runtime
        .block_on(coordinator.set_desired(Some(&token), body.as_bytes()))
        .expect("CI sets the version");
}

/// Where the coordinator says `web` stands: `converged`, `in_progress`, `blocked` or `halted`.
fn convergence(coordinator: &Coordinator) -> String {
    let status = coordinator.status(Some(OPERATOR)).expect("the status");
    let service = &status.per_service[SERVICE];
    let read = serde_json::to_value(&service.convergence).expect("JSON");
    read["state"].as_str().expect("a state").to_owned()
}

fn wire_version(text: &str) -> hostkeeper_wire::version::Version {
    hostkeeper_wire::version::Version::parse(text).expect("a version the wire carries")
}

fn wire_health(health: Health) -> wire::Health {
    match health {
        Health::Healthy => wire::Health::Healthy,
        Health::Unhealthy => wire::Health::Unhealthy,
        Health::Absent => wire::Health::Absent,
    }
}

fn wire_phase(phase: Phase) -> wire::Phase {
    match phase {
        Phase::BeforeCommit => wire::Phase::BeforeCommit,
        Phase::GateRelease => wire::Phase::GateRelease,
        Phase::GateEnvironment => wire::Phase::GateEnvironment,
        Phase::AfterCommit => wire::Phase::AfterCommit,
        Phase::Halt => wire::Phase::Halt,
    }
}

fn wire_class(class: Class) -> wire::Class {
    match class {
        Class::Release => wire::Class::Release,
        Class::Environment => wire::Class::Environment,
    }
}

/// A verdict as the coordinator reads it: its order, outcome, phase, class and reason.
fn update_result(result: &OrderResult) -> UpdateResult {
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

fn service_beat(facts: &ServiceFacts) -> ServiceBeat {
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
    }
}

/// The loop's heartbeat as the coordinator reads it: every service, and the features announced.
fn wire_beat(beat: &Heartbeat) -> wire::Heartbeat {
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
        agent: None,
        sent_unix: None,
        cert_not_after_unix: None,
        last_renew_error: None,
    }
}

/// The coordinator's typed order as the loop reads it: the protobuf order it sends, word for
/// word in the loop's types.
fn link_order(ordered: &Ordered) -> Order {
    let order = ordered.to_proto();
    let target = |service: &str, version: &str, digest: &str, index: &str| Target {
        service: ServiceName::new(service).expect("a service"),
        version: Version::new(version).expect("a version"),
        digest: PackageDigest::new(digest).expect("a digest"),
        index_digest: (!index.is_empty())
            .then(|| PackageDigest::new(index).expect("an index digest")),
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
struct InProcess {
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
            .heartbeat_with_orders(who, wire_beat(beat))
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
        })
    }
}

/// A host whose daemon started on `rig`, its loop over the in-process link to `coordinator`
/// announcing `features`, the jobs of its orders kept until the test runs them.
struct Host {
    heartbeater: Heartbeater,
    dispatch: Arc<ManualDispatch>,
    log: Arc<RecordingLog>,
    ordered: Arc<Mutex<Vec<Option<Order>>>>,
    _started: start::Started,
}

impl Host {
    fn new(rig: &Rig, coordinator: &Arc<Coordinator>) -> Host {
        let started = start::startup(&rig.settings(), rig.ports(), OWN).expect("the daemon starts");
        assert_eq!(
            started.boot_report().ready,
            Some(Ok(())),
            "the boot pass ran"
        );
        let daemon = Arc::clone(started.daemon());
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
    fn beat(&mut self) {
        assert_eq!(self.heartbeater.beat_once(), BeatOutcome::Sent);
    }

    /// The order of each answer, in order.
    fn ordered(&self) -> Vec<Option<Order>> {
        self.ordered
            .lock()
            .expect("the log is not poisoned")
            .clone()
    }
}

/// Catches a daemon the coordinator never orders a first install (it did not announce
/// `install-v1`), a loop that drops an install order or refuses it for its service not being
/// installed, and an install that does not reach the executor's install path: the host, which
/// does not run `web`, is ordered its install at the coordinator's first answer; the real
/// executor runs the hook's four steps; the next heartbeat lists `web` at the version with the
/// install's verdict, and the coordinator reads `web` converged and orders nothing more.
#[test]
fn an_install_the_coordinator_orders_installs_the_service_and_converges() {
    let rig = Rig::new("typed-install");
    rig.stage(SERVICE, TARGET);
    for (step, line) in HOOK_LINES {
        rig.launcher.replies(SERVICE, step, &[(0, line)]);
    }
    let dir = Scratch::new("typed-install-coordinator");
    let coordinator = coordinator(dir.path());
    let mut host = Host::new(&rig, &coordinator);

    host.beat();
    let ordered = host.ordered();
    let install = ordered[0].clone().expect("an order");
    assert!(
        matches!(&install.verb, Verb::Install(target)
            if target.service.as_str() == SERVICE && target.version.as_str() == TARGET),
        "{install:?}"
    );
    assert!(host.dispatch.run_next());
    assert_eq!(
        rig.launcher.steps(SERVICE),
        [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup]
    );

    host.beat();
    host.beat();
    assert_eq!(host.ordered()[1..], [None, None]);
    assert_eq!(convergence(&coordinator), "converged");
    assert!(ANNOUNCED.contains(&Feature::InstallV1));
}

/// The version of `web` that `Hosts.Get` answers installed on `host-1`; none when it lists
/// none.
fn listed_version(coordinator: &Coordinator) -> Option<String> {
    let operator = Caller::Operator("operator".parse().expect("a token name"));
    let request = GetHostRequest {
        node_id: "host-1".to_owned(),
    };
    let host = coordinator
        .get_host(&operator, &request)
        .expect("Hosts.Get answers the host")
        .host
        .expect("a host");
    host.services
        .into_iter()
        .find(|service| service.service == SERVICE)
        .and_then(|service| service.installed_version)
}

/// The heartbeat loop's waits: in the first, the install the first heartbeat ordered runs to its
/// end; the second stops the loop. Before each wait it notes what `Hosts.Get` lists of `web`.
struct InstallInTheWait {
    dispatch: Arc<ManualDispatch>,
    coordinator: Arc<Coordinator>,
    /// Each wait asked, in seconds, and the version `Hosts.Get` listed as it began.
    waits: Mutex<Vec<(u64, Option<String>)>>,
}

impl Sleeper for InstallInTheWait {
    fn sleep(&self, secs: u64) -> Tick {
        let mut waits = self.waits.lock().expect("the log is not poisoned");
        waits.push((secs, listed_version(&self.coordinator)));
        if waits.len() == 1 {
            assert!(self.dispatch.run_next(), "the install runs");
            Tick::Beat
        } else {
            Tick::Stop
        }
    }
}

/// Catches a host whose new service the coordinator lists only a heartbeat period after its
/// install ended: the coordinator orders the install at the first heartbeat, the real executor
/// runs it to its end within the first second of the loop's wait, and by the next wait
/// `Hosts.Get` lists `web` installed at the target, with fewer seconds waited in between than
/// one heartbeat period, on the loop's own sleeper and not the wall clock.
#[test]
fn hosts_get_lists_an_installed_service_without_another_heartbeat_period() {
    let rig = Rig::new("typed-install-listed");
    rig.stage(SERVICE, TARGET);
    for (step, line) in HOOK_LINES {
        rig.launcher.replies(SERVICE, step, &[(0, line)]);
    }
    let dir = Scratch::new("typed-install-listed-coordinator");
    let coordinator = coordinator(dir.path());
    let host = Host::new(&rig, &coordinator);
    let sleeper = InstallInTheWait {
        dispatch: Arc::clone(&host.dispatch),
        coordinator: Arc::clone(&coordinator),
        waits: Mutex::new(Vec::new()),
    };
    let mut seen = Vec::new();
    host.heartbeater
        .run(&sleeper, &mut |outcome| seen.push(outcome.clone()));

    assert_eq!(seen, [BeatOutcome::Sent, BeatOutcome::Sent]);
    assert_eq!(
        rig.launcher.steps(SERVICE),
        [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup]
    );
    let ordered = host
        .ordered
        .lock()
        .expect("the log is not poisoned")
        .clone();
    assert!(
        matches!(&ordered[0], Some(Order { verb: Verb::Install(target), .. })
            if target.service.as_str() == SERVICE && target.version.as_str() == TARGET),
        "{ordered:?}"
    );
    let waits = sleeper
        .waits
        .lock()
        .expect("the log is not poisoned")
        .clone();
    let listed: Vec<Option<&str>> = waits.iter().map(|(_, at)| at.as_deref()).collect();
    assert_eq!(listed, [None, Some(TARGET)], "{waits:?}");
    let waited = waits[0].0;
    assert!(
        waited < DEFAULT_HEARTBEAT_SECS.get(),
        "listed after {waited} seconds of a {DEFAULT_HEARTBEAT_SECS}-second period"
    );
    assert_eq!(listed_version(&coordinator).as_deref(), Some(TARGET));
}

/// Catches an abort the coordinator never sends (the daemon did not announce `abort-v1`), an
/// abort the loop answers `busy` because the update it is for runs, one run on the loop's own
/// thread, and an abort whose update goes on or reports no halt: an update of `web` holds in
/// `prepare` until its unit is stopped; an operator's `Hosts.Abort` is answered at the next
/// heartbeat; the loop hands it to the executor beside the update, which stops the step and ends
/// the update in the halt phase (`aborted_by_operator`) with no later step run; the abort's
/// answer is logged, and the next heartbeat reports the update's halt, which halts `web` on the
/// coordinator.
#[test]
fn an_abort_the_coordinator_queues_stops_the_live_update_and_its_halt_is_reported() {
    let rig = Rig::new("typed-abort");
    rig.add(SERVICE, TARGET);
    for (step, line) in HOOK_LINES {
        rig.launcher.replies(SERVICE, step, &[(0, line)]);
    }
    let gate = rig.launcher.hold_until_stopped(SERVICE, Step::Prepare);
    let dir = Scratch::new("typed-abort-coordinator");
    let coordinator = coordinator(dir.path());
    let mut host = Host::new(&rig, &coordinator);

    host.beat();
    let update = host.ordered()[0].clone().expect("an order");
    assert!(matches!(update.verb, Verb::Update(_)), "{update:?}");
    let worker = {
        let dispatch = Arc::clone(&host.dispatch);
        spawn("the update's worker", move || dispatch.run_next())
    };
    gate.entered(&worker).expect("the held step is entered");

    let operator = Caller::Operator("operator".parse().expect("a token name"));
    let queued = coordinator
        .abort(
            &operator,
            AbortRequest {
                node_id: "host-1".to_owned(),
                service: SERVICE.to_owned(),
                reason: "stuck in prepare".to_owned(),
            },
        )
        .expect("queued");
    assert!(queued.queued);
    host.beat();
    let abort = host.ordered()[1].clone().expect("an order");
    assert_eq!(
        abort.verb,
        Verb::Abort(ServiceName::new(SERVICE).expect("a service"))
    );
    assert!(host.dispatch.run_next());
    assert!(worker.join().expect("the update's worker"));
    assert_eq!(rig.launcher.steps(SERVICE), [Step::Prepare]);
    assert_eq!(
        host.log.lines(),
        [format!(
            "order {} try 1 of {SERVICE} answered failed: aborted_by_operator",
            abort.id.as_str()
        )]
    );

    host.beat();
    assert_eq!(host.ordered()[2], None);
    assert_eq!(convergence(&coordinator), "halted");
}

/// The next version CI sets after the install.
const NEXT: &str = "3.0.0";

/// The order of the answer `n` (from 0) and its try, as the loop read it.
fn order_and_try(host: &Host, n: usize) -> (Verb, u64) {
    let order = host.ordered()[n].clone().expect("an order");
    (order.verb, order.order_try)
}

/// `web` at `version`, as an update or an install of the package [`d`] names it.
fn web_at(version: &str) -> Target {
    Target {
        service: ServiceName::new(SERVICE).expect("a service"),
        version: Version::new(version).expect("a version"),
        digest: PackageDigest::new(&d()).expect("a digest"),
        index_digest: None,
    }
}

/// Catches the update after an install refused `try_conflict` (komira-ai/hostkeeper#115): the
/// coordinator numbered tries per order id, so the update of a service the host had just
/// installed went at try 1, the try the daemon's step record held for the install, and the daemon
/// refused it for its other package. The coordinator installs `web` at [`TARGET`] (try 1); CI sets
/// [`NEXT`]; the coordinator orders the update at try 2; the real executor runs all four steps of
/// it, and the coordinator reads `web` converged at [`NEXT`] and orders nothing more.
#[test]
fn an_update_after_an_install_runs_and_converges() {
    let rig = Rig::new("typed-install-update");
    rig.stage(SERVICE, TARGET);
    rig.stage(SERVICE, NEXT);
    for (step, line) in HOOK_LINES {
        rig.launcher.replies(SERVICE, step, &[(0, line), (0, line)]);
    }
    let dir = Scratch::new("typed-install-update-coordinator");
    let coordinator = coordinator(dir.path());
    let mut host = Host::new(&rig, &coordinator);

    host.beat();
    assert_eq!(order_and_try(&host, 0), (Verb::Install(web_at(TARGET)), 1));
    assert!(host.dispatch.run_next());
    host.beat();
    assert_eq!(host.ordered()[1], None);
    assert_eq!(convergence(&coordinator), "converged");

    set_desired(&coordinator, NEXT);
    host.beat();
    assert_eq!(order_and_try(&host, 2), (Verb::Update(web_at(NEXT)), 2));
    assert!(host.dispatch.run_next());
    assert_eq!(
        rig.launcher.steps(SERVICE),
        [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup].repeat(2)
    );
    host.beat();
    host.beat();
    assert_eq!(host.ordered()[3..], [None, None]);
    assert_eq!(convergence(&coordinator), "converged");
}

/// Catches a host ordered from try 1 again after its daemon restarts: the daemon installs `web`
/// (try 1) and reports the install's verdict, which the coordinator stores and acknowledges; the
/// daemon restarts, and its loop, which no longer sends that verdict, lists `web` with none. The
/// coordinator reads the host as still reporting the verdict it acknowledged (design 8.2), so it
/// orders the next update one past it, at try 2, which the restarted daemon runs to convergence.
/// At try 1 the daemon, whose step record holds the install's try 1, would refuse it
/// `try_conflict`.
#[test]
fn a_restarted_daemon_is_ordered_one_past_the_try_it_reported() {
    let rig = Rig::new("typed-restart-update");
    rig.stage(SERVICE, TARGET);
    rig.stage(SERVICE, NEXT);
    for (step, line) in HOOK_LINES {
        rig.launcher.replies(SERVICE, step, &[(0, line), (0, line)]);
    }
    let dir = Scratch::new("typed-restart-update-coordinator");
    let coordinator = coordinator(dir.path());
    let mut host = Host::new(&rig, &coordinator);
    host.beat();
    assert_eq!(order_and_try(&host, 0), (Verb::Install(web_at(TARGET)), 1));
    assert!(host.dispatch.run_next());
    host.beat();
    assert_eq!(host.ordered()[1], None);
    assert_eq!(convergence(&coordinator), "converged");
    drop(host);

    set_desired(&coordinator, NEXT);
    let mut host = Host::new(&rig, &coordinator);
    host.beat();
    assert_eq!(order_and_try(&host, 0), (Verb::Update(web_at(NEXT)), 2));
    assert!(host.dispatch.run_next());
    assert_eq!(
        rig.launcher.steps(SERVICE),
        [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup].repeat(2)
    );
    host.beat();
    assert_eq!(host.ordered()[1], None);
    assert_eq!(convergence(&coordinator), "converged");
}

/// The version CI sets after [`NEXT`], once both the daemon and the coordinator restarted.
const LAST: &str = "4.0.0";

/// Catches a try numbered from what the beat carries now instead of the highest the host
/// reported: an acknowledged verdict is no longer sent, and after a restart of both the daemon
/// and the coordinator nothing but the coordinator's verdicts file remembers it. The coordinator
/// installs `web` at [`TARGET`] (try 1) and updates it to [`NEXT`] (try 2), each verdict
/// reported and acknowledged; the daemon and the coordinator both restart on their own files; CI
/// sets [`LAST`]; the restarted coordinator orders the update at try 3, which the restarted
/// daemon runs through all four steps, and `web` reads converged at [`LAST`]. At try 1 or 2 the
/// daemon, whose step record holds try 2, would refuse it `stale_try` or `try_conflict`.
#[test]
fn install_update_restart_update_runs_every_order_one_try_past_the_last() {
    let rig = Rig::new("typed-twice");
    for version in [TARGET, NEXT, LAST] {
        rig.stage(SERVICE, version);
    }
    for (step, line) in HOOK_LINES {
        rig.launcher
            .replies(SERVICE, step, &[(0, line), (0, line), (0, line)]);
    }
    let dir = Scratch::new("typed-twice-coordinator");
    let first = coordinator(dir.path());
    let mut host = Host::new(&rig, &first);

    host.beat();
    assert_eq!(order_and_try(&host, 0), (Verb::Install(web_at(TARGET)), 1));
    assert!(host.dispatch.run_next());
    host.beat();
    assert_eq!(host.ordered()[1], None);

    set_desired(&first, NEXT);
    host.beat();
    assert_eq!(order_and_try(&host, 2), (Verb::Update(web_at(NEXT)), 2));
    assert!(host.dispatch.run_next());
    host.beat();
    host.beat();
    assert_eq!(host.ordered()[3..], [None, None]);
    assert_eq!(convergence(&first), "converged");
    drop(host);
    drop(first);

    let second = coordinator(dir.path());
    let mut host = Host::new(&rig, &second);
    set_desired(&second, LAST);
    host.beat();
    assert_eq!(order_and_try(&host, 0), (Verb::Update(web_at(LAST)), 3));
    assert!(host.dispatch.run_next());
    assert_eq!(
        rig.launcher.steps(SERVICE),
        [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup].repeat(3)
    );
    host.beat();
    assert_eq!(host.ordered()[1], None);
    assert_eq!(convergence(&second), "converged");
}
