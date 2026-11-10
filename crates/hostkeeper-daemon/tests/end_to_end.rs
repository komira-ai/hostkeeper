//! The first update end to end, in process: the real coordinator (`hostkeeper-coordinator`), the
//! real heartbeat loop over the daemon (`hostkeeper_daemon::links`), and the real executor with
//! its step runner and boot pass. The fakes are the network transport (a link that calls the
//! coordinator's gRPC heartbeat as an explicit host), the service manager and the package source
//! (the fake of apt and dpkg, which the package of the desired version is published in, and the
//! scripted launcher).
//!
//! CI sets the desired version with the release that lists the entry of that package: the daemon
//! lists `apt`, so the coordinator picks the entry for it and orders the update as a typed order
//! that carries it. A version set with no release orders nothing.

mod support;
mod support_apt;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_coordinator::http::{CONVERGENCE_PATH, Surface, handle};
use hostkeeper_coordinator::service::Coordinator;
use hostkeeper_daemon::link_adapters::{
    DaemonHalts, DaemonObserver, DaemonRecords, DaemonRenewal, NoTrustedRoot,
};
use hostkeeper_daemon::links::{Links, PLATFORM};
use hostkeeper_daemon::start;
use hostkeeper_daemon::{Daemon, ForcedResult};
use hostkeeper_hook::Step;
use hostkeeper_link::testing::{
    FakeCerts, FakeLink, FakeSink, ManualDispatch, RecordingLog, ScriptedSleeper, identity,
};
use hostkeeper_link::{
    ANNOUNCED, BeatOutcome, CertState, CoordinatorLink, DEFAULT_HEARTBEAT_SECS, Health, Heartbeat,
    Heartbeater, LoopConfig, Ports, Renewal, ServiceFacts, StopFlag, Verb,
};
use hostkeeper_oidc::testkit::{STATIC_ISSUER, good_claims, keys, mint};
use hostkeeper_proto::hostkeeper::operator::v1 as operator;
use hostkeeper_proto::hostkeeper::operator::v1::force_request::Targets;
use hostkeeper_steps::ForceVerdict;
use http_body_util::{BodyExt, Full};
use serde_json::Value;
use support::coordinated::coordinator_over;
use support::typed::{GrpcInProcess, set_release};
use support::{OWN, Scratch};
use support_apt::{AptRig, services};

/// The service of the scenario, its version before and the version CI sets.
const SERVICE: &str = "web";
const BEFORE: &str = "0.0.0";
const TARGET: &str = "2.0.0";

/// The operator token of the coordinator; the force uses it.
const OPERATOR: &str = support::coordinated::OPERATOR;

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

/// The real coordinator: `web` runs on the role `daemon`, CI's allow-list admits it, nothing is
/// desired until CI sets a version ([`publish_and_set`]), and it orders from its start (no quiet
/// period), on a clock that stays at second 1000.
fn coordinator(dir: &Path) -> Arc<Coordinator> {
    coordinator_over(dir, SERVICE, "")
}

/// `web` at [`TARGET`] published in the repository of `apt`, and CI sets `web` to it with the
/// release that lists the entry of that package.
fn publish_and_set(apt: &AptRig, coordinator: &Arc<Coordinator>) {
    let entry = services::publish(apt, SERVICE, TARGET);
    set_release(coordinator, SERVICE, TARGET, &entry);
}

/// The coordinator's answer to one request of CI on its private listener, with a token of its
/// own: the status and the body.
fn ci_call(coordinator: &Arc<Coordinator>, method: &str, path: &str, body: &str) -> (u16, String) {
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
fn convergence(coordinator: &Arc<Coordinator>) -> String {
    let path = format!("{CONVERGENCE_PATH}?service={SERVICE}");
    let (status, body) = ci_call(coordinator, "GET", &path, "");
    assert_eq!(status, 200, "{body}");
    let read: Value = serde_json::from_str(&body).expect("JSON");
    read["state"].as_str().expect("a state").to_owned()
}

/// One heartbeat of the scenario: whether it carried a verdict for the service, and whether the
/// coordinator's answer ordered anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Seen {
    verdict: bool,
    ordered: bool,
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

/// The host as it starts: `web` at 0.0.0, healthy, installed with `apt`, nothing running.
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
            record_try: None,
            installer: Some("apt".to_owned()),
            drift: None,
        }],
        sent_unix: 0,
        renewal: Renewal::default(),
        trusted_root: None,
        installers: vec!["apt".to_owned()],
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
            installers: vec!["apt".to_owned()],
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
            renewal: Arc::new(DaemonRenewal::new(links.renewal.clone())),
            trusted_root: Arc::new(NoTrustedRoot),
        },
    )
}

