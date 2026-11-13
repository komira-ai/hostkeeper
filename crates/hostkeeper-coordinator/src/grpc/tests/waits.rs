//! `Releases.WaitConverged` through the router: each wait is ended by an explicit change made
//! through the router (a heartbeat, an operator's set), by the paused clock (its deadline), or
//! not held at all (a refusal), never by elapsed time. A test makes its change once the wait has
//! read and gone on waiting: the coordinator tells it so ([`crate::wait::Waits::tell_reads`]).
//! Every test runs on the paused clock, which moves only when every task waits: a wait that a
//! change failed to end is answered `DEADLINE` at once, and the test fails on that outcome.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use hostkeeper_oidc::testkit::{NOW, STATIC_ISSUER, good_claims, keys, mint, static_verifier};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::HEARTBEAT_PATH;
use http_body_util::Full;
use hyper::{Request, StatusCode};
use serde_json::{Value, json};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::task::JoinHandle;
use tonic::Code;

use super::support::{
    CONVERGE, SET_DESIRED, Stalled, WAIT_CONVERGED, call, call_in, decode, grpc_request, grpc_with,
    raw, updater,
};
use crate::converge::State;
use crate::desired::DesiredStore;
use crate::grpc::Admitted;
use crate::grpc::operator::ReleasesService;
use crate::http::Surface;
use crate::listen::CLIENT_PATIENCE;
use crate::service::Coordinator;
use crate::testutil::{LEGACY, OPERATOR, Rig, clock_of, kbf_prefixes, kbf_services, ops_of};
use crate::wait::MAX_WAITS;
use hostkeeper_proto::hostkeeper::operator::v1::releases_server::Releases as _;

/// The desired state CI sets before each wait: kbf at 2.0, no role kept up.
const DESIRED: &str = r#"{"version":"2.0","min_up":{"storage":0,"frontend":0}}"#;

