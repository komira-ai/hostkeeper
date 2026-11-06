//! The first update end to end, in process: the real coordinator (`hostkeeper-coordinator`), the
//! real heartbeat loop over the daemon (`hostkeeper_daemon::links`), and the real executor with
//! its step runner and boot pass. The fakes are the network transport (a link that calls the
//! coordinator's heartbeat with an explicit host identity), the service manager and the package
//! source (`FakePackages`, the scripted launcher).
//!
//! The daemon announces `order-v1`, so the coordinator's update carries its order terms: the
//! order id, the try and the package digest CI registered with the desired version. The
//! in-process link reads the order from those terms, as the daemon's JSON link does.

mod support;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_coordinator::desired::DesiredStore;
use hostkeeper_coordinator::http::{CONVERGENCE_PATH, DESIRED_PATH, Surface, handle};
use hostkeeper_coordinator::ops::OpsStore;
use hostkeeper_coordinator::service::{Clock, Coordinator};
use hostkeeper_coordinator::services::Services;
use hostkeeper_daemon::Daemon;
use hostkeeper_daemon::link_adapters::{DaemonHalts, DaemonObserver, DaemonRecords};
use hostkeeper_daemon::links::{Links, PLATFORM};
use hostkeeper_daemon::start;
use hostkeeper_hook::Step;
use hostkeeper_link::testing::{
    FakeCerts, FakeLink, FakeSink, ManualDispatch, RecordingLog, ScriptedSleeper, identity,
};
use hostkeeper_link::{
    ANNOUNCED, Acknowledged, BeatOutcome, CertState, ClientIdentity, CoordinatorLink,
    DEFAULT_HEARTBEAT_SECS, Feature, Health, Heartbeat, Heartbeater, LinkError, LoopConfig, Order,
    OrderId, OrderRef, OrderResult, Outcome, PackageDigest, Ports, Reason, Reply, ServiceFacts,
    StopFlag, Target, Verb,
};
use hostkeeper_oidc::testkit::{
    STATIC_ISSUER, StaticKeys, clock as oidc_clock, config_text, good_claims, jwks_of, keys, mint,
    spent,
};
use hostkeeper_oidc::{OidcConfig, Verifier};
use hostkeeper_wire::identity::Identity;
use hostkeeper_wire::wire::{self, Answer, ServiceBeat, ServiceReply, UpdateResult};
use http_body_util::{BodyExt, Full};
use serde_json::Value;
use support::{OWN, Rig, Scratch, d};

/// The service of the scenario, its version before and the version CI sets.
const SERVICE: &str = "web";
const BEFORE: &str = "0.0.0";
const TARGET: &str = "2.0.0";

/// The operator token of the coordinator; nothing here uses it.
const OPERATOR: &str = "operator-token-0123456789abcdef0123456789";

/// How long the coordinator's handler waits for a body: the whole body is there.
const PATIENCE: Duration = Duration::from_secs(30);

/// How many heartbeats the host sends.
const BEATS: usize = 3;