/// The loop's links over `link`, the jobs of its orders kept until the test runs them.
fn links_over(link: Arc<dyn CoordinatorLink>, dispatch: &Arc<ManualDispatch>) -> Links {
    Links {
        enroller: Box::new(|| Err("the scenario has no enrollment".to_owned())),
        link,
        certs: Arc::new(FakeCerts::new(CertState::Enrolled(identity("host-1")))),
        clock: Arc::new(hostkeeper_link::testing::FakeClock::new(2_000_000_000, 7)),
        log: Arc::new(RecordingLog::default()),
        sleeper: Box::new(ScriptedSleeper::new(0)),
        period: DEFAULT_HEARTBEAT_SECS,
        dispatch: dispatch.clone(),
        stop: StopFlag::default(),
        enroll_sleeper: Box::new(ScriptedSleeper::new(0)),
        renewal: hostkeeper_enroll::RenewalState::default(),
        watchdog: Box::new(hostkeeper_daemon::NoWatchdog),
        console: None,
    }
}

/// `web` runs [`BEFORE`] on `apt`, installed by an earlier order, and the service manager runs
/// its unit; the daemon of the host started.
fn started_host(apt: &AptRig) -> start::Started {
    services::installed(apt, SERVICE, BEFORE);
    apt.rig.manager.set(
        "web.service",
        hostkeeper_observe::ActiveState::Active,
        Some(4242),
    );
    let started = start::startup(&apt.rig.settings(), apt.ports(), OWN).expect("the daemon starts");
    assert_eq!(
        started.boot_report().ready,
        Some(Ok(())),
        "the boot pass ran"
    );
    started
}