/// A healthy kbf host at `version`.
fn at(version: &str) -> String {
    format!(r#"{{"platform":"linux-x86_64","version":"{version}","health":"healthy"}}"#)
}

/// A kbf host at 1.0 whose update to 2.0 failed: it blocks the service.
const FAILED: &str = r#"{"platform":"linux-x86_64","version":"1.0","health":"healthy","last_update":{"target":"2.0","outcome":"failed"}}"#;

/// A kbf host whose hook halted the update to 2.0: it halts the service.
const HALTED: &str = r#"{"platform":"linux-x86_64","version":"1.0","health":"healthy","last_update":{"target":"2.0","outcome":"failed","phase":"halt","reason":"disk_tainted"}}"#;

/// A coordinator past its quiet start, whose clock and whose CI issuer's clock the test sets.
struct World {
    coordinator: Arc<Coordinator>,
    rig: Rig,
    issuer: Arc<AtomicU64>,
}

/// The recorded kbf coordinator over a rig's directory and clock, with kbf at 2.0 desired.
async fn world(name: &str) -> World {
    let rig = Rig::new(name);
    let store =
        DesiredStore::open(&rig.dir.path().join("desired.json"), Some(LEGACY)).expect("a store");
    let (verifier, issuer) = static_verifier();
    let coordinator = Coordinator::new(store, verifier, OPERATOR.to_owned(), clock_of(&rig.clock))
        .with_allow_list(
            &crate::testutil::allow_list(&hostkeeper_oidc::testkit::config_text(
                hostkeeper_oidc::testkit::STATIC_ISSUER,
            )),
            Some(LEGACY),
        )
        .with_services(kbf_services())
        .with_legacy_service(Some(LEGACY.to_owned()))
        .with_prefixes(kbf_prefixes())
        .with_ops(ops_of(&rig.dir));
    rig.at(1_100);
    crate::testutil::set_native(&coordinator, DESIRED).await;
    World {
        coordinator: Arc::new(coordinator),
        rig,
        issuer,
    }
}

/// The states the waits of `world` read and went on from, in order.
fn reads(world: &World) -> UnboundedReceiver<State> {
    let (tell, reads) = unbounded_channel();
    world.coordinator.waits.tell_reads(tell);
    reads
}

/// A heartbeat of the host `host` of role `role` with the JSON `body`, through the router.
///
/// The daemon announces `target-v1` and lists `apt`, so that the release set with its entry picks
/// it.
async fn beat(world: &World, host: &str, role: &str, body: &str) {
    let mut fields: Value = serde_json::from_str(body).expect("a heartbeat");
    fields["agent_features"] = json!([crate::picks::TARGET_FEATURE]);
    world
        .coordinator
        .hear_apt(&crate::marks_support::who(host, role));
    let body = fields.to_string();
    let request = Request::builder()
        .method("POST")
        .uri(HEARTBEAT_PATH)
        .body(Full::new(Bytes::from(body)))
        .expect("a request");
    let (status, _, answer) = raw(
        &world.coordinator,
        Surface::Heartbeat,
        &updater(host, role),
        request,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answer:?}");
}

/// The request of a wait for `version` of `service`, for `wait_secs`.
fn wait_for(service: &str, version: &str, wait_secs: u32) -> proto::WaitConvergedRequest {
    proto::WaitConvergedRequest {
        service: service.to_owned(),
        version: version.to_owned(),
        wait_secs,
        host_group: String::new(),
    }
}

/// A gRPC wait for 2.0 of kbf for `wait_secs` with `bearer`, answered on a task of its own
/// through the router with `patience`.
fn held(
    world: &World,
    wait_secs: u32,
    bearer: &str,
    patience: Duration,
) -> JoinHandle<(Code, String, Option<Bytes>)> {
    let request = grpc_request(
        WAIT_CONVERGED,
        wait_for("kbf", "2.0", wait_secs),
        Some(bearer),
    );
    let coordinator = Arc::clone(&world.coordinator);
    tokio::spawn(
        async move { call_in(&coordinator, Surface::Control, &None, request, patience).await },
    )
}

/// How a gRPC wait was answered: its outcome and its convergence's state.
fn answered(
    answer: (Code, String, Option<Bytes>),
) -> (proto::WaitOutcome, proto::ConvergenceState) {
    let (code, message, body) = answer;
    assert_eq!((code, message.as_str()), (Code::Ok, ""));
    let reply: proto::WaitConvergedResponse = decode(&body.expect("a message"));
    let outcome = reply.outcome();
    let convergence = reply.convergence.expect("a convergence");
    (outcome, convergence.state())
}

/// A wait for 2.0 of kbf for 1800 seconds as the operator, once it has read `state` and gone on.
async fn waiting(
    world: &World,
    reads: &mut UnboundedReceiver<State>,
    state: State,
) -> JoinHandle<(Code, String, Option<Bytes>)> {
    let wait = held(world, 1800, OPERATOR, CLIENT_PATIENCE);
    assert_eq!(reads.recv().await, Some(state));
    wait
}

/// The state the next read of a wait found, once it went on waiting; a failure when the wait
/// `wait` was answered instead (a change that did not reach it leaves it to its deadline).
async fn next_read(
    reads: &mut UnboundedReceiver<State>,
    wait: &mut JoinHandle<(Code, String, Option<Bytes>)>,
) -> State {
    tokio::select! {
        read = reads.recv() => read.expect("a read"),
        answer = wait => panic!("answered instead of reading again: {answer:?}"),
    }
}

/// The answer of the wait `wait`, given while the paused clock stands: a wait the change before
/// did not end would hold on to its deadline, and the clock moves on to a second first.
async fn at_once<T: std::fmt::Debug>(wait: JoinHandle<T>) -> T {
    tokio::select! {
        answer = wait => answer.expect("joined"),
        () = tokio::time::sleep(Duration::from_secs(1)) => panic!("not answered at the change"),
    }
}

/// The number of audit entries the coordinator holds.
fn audit_lines(world: &World) -> usize {
    world.coordinator.ops().state().audit.len()
}

/// Proves: a wait whose first read ends it is answered at once, with the convergence
/// `Converge` answers then, record included: `CONVERGED` when every host runs the version,
/// `SUPERSEDED` when the version asked for is not the desired one. Neither writes an audit
/// line.
///
/// Catches: a wait that always waits for a change, an answer whose convergence is not the one
/// `Converge` reads, and a wait that audits.
#[tokio::test(start_paused = true)]
async fn a_wait_that_is_over_already_is_answered_at_once_with_the_convergence() {
    let world = world("wait-at-once").await;
    beat(&world, "s1", "storage", &at("2.0")).await;
    let lines = audit_lines(&world);
    let request = grpc_request(WAIT_CONVERGED, wait_for("kbf", "2.0", 1800), Some(OPERATOR));
    let (code, _, body) = call(&world.coordinator, Surface::Control, &None, request).await;
    assert_eq!(code, Code::Ok);
    let reply: proto::WaitConvergedResponse = decode(&body.expect("a message"));
    assert_eq!(reply.outcome(), proto::WaitOutcome::Converged);
    let converge = grpc_request(CONVERGE, proto::ConvergeRequest::default(), Some(OPERATOR));
    let (_, _, read) = call(&world.coordinator, Surface::Control, &None, converge).await;
    let read: proto::ConvergeResponse = decode(&read.expect("a message"));
    assert_eq!(reply.convergence, read.convergence);
    assert_eq!(
        reply.convergence.expect("a convergence").desired.as_deref(),
        Some("2.0")
    );

    let request = grpc_request(WAIT_CONVERGED, wait_for("kbf", "1.9", 1800), Some(OPERATOR));
    let answer = call(&world.coordinator, Surface::Control, &None, request).await;
    assert_eq!(
        answered(answer),
        (
            proto::WaitOutcome::Superseded,
            proto::ConvergenceState::Converged
        )
    );
    assert_eq!(audit_lines(&world), lines);
}

/// Proves: a wait held while a host is behind ends `CONVERGED` at the heartbeat that brings the
/// host to the version, and no sooner.
///
/// Catches: a wait that does not read again after a heartbeat, or that ends while the service is
/// still in progress.
#[tokio::test(start_paused = true)]
async fn a_wait_ends_converged_at_the_heartbeat_that_converges_the_service() {
    let world = world("wait-converged").await;
    beat(&world, "s1", "storage", &at("1.0")).await;
    let mut reads = reads(&world);
    let mut wait = waiting(&world, &mut reads, State::InProgress).await;
    beat(&world, "s1", "storage", &at("1.0")).await;
    assert_eq!(next_read(&mut reads, &mut wait).await, State::InProgress);
    beat(&world, "s1", "storage", &at("2.0")).await;
    assert_eq!(
        answered(at_once(wait).await),
        (
            proto::WaitOutcome::Converged,
            proto::ConvergenceState::Converged
        )
    );
}

/// Proves: a wait ends `HALTED` at the heartbeat of a host whose hook halted the update.
///
/// Catches: a halted service that keeps the wait for its whole budget.
#[tokio::test(start_paused = true)]
async fn a_wait_ends_halted_at_the_heartbeat_that_halts_the_service() {
    let world = world("wait-halted").await;
    beat(&world, "s1", "storage", &at("1.0")).await;
    let mut reads = reads(&world);
    let wait = waiting(&world, &mut reads, State::InProgress).await;
    beat(&world, "s1", "storage", HALTED).await;
    assert_eq!(
        answered(at_once(wait).await),
        (proto::WaitOutcome::Halted, proto::ConvergenceState::Halted)
    );
}

/// Proves: a wait ends `SUPERSEDED` when an operator sets another version through the router,
/// with the convergence of the version now desired.
///
/// Catches: a wait that goes on for a version no longer desired, or that reports the newer
/// version's convergence as the caller's.
#[tokio::test(start_paused = true)]
async fn a_wait_ends_superseded_when_another_version_is_set() {
    let world = world("wait-superseded").await;
    beat(&world, "s1", "storage", &at("1.0")).await;
    let mut reads = reads(&world);
    let wait = waiting(&world, &mut reads, State::InProgress).await;
    let set = proto::SetDesiredRequest {
        service: "kbf".to_owned(),
        desired: Some(proto::DesiredState {
            version: "3.0".to_owned(),
            ..proto::DesiredState::default()
        }),
        ..proto::SetDesiredRequest::default()
    };
    let request = grpc_request(SET_DESIRED, set, Some(OPERATOR));
    let (code, _, _) = call(&world.coordinator, Surface::Control, &None, request).await;
    assert_eq!(code, Code::Ok);
    let (code, _, body) = at_once(wait).await;
    assert_eq!(code, Code::Ok);
    let reply: proto::WaitConvergedResponse = decode(&body.expect("a message"));
    assert_eq!(reply.outcome(), proto::WaitOutcome::Superseded);
    assert_eq!(
        reply.convergence.expect("a convergence").desired.as_deref(),
        Some("3.0")
    );
}

/// Proves: a blocked service keeps the wait (a host failed its update), and the wait ends
/// `CONVERGED` once the host reports the version.
///
/// Catches: a wait that gives up at `BLOCKED`, which the `hostkeeper ci` of older releases waits
/// through.
#[tokio::test(start_paused = true)]
async fn a_blocked_service_keeps_the_wait_until_it_converges() {
    let world = world("wait-blocked").await;
    beat(&world, "s1", "storage", &at("1.0")).await;
    let mut reads = reads(&world);
    let mut wait = waiting(&world, &mut reads, State::InProgress).await;
    beat(&world, "s1", "storage", FAILED).await;
    assert_eq!(next_read(&mut reads, &mut wait).await, State::Blocked);
    beat(&world, "s1", "storage", &at("2.0")).await;
    assert_eq!(
        answered(at_once(wait).await),
        (
            proto::WaitOutcome::Converged,
            proto::ConvergenceState::Converged
        )
    );
}

/// Proves: on the paused clock a wait of 60 seconds is still held a second before its deadline
/// and answered `DEADLINE` at it, with the convergence it reads then.
///
/// Catches: a wait that ends early, one that never ends, and one cut by the call's patience
/// rather than answered.
#[tokio::test(start_paused = true)]
async fn a_wait_ends_deadline_at_its_wait_secs_on_the_paused_clock() {
    let world = world("wait-deadline").await;
    beat(&world, "s1", "storage", &at("1.0")).await;
    let mut wait = held(&world, 60, OPERATOR, CLIENT_PATIENCE);
    tokio::select! {
        _ = &mut wait => panic!("answered before its deadline"),
        () = tokio::time::sleep(Duration::from_secs(59)) => {}
    }
    assert_eq!(
        answered(wait.await.expect("joined")),
        (
            proto::WaitOutcome::Deadline,
            proto::ConvergenceState::InProgress
        )
    );
}

/// Proves: at its deadline a wait reads once more and answers what that read says: a host that
/// fell silent (down) while the wait was held leaves the one live host at the version, which no
/// request told the wait, and the answer is `CONVERGED`, not `DEADLINE`.
///
/// Catches: an answer at the deadline that contradicts the convergence it carries.
#[tokio::test(start_paused = true)]
async fn a_wait_reads_again_at_its_deadline() {
    let world = world("wait-deadline-read").await;
    beat(&world, "f1", "frontend", &at("1.0")).await;
    world.rig.at(1_140);
    beat(&world, "s1", "storage", &at("2.0")).await;
    let mut reads = reads(&world);
    let wait = held(&world, 60, OPERATOR, CLIENT_PATIENCE);
    assert_eq!(reads.recv().await, Some(State::InProgress));
    // f1 was last heard at 1100: by 1150 it is down. s1, heard at 1140, is live.
    world.rig.at(1_150);
    assert_eq!(
        answered(wait.await.expect("joined")),
        (
            proto::WaitOutcome::Converged,
            proto::ConvergenceState::Converged
        )
    );
}

/// Proves: a gRPC wait is held past the call's patience (three patiences, on the paused clock)
/// and answered when a heartbeat converges the service.
///
/// Catches: a held call cut with `DEADLINE_EXCEEDED` at one patience, the bound of every other
/// call.
#[tokio::test(start_paused = true)]
async fn a_grpc_wait_is_held_past_one_patience_and_answered() {
    let world = world("wait-grpc-patience").await;
    beat(&world, "s1", "storage", &at("1.0")).await;
    let mut reads = reads(&world);
    let mut wait = held(&world, 1800, OPERATOR, CLIENT_PATIENCE);
    assert_eq!(reads.recv().await, Some(State::InProgress));
    tokio::select! {
        _ = &mut wait => panic!("cut or answered within three patiences"),
        () = tokio::time::sleep(CLIENT_PATIENCE * 3) => {}
    }
    beat(&world, "s1", "storage", &at("2.0")).await;
    assert_eq!(
        answered(at_once(wait).await),
        (
            proto::WaitOutcome::Converged,
            proto::ConvergenceState::Converged
        )
    );
}

/// Proves: a gateway wait (JSON over HTTP/1.1) is held past the patience (three of them, on the
/// paused clock) and answered as JSON when a heartbeat converges the service.
///
/// Catches: a gateway call cut by a deadline of the gateway's own.
#[tokio::test(start_paused = true)]
async fn a_gateway_wait_is_held_past_one_patience_and_answered() {
    let world = world("wait-gateway-patience").await;
    beat(&world, "s1", "storage", &at("1.0")).await;
    let mut reads = reads(&world);
    let request = Request::builder()
        .method("POST")
        .uri(WAIT_CONVERGED)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {OPERATOR}"))
        .body(Full::new(Bytes::from(
            r#"{"service":"kbf","version":"2.0","waitSecs":1800}"#,
        )))
        .expect("a request");
    let coordinator = Arc::clone(&world.coordinator);
    let mut wait =
        tokio::spawn(async move { raw(&coordinator, Surface::Control, &None, request).await });
    assert_eq!(reads.recv().await, Some(State::InProgress));
    tokio::select! {
        _ = &mut wait => panic!("cut or answered within three patiences"),
        () = tokio::time::sleep(CLIENT_PATIENCE * 3) => {}
    }
    beat(&world, "s1", "storage", &at("2.0")).await;
    let (status, _, body) = at_once(wait).await;
    assert_eq!(status, StatusCode::OK);
    let reply: Value = serde_json::from_slice(&body).expect("JSON");
    assert_eq!(reply["outcome"], "WAIT_OUTCOME_CONVERGED");
    assert_eq!(reply["convergence"]["state"], "CONVERGENCE_STATE_CONVERGED");
}

/// Proves: a wait whose request never arrives whole is cut with `DEADLINE_EXCEEDED` at one
/// patience: the bound moves only once the call is held, after its request is read.
///
/// Catches: a held call's longer bound given to a request that has not arrived, which would hold
/// a stream for 30 minutes on a few bytes.
#[tokio::test(start_paused = true)]
async fn a_wait_whose_request_never_arrives_is_cut_at_one_patience() {
    let world = world("wait-stalled").await;
    let request = grpc_with(
        WAIT_CONVERGED,
        // A message that says it has ten bytes, and two of them.
        Stalled::after(Bytes::from_static(&[0, 0, 0, 0, 10, 0x0a, 3])),
        Some(OPERATOR),
    );
    let coordinator = Arc::clone(&world.coordinator);
    let mut wait = tokio::spawn(async move {
        call_in(
            &coordinator,
            Surface::Control,
            &None,
            request,
            CLIENT_PATIENCE,
        )
        .await
    });
    tokio::select! {
        _ = &mut wait => panic!("cut before the patience"),
        () = tokio::time::sleep(CLIENT_PATIENCE - Duration::from_millis(1)) => {}
    }
    let (code, message, _) = wait.await.expect("joined");
    assert_eq!((code, message.as_str()), (Code::DeadlineExceeded, ""));
}

/// Proves: `wait_secs` 0 and 1801 are `invalid_wait` (`INVALID_ARGUMENT`), 1 and 1800 are held;
/// a service the coordinator does not manage is `unknown_service` (`NOT_FOUND`); no version is
/// `invalid_request`; no service is the legacy service's wait, as for `Converge`.
///
/// Catches: a bound off by one at either end, a wait for a service that can never converge, and
/// a wait for no version, which every desired version would end as `SUPERSEDED`.
#[tokio::test(start_paused = true)]
async fn a_wait_outside_its_bounds_or_for_an_unknown_service_is_refused_by_name() {
    let world = world("wait-refusals").await;
    let refusals = [
        (
            wait_for("kbf", "2.0", 0),
            Code::InvalidArgument,
            "invalid_wait",
        ),
        (
            wait_for("kbf", "2.0", 1801),
            Code::InvalidArgument,
            "invalid_wait",
        ),
        (
            wait_for("kbf", "2.0", u32::MAX),
            Code::InvalidArgument,
            "invalid_wait",
        ),
        (
            wait_for("nope", "2.0", 60),
            Code::NotFound,
            "unknown_service",
        ),
        (
            wait_for("kbf", "", 60),
            Code::InvalidArgument,
            "invalid_request",
        ),
    ];
    for (request, code, token) in refusals {
        let asked = format!("{request:?}");
        let request = grpc_request(WAIT_CONVERGED, request, Some(OPERATOR));
        let answer = call(&world.coordinator, Surface::Control, &None, request).await;
        assert_eq!(answer, (code, token.to_owned(), None), "{asked}");
    }
    for request in [
        wait_for("kbf", "1.9", 1),
        wait_for("kbf", "1.9", 1800),
        wait_for("", "1.9", 60),
    ] {
        let asked = format!("{request:?}");
        let request = grpc_request(WAIT_CONVERGED, request, Some(OPERATOR));
        let answer = call(&world.coordinator, Surface::Control, &None, request).await;
        assert_eq!(
            answered(answer),
            (
                proto::WaitOutcome::Superseded,
                proto::ConvergenceState::InProgress
            ),
            "{asked}"
        );
    }
}

/// Proves: with 16 waits held, the 17th is `too_many_waits` (`FAILED_PRECONDITION`), and once
/// the 16 end (a heartbeat converges the service) a wait is held again.
///
/// Catches: no cap on held calls, a cap off by one, and a slot never given back.
#[tokio::test(start_paused = true)]
async fn the_17th_wait_held_at_once_is_too_many_waits() {
    let world = world("wait-cap").await;
    beat(&world, "s1", "storage", &at("1.0")).await;
    let mut reads = reads(&world);
    let mut calls = Vec::new();
    for _ in 0..MAX_WAITS {
        calls.push(waiting(&world, &mut reads, State::InProgress).await);
    }
    assert_eq!(MAX_WAITS, 16);
    let request = grpc_request(WAIT_CONVERGED, wait_for("kbf", "2.0", 1800), Some(OPERATOR));
    let answer = call(&world.coordinator, Surface::Control, &None, request).await;
    assert_eq!(
        answer,
        (Code::FailedPrecondition, "too_many_waits".to_owned(), None)
    );
    beat(&world, "s1", "storage", &at("2.0")).await;
    for wait in calls {
        assert_eq!(
            answered(at_once(wait).await),
            (
                proto::WaitOutcome::Converged,
                proto::ConvergenceState::Converged
            )
        );
    }
    let request = grpc_request(WAIT_CONVERGED, wait_for("kbf", "2.0", 1800), Some(OPERATOR));
    let answer = call(&world.coordinator, Surface::Control, &None, request).await;
    assert_eq!(
        answered(answer),
        (
            proto::WaitOutcome::Converged,
            proto::ConvergenceState::Converged
        )
    );
}

/// Proves: CI's token is admitted as for `Converge`, and a held wait whose token's `exp` passes
/// while it waits is still answered: once the issuer's clock is past it, a token with the same
/// claims is refused `unauthorized`, and the held call ends `CONVERGED` at the next heartbeat.
///
/// Catches: a wait that takes only the operator's token, and one that checks its token again
/// while it is held, which would cut every CI wait longer than its token's life.
#[tokio::test(start_paused = true)]
async fn a_ci_token_is_admitted_and_a_wait_outlives_its_exp() {
    let world = world("wait-ci").await;
    beat(&world, "s1", "storage", &at("1.0")).await;
    let mut reads = reads(&world);
    let claims = good_claims(STATIC_ISSUER);
    let exp = claims["exp"].as_u64().expect("an exp");
    assert_eq!(exp, NOW + 300);
    let wait = held(&world, 1800, &mint(&keys()[0], &claims), CLIENT_PATIENCE);
    assert_eq!(reads.recv().await, Some(State::InProgress));
    world.issuer.store(exp + 600, Ordering::SeqCst);
    let mut late = good_claims(STATIC_ISSUER);
    late["exp"] = json!(exp);
    let request = grpc_request(
        WAIT_CONVERGED,
        wait_for("kbf", "2.0", 1800),
        Some(&mint(&keys()[0], &late)),
    );
    let answer = call(&world.coordinator, Surface::Control, &None, request).await;
    assert_eq!(
        answer,
        (Code::Unauthenticated, "unauthorized".to_owned(), None)
    );
    beat(&world, "s1", "storage", &at("2.0")).await;
    assert_eq!(
        answered(at_once(wait).await),
        (
            proto::WaitOutcome::Converged,
            proto::ConvergenceState::Converged
        )
    );
}

/// Proves: the handler itself refuses a call the router admitted as neither CI nor an operator
/// (none at all, or a peer): `unauthorized`, before anything is held.
///
/// Catches: a wait that trusts the path it came by rather than the caller the router admitted.
#[tokio::test(start_paused = true)]
async fn a_wait_admitted_as_neither_ci_nor_an_operator_is_unauthorized() {
    let world = world("wait-unadmitted").await;
    let service = ReleasesService::new(Arc::clone(&world.coordinator));
    let mut peer = tonic::Request::new(wait_for("kbf", "2.0", 60));
    peer.extensions_mut().insert(Admitted::Peer(None));
    for request in [tonic::Request::new(wait_for("kbf", "2.0", 60)), peer] {
        let status = service.wait_converged(request).await.expect_err("refused");
        assert_eq!(
            (status.code(), status.message()),
            (Code::Unauthenticated, "unauthorized")
        );
    }
}