/// The closing JSON line of each step of a conforming hook that updates: `prepare` changes
/// nothing it cannot undo, `cutover` commits, `verify` asks for no promotion, `cleanup` says
/// nothing more.
const HOOK_LINES: [(Step, &str); 4] = [
    (Step::Prepare, r#"{"reversible":true}"#),
    (Step::Cutover, r#"{"reversible":false}"#),
    (Step::Verify, r#"{"promotion_pending":false}"#),
    (Step::Cleanup, "{}"),
];

/// The real coordinator: `web` runs on the role `daemon`, CI's allow-list admits it, and it
/// orders from its start (no quiet period), on a clock that stays at second 1000.
fn coordinator(dir: &Path) -> Arc<Coordinator> {
    let desired = DesiredStore::open(&dir.join("desired.json"), None).expect("the desired state");
    let ops = OpsStore::open(&dir.join("desired.json.ops")).expect("the ops store");
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
            .with_ops(ops),
    )
}

/// The coordinator's answer to one request of CI on its private listener, with a token of its
/// own: the status and the body.
fn ci_call(coordinator: &Coordinator, method: &str, path: &str, body: &str) -> (u16, String) {
    let token = mint(&keys()[0], &good_claims(STATIC_ISSUER));
    let request = hyper::Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {token}"))
        .body(Full::new(Bytes::from(body.to_owned())))
        .expect("a request");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    runtime.block_on(async {
        let response = handle(coordinator, Surface::Control, &None, request, PATIENCE).await;
        let status = response.status().as_u16();
        let body = response
            .into_body()
            .collect()
            .await
            .expect("a body")
            .to_bytes()
            .to_vec();
        (status, String::from_utf8(body).expect("text"))
    })
}

/// Where the coordinator says `web` stands: `converged`, `in_progress`, `blocked` or `halted`.
fn convergence(coordinator: &Coordinator) -> String {
    let path = format!("{CONVERGENCE_PATH}?service={SERVICE}");
    let (status, body) = ci_call(coordinator, "GET", &path, "");
    assert_eq!(status, 200, "{body}");
    let read: Value = serde_json::from_str(&body).expect("JSON");
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

fn update_result(result: &OrderResult) -> UpdateResult {
    let outcome = match result.outcome {
        Outcome::Succeeded => wire::Outcome::Succeeded,
        Outcome::Failed | Outcome::Deferred { .. } => wire::Outcome::Failed,
    };
    UpdateResult {
        order_id: wire::OrderId::parse(result.order.id.as_str()),
        order_try: Some(result.order.order_try),
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

/// The loop's heartbeat as the coordinator reads it: every service in `services`, and the
/// features it announces.
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
    }
}

/// The coordinator's answer as the loop reads it: its update an order of the version under the
/// answer's order terms (an update without them is no order), its features, its halt
/// generations and its acknowledgements.
fn reply_of(answer: &Answer) -> Reply {
    let order = match (
        &answer.reply,
        &answer.order_id,
        answer.order_try,
        &answer.package_digest,
    ) {
        (
            ServiceReply::Update {
                service: Some(service),
                version,
            },
            Some(id),
            Some(order_try),
            Some(digest),
        ) => Some(Order {
            id: OrderId::new(id.as_str()).expect("an id"),
            order_try,
            verb: Verb::Update(Target {
                service: ServiceName::new(service).expect("a service"),
                version: Version::new(version.as_str()).expect("a version"),
                digest: PackageDigest::new(digest.as_str()).expect("a digest"),
            }),
        }),
        _ => None,
    };
    Reply {
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
    }
}

/// The network transport, in process: each heartbeat goes to the coordinator's heartbeat as
/// the host `host-1` of the role `daemon`, whatever certificate the loop presents.
struct InProcess {
    coordinator: Arc<Coordinator>,
    /// What each heartbeat said of the service and what its answer ordered, in order.
    seen: Arc<Mutex<Vec<Seen>>>,
}

/// One heartbeat of the scenario: whether it carried a verdict for the service, and whether the
/// coordinator's answer ordered anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Seen {
    verdict: bool,
    ordered: bool,
}

impl CoordinatorLink for InProcess {
    fn heartbeat(&self, _identity: &ClientIdentity, beat: &Heartbeat) -> Result<Reply, LinkError> {
        let who = Identity {
            host_id: "host-1".parse().expect("a host id"),
            role: "daemon".parse().expect("a role"),
        };
        let reply = self
            .coordinator
            .heartbeat(who, wire_beat(beat))
            .map(|answer| reply_of(&answer))
            .map_err(|_| LinkError::Refused(Reason::new("forbidden").expect("a token")))?;
        self.seen
            .lock()
            .expect("the log is not poisoned")
            .push(Seen {
                verdict: beat.services.iter().any(|s| s.last_update.is_some()),
                ordered: reply.order.is_some(),
            });
        Ok(reply)
    }
}

/// Which part of the scenario is replaced by a no-op.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stub {
    /// Nothing: every part is real.
    Nothing,
    /// The link answers every heartbeat with nothing, and never reaches the coordinator.
    Coordinator,
    /// No loop: the same heartbeat, of the host as it starts, is sent at every period and its
    /// answer dropped.
    Loop,
    /// The executor answers every order succeeded and runs nothing.
    Executor,
}

/// What the scenario came to: where the coordinator says `web` stands, and the steps the
/// launcher ran, in order.
struct Ran {
    state: String,
    steps: Vec<Step>,
    /// The heartbeats that reached the coordinator.
    seen: Vec<Seen>,
}

/// The host as it starts: `web` at 0.0.0, healthy, nothing running.
fn first_beat() -> Heartbeat {
    Heartbeat {
        platform: PLATFORM,
        agent_version: Version::from(OWN),
        features: ANNOUNCED.to_vec(),
        services: vec![ServiceFacts {
            name: ServiceName::new(SERVICE).expect("a service"),
            version: Version::new(BEFORE).expect("a version"),
            health: Health::Healthy,
            updating: None,
            last_update: None,
            deferred: None,
            health_detail: None,
            observed: None,
        }],
    }
}

/// The heartbeat loop over `daemon` whose executor runs nothing and answers succeeded.
fn without_executor(daemon: &Arc<Daemon>, links: &Links) -> Heartbeater {
    Heartbeater::new(
        LoopConfig {
            platform: PLATFORM,
            agent_version: Version::from(OWN),
            state_dir: daemon.ctx().dirs.state.clone(),
            period_secs: links.period,
        },
        Ports {
            link: Arc::clone(&links.link),
            certs: Arc::clone(&links.certs),
            records: Arc::new(DaemonRecords::new(Arc::clone(daemon))),
            observer: Arc::new(DaemonObserver::new(Arc::clone(daemon))),
            orders: Arc::new(FakeSink::succeeding()),
            dispatch: Arc::clone(&links.dispatch),
            halts: Arc::new(DaemonHalts::new(Arc::clone(daemon))),
            clock: Arc::clone(&links.clock),
            log: Arc::clone(&links.log),
        },
    )
}

/// CI sets `web` to 2.0.0 on the coordinator, and the host, which runs 0.0.0 of it, beats
/// [`BEATS`] times, running each order of a reply before the next heartbeat; with `stub`
/// replaced by a no-op.
fn scenario(name: &str, stub: Stub) -> Ran {
    let rig = Rig::new(name);
    rig.add(SERVICE, TARGET);
    for (step, line) in HOOK_LINES {
        rig.launcher.replies(SERVICE, step, &[(0, line)]);
    }
    let started = start::startup(&rig.settings(), rig.ports(), OWN).expect("the daemon starts");
    assert_eq!(
        started.boot_report().ready,
        Some(Ok(())),
        "the boot pass ran"
    );
    let daemon = Arc::clone(started.daemon());

    let dir = Scratch::new(&format!("{name}-coordinator"));
    let coordinator = coordinator(dir.path());
    let desired = format!(
        r#"{{"service":"{SERVICE}","version":"{TARGET}","release_digest":"{}"}}"#,
        d()
    );
    let (status, body) = ci_call(&coordinator, "POST", DESIRED_PATH, &desired);
    assert_eq!(status, 200, "{body}");

    let seen = Arc::new(Mutex::new(Vec::new()));
    let link: Arc<dyn CoordinatorLink> = match stub {
        Stub::Coordinator => Arc::new(FakeLink::default()),
        _ => Arc::new(InProcess {
            coordinator: Arc::clone(&coordinator),
            seen: Arc::clone(&seen),
        }),
    };
    let dispatch = Arc::new(ManualDispatch::default());
    let links = Links {
        enroller: Box::new(|| Err("the scenario has no enrollment".to_owned())),
        link,
        certs: Arc::new(FakeCerts::new(CertState::Enrolled(identity("host-1")))),
        clock: Arc::new(hostkeeper_link::testing::FakeClock::new(2_000_000_000, 7)),
        log: Arc::new(RecordingLog::default()),
        sleeper: Box::new(ScriptedSleeper::new(0)),
        period: DEFAULT_HEARTBEAT_SECS,
        dispatch: dispatch.clone(),
        stop: StopFlag::default(),
    };
    match stub {
        Stub::Loop => {
            for _ in 0..BEATS {
                let answered = links.link.heartbeat(&identity("host-1"), &first_beat());
                assert!(answered.is_ok(), "{answered:?}");
            }
        }
        Stub::Executor => beat(&mut without_executor(&daemon, &links), &dispatch),
        Stub::Nothing | Stub::Coordinator => beat(&mut links.heartbeater(&daemon), &dispatch),
    }
    let ran = Ran {
        state: convergence(&coordinator),
        steps: rig.launcher.steps(SERVICE),
        seen: seen.lock().expect("the log is not poisoned").clone(),
    };
    drop(started);
    ran
}

/// [`BEATS`] heartbeats of `heartbeater`, each answered, and every order a reply started run to
/// its end before the next one.
fn beat(heartbeater: &mut Heartbeater, dispatch: &ManualDispatch) {
    for _ in 0..BEATS {
        assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
        while dispatch.run_next() {}
    }
}

/// Catches a link of the daemon to its coordinator that does not carry an update to its end:
/// CI sets the desired version on the real coordinator through `/v1/desired`; the real loop's
/// heartbeat is answered with the update; the real executor runs the hook's four steps in their
/// order, `prepare`, `cutover`, `verify`, `cleanup`, through the step runner; the next heartbeat
/// reports the host at the new version; and the coordinator's convergence of the service reads
/// converged.
#[test]
fn a_desired_version_set_on_the_coordinator_reaches_a_fixture_hook_and_reads_converged() {
    let ran = scenario("e2e-real", Stub::Nothing);
    assert_eq!(
        ran.steps,
        [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup]
    );
    assert_eq!(ran.state, "converged");
}

/// Catches an end-to-end test that converges without the coordinator: a link that never reaches
/// it answers nothing, no step runs, and the service does not converge.
#[test]
fn the_end_to_end_fails_when_the_coordinator_is_stubbed() {
    let ran = scenario("e2e-no-coordinator", Stub::Coordinator);
    assert!(ran.steps.is_empty(), "{:?}", ran.steps);
    assert_ne!(ran.state, "converged");
}

/// Catches an end-to-end test that converges without the loop: the coordinator is told of the
/// host as it starts and answers the update, which nobody takes; no step runs, and the service
/// does not converge.
#[test]
fn the_end_to_end_fails_when_the_loop_is_stubbed() {
    let ran = scenario("e2e-no-loop", Stub::Loop);
    assert!(ran.steps.is_empty(), "{:?}", ran.steps);
    assert_ne!(ran.state, "converged");
}

/// Catches an end-to-end test that converges without the executor: the loop takes the update to
/// an executor that answers succeeded and runs nothing; the host still runs 0.0.0, and the
/// service does not converge.
#[test]
fn the_end_to_end_fails_when_the_executor_is_stubbed() {
    let ran = scenario("e2e-no-executor", Stub::Executor);
    assert!(ran.steps.is_empty(), "{:?}", ran.steps);
    assert_ne!(ran.state, "converged");
}

/// Catches a verdict that is not acknowledged end to end, or an acknowledgement that does not
/// reach the daemon's loop: the first heartbeat orders the update, the second carries the
/// daemon's verdict, the coordinator stores it and names it in its answer, and the third carries
/// no verdict for the service while the coordinator, still reading the acknowledged verdict,
/// orders nothing.
#[test]
fn the_daemons_verdict_is_acknowledged_and_not_sent_or_reordered_again() {
    let ran = scenario("e2e-ack", Stub::Nothing);
    assert_eq!(
        ran.seen,
        [
            Seen {
                verdict: false,
                ordered: true
            },
            Seen {
                verdict: true,
                ordered: false
            },
            Seen {
                verdict: false,
                ordered: false
            },
        ]
    );
    assert_eq!(ran.state, "converged");
}