/// CI sets `web` to 2.0.0 on the coordinator, and the host, which runs 0.0.0 of it, beats
/// [`BEATS`] times, running each order of a reply before the next heartbeat; with `stub`
/// replaced by a no-op.
fn scenario(name: &str, stub: Stub) -> Ran {
    let apt = AptRig::new(name);
    let rig = &apt.rig;
    for (step, line) in HOOK_LINES {
        rig.launcher.replies(SERVICE, step, &[(0, line)]);
    }
    let started = started_host(&apt);
    let daemon = Arc::clone(started.daemon());

    let dir = Scratch::new(&format!("{name}-coordinator"));
    let coordinator = coordinator(dir.path());
    publish_and_set(&apt, &coordinator);

    let ordered = Arc::new(Mutex::new(Vec::new()));
    let grpc = Arc::new(GrpcInProcess::new(&coordinator, Arc::clone(&ordered)));
    let link: Arc<dyn CoordinatorLink> = match stub {
        Stub::Coordinator => Arc::new(FakeLink::default()),
        _ => grpc.clone(),
    };
    let dispatch = Arc::new(ManualDispatch::default());
    let links = links_over(link, &dispatch);
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
    let sent = grpc.sent.lock().expect("the log is not poisoned").clone();
    let answers = ordered.lock().expect("the log is not poisoned").clone();
    let ran = Ran {
        state: convergence(&coordinator),
        steps: rig.launcher.steps(SERVICE),
        seen: sent
            .iter()
            .zip(&answers)
            .map(|(request, order)| Seen {
                verdict: request
                    .services
                    .iter()
                    .any(|service| service.last_update.is_some()),
                ordered: order.is_some(),
            })
            .collect(),
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
/// CI sets the desired version, with the release that lists the entry of its package, on the real
/// coordinator; the real loop's heartbeat is answered with the update, which carries that entry;
/// the real executor stages the package from the repository and runs the hook's four steps in
/// their order, `prepare`, `cutover`, `verify`, `cleanup`, through the step runner; the next
/// heartbeat reports the host at the new version; and the coordinator's convergence of the
/// service reads converged.
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
    assert!(
        ran.seen.len() == BEATS && ran.seen.iter().all(|seen| seen.ordered),
        "{:?}",
        ran.seen
    );
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

/// What the force came to: the steps the launcher ran, the daemon's forced verdict of `web`,
/// and where the coordinator says `web` stands.
struct Forced {
    /// The verbs of the orders the coordinator's answers carried, in order.
    ordered: Vec<Option<&'static str>>,
    steps: Vec<Step>,
    forced: Option<ForcedResult>,
    state: String,
}

/// The host, which runs 0.0.0 of `web`, is heard by the coordinator; CI then sets `web` to 2.0.0
/// and an operator forces the host to it; the host beats twice more over the typed transport
/// (whose replies leave `forced-v1` out with `unadvertise`), running each order of a reply
/// before the next heartbeat.
fn forced_scenario(name: &str, unadvertise: bool) -> Forced {
    let apt = AptRig::new(name);
    let rig = &apt.rig;
    let started = started_host(&apt);
    let daemon = Arc::clone(started.daemon());

    let dir = Scratch::new(&format!("{name}-coordinator"));
    let coordinator = coordinator(dir.path());
    let dispatch = Arc::new(ManualDispatch::default());
    let answers = Arc::new(Mutex::new(Vec::new()));
    let mut grpc = GrpcInProcess::new(&coordinator, Arc::clone(&answers));
    grpc.unadvertise = unadvertise;
    let links = links_over(Arc::new(grpc), &dispatch);
    let mut heartbeater = links.heartbeater(&daemon);
    // Heard before anything is desired: told nothing, and now a host a force may target.
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    assert!(
        !dispatch.run_next(),
        "nothing is ordered before CI sets a version"
    );

    publish_and_set(&apt, &coordinator);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a runtime");
    let caller = runtime.block_on(coordinator.caller(Some(OPERATOR)));
    let response = coordinator
        .force(
            &caller,
            operator::ForceRequest {
                service: SERVICE.to_owned(),
                version: TARGET.to_owned(),
                reason: "fix the farm".to_owned(),
                targets: Some(Targets::Hosts(operator::HostList {
                    node_ids: vec!["host-1".to_owned()],
                })),
                include_held: false,
            },
        )
        .expect("the force is accepted");
    assert_eq!(response.targeted, 1, "{response:?}");

    for _ in 0..2 {
        assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
        while dispatch.run_next() {}
    }
    let status = daemon.status().expect("the status");
    let forced = status
        .services
        .iter()
        .find(|service| service.service.as_str() == SERVICE)
        .and_then(|service| service.forced());
    let ordered = answers
        .lock()
        .expect("the log is not poisoned")
        .iter()
        .map(|order| {
            order.as_ref().map(|order| match order.verb {
                Verb::Force(_) => "force",
                Verb::Update(_) => "update",
                _ => "another verb",
            })
        })
        .collect();
    let ran = Forced {
        ordered,
        steps: rig.launcher.steps(SERVICE),
        forced,
        state: convergence(&coordinator),
    };
    drop(started);
    ran
}

/// Catches a force the coordinator sends but the daemon never runs, because the coordinator
/// does not advertise `forced-v1` and the daemon runs a forced order only while it does: an
/// operator forces the host to 2.0.0 on the real coordinator; the real loop takes the typed
/// `force` and the real executor runs it, hookless (no step of the hook runs); the next
/// heartbeat reports the forced verdict, which the coordinator stores, and the service reads
/// converged.
#[test]
fn a_force_made_on_the_coordinator_is_run_by_the_daemon() {
    let ran = forced_scenario("e2e-force", false);
    assert_eq!(ran.ordered, [None, Some("force"), None]);
    assert_eq!(ran.steps, Vec::<Step>::new(), "a force runs no hook");
    assert_eq!(
        ran.forced,
        Some(ForcedResult {
            order_id: Some("force-1".to_owned()),
            order_try: 1,
            target: TARGET.to_owned(),
            verdict: ForceVerdict::Succeeded,
        })
    );
    assert_eq!(ran.state, "converged");
}

/// Catches a force end to end that runs without `forced-v1` advertised: the same scenario, its
/// replies without `forced-v1` (as the coordinator answered before it advertised it), carries
/// the force to the daemon, which does not run it, and the service stays in progress.
#[test]
fn a_force_is_not_run_while_the_reply_does_not_advertise_forced_v1() {
    let ran = forced_scenario("e2e-force-unadvertised", true);
    assert_eq!(ran.ordered[..2], [None, Some("force")]);
    assert_eq!(ran.steps, Vec::<Step>::new());
    assert_eq!(ran.forced, None);
    assert_eq!(
        ran.state, "in_progress",
        "the force stands, its host not on 2.0.0"
    );
}
