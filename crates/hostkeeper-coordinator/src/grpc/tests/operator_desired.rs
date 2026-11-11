//! `Releases.SetDesired` and `Releases.Converge` with an operator's token: the operator sets the
//! desired state of any service the coordinator manages, the set is one audit entry under the
//! token's name, and the refusals, the end of a force and the lift of a halt are CI's, audited
//! under the operator's name.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use bytes::Bytes;
use hostkeeper_oidc::testkit::{STATIC_ISSUER, config_text, static_verifier};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::audit_server::Audit as _;
use hostkeeper_proto::hostkeeper::operator::v1::releases_server::Releases as _;
use http_body_util::Full;
use hyper::StatusCode;
use serde_json::{Value, json};
use tonic::{Code, Request, Response, Status};

use super::operator::{World, as_ci, as_operator, disk_full, state};
use super::support::{CONVERGE, SET_DESIRED, call, decode, grpc_request, raw, served_unreachable};
use crate::desired::DesiredStore;
use crate::force::{ForceRecord, ForceTargets};
use crate::grpc::operator::{AuditService, ReleasesService};
use crate::http::Surface;
use crate::ops::{AuditEntry, Halt, HaltOrigin, OpsState, OpsStore, Write};
use crate::ops_file::digest;
use crate::registry::Digest;
use crate::service::Coordinator;
use crate::testutil::{
    LEGACY, OPERATOR, Rig, ci_token, clock_of, kbf_prefixes, ops_of, version, with_kbf,
};
use crate::tokens::OperatorTokens;

/// Two more services: `other`, and `floor`, which refuses a version below 1.0.
const EXTRA: &str = "[[service]]\nname = \"other\"\nroles = [\"daemon\"]\n\n\
                     [[service]]\nname = \"floor\"\nroles = [\"daemon\"]\nmin_version = \"1.0\"\n";

/// The token of the operator named `alice` in [`alice_world`].
const ALICE: &str = "alice-token-0123456789abcdef0123456789";

/// A world that manages kbf, `other` and `floor`, where CI's allow-list entry names kbf only.
fn world(name: &str) -> World {
    let rig = Rig::new(name);
    let list = format!("{}services = [\"kbf\"]\n", config_text(STATIC_ISSUER));
    let started = Rig::start_with_allow_list(&rig.dir, &rig.clock, &list);
    World::over(rig, started.with_services(with_kbf(EXTRA)))
}

/// A world whose one operator token, [`ALICE`], is named `alice`.
fn alice_world(name: &str) -> World {
    let rig = Rig::new(name);
    let file = rig.dir.path().join("tokens.toml");
    let text = format!(
        "[[token]]\nname = \"alice\"\nsha256 = \"{}\"\n",
        digest(ALICE.as_bytes())
    );
    std::fs::write(&file, text).expect("a tokens file");
    std::fs::set_permissions(&file, std::os::unix::fs::PermissionsExt::from_mode(0o600))
        .expect("a mode");
    let tokens = OperatorTokens::from_file(&file).expect("tokens");
    let store =
        DesiredStore::open(&rig.dir.path().join("desired.json"), Some(LEGACY)).expect("a store");
    let coordinator = Coordinator::new(store, static_verifier().0, tokens, clock_of(&rig.clock))
        .with_services(with_kbf(EXTRA))
        .with_legacy_service(Some(LEGACY.to_owned()))
        .with_prefixes(kbf_prefixes())
        .with_ops(ops_of(&rig.dir));
    World::raw(rig, coordinator)
}

fn releases(world: &World) -> ReleasesService {
    ReleasesService::new(world.coordinator.clone())
}

/// The set of `version` for `service`, as a gRPC call carries it.
fn set_request(service: &str, version: &str) -> proto::SetDesiredRequest {
    proto::SetDesiredRequest {
        service: service.to_owned(),
        desired: Some(state(version, &[])),
        ..Default::default()
    }
}

/// Sets `version` of `service` as the operator of [`OPERATOR`].
async fn operator_sets(
    world: &World,
    service: &str,
    version: &str,
) -> Result<Response<proto::SetDesiredResponse>, Status> {
    releases(world)
        .set_desired(as_operator(&world.coordinator, set_request(service, version)).await)
        .await
}

/// Asserts that `result` is a refusal of `code` whose message is exactly `token`.
fn assert_refused<T: std::fmt::Debug>(
    result: Result<Response<T>, Status>,
    code: Code,
    token: &str,
) {
    let status = result.expect_err("the call is refused");
    assert_eq!((status.code(), status.message()), (code, token));
}

/// The audit entries of the coordinator as `(by, action, service, reason)`.
fn audited(world: &World) -> Vec<(String, String, Option<String>, Option<String>)> {
    world
        .coordinator
        .ops()
        .state()
        .audit
        .into_iter()
        .map(|e| (e.by, e.action, e.service, e.reason))
        .collect()
}

/// The files a set could change: the desired state and the ops store's.
fn files(world: &World) -> [Option<Vec<u8>>; 3] {
    ["desired.json", "desired.json.ops", "desired.json.ops.audit"].map(|name| world.file(name))
}

/// A force of `forced` of kbf that stands.
fn stand_force(world: &World, forced: &str) {
    let record = ForceRecord {
        version: version(forced),
        generation: 1,
        reason: "test".to_owned(),
        by: "operator".to_owned(),
        digest: Digest::parse(&"ab".repeat(32)).expect("a digest"),
        release_index: None,
        halt_generation: 0,
        targets: ForceTargets::Fleet,
        include_held: false,
        hosts: Vec::new(),
    };
    world
        .coordinator
        .ops()
        .change(
            Write::BeforeMemory,
            AuditEntry::new("operator", "force"),
            |s| {
                s.services.entry("kbf".to_owned()).or_default().force = Some(record);
            },
        )
        .expect("the force stands");
}

/// A halt of kbf by `origin` for the update of `d1` to 2.0 that stands.
fn stand_halt(world: &World, origin: HaltOrigin) {
    let halt = Halt {
        node_id: "d1".to_owned(),
        check: "digest".to_owned(),
        target: version("2.0"),
        report: None,
        origin,
    };
    world
        .coordinator
        .ops()
        .change(Write::Halt, AuditEntry::new("coordinator", "halt"), |s| {
            s.services.entry("kbf".to_owned()).or_default().halt = Some(halt);
        })
        .expect("the halt stands");
}

fn marks(world: &World) -> crate::ops::ServiceOps {
    world.coordinator.ops().marks("kbf")
}

/// Proves: the operator's token on `Releases/SetDesired` through the gateway writes the desired
/// file byte for byte as CI's token does for the same body, answers 200 with the stored state,
/// and makes exactly one audit entry, `set_desired` by the operator, whose reason is the
/// version; `Audit.List` shows it with the actor `ACTOR_OPERATOR`. CI's own set makes no audit
/// entry.
#[tokio::test]
async fn an_operator_set_writes_the_state_ci_would_and_one_audit_entry() {
    let body =
        r#"{"service":"kbf","desired":{"version":"2.0","minUp":{"storage":1,"frontend":1}}}"#;
    let path = "/hostkeeper.operator.v1.Releases/SetDesired";
    let send = |world: &World, bearer: String| {
        let request = hyper::Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {bearer}"))
            .body(Full::new(Bytes::from(body)))
            .expect("a request");
        let coordinator = world.coordinator.clone();
        async move { raw(&coordinator, Surface::Control, &None, request).await }
    };
    let by_ci = world("od-gateway-ci");
    let (status, _, _) = send(&by_ci, ci_token()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(audited(&by_ci).is_empty(), "CI's set is no audit entry");

    let by_operator = world("od-gateway-operator");
    let (status, _, answer) = send(&by_operator, OPERATOR.to_owned()).await;
    assert_eq!(status, StatusCode::OK);
    let answer: Value = serde_json::from_slice(&answer).expect("JSON");
    assert_eq!(answer["desired"]["version"], "2.0");
    assert_eq!(
        answer["desired"]["minUp"],
        json!({"storage": 1, "frontend": 1})
    );
    assert_eq!(by_operator.file("desired.json"), by_ci.file("desired.json"));
    assert_eq!(
        audited(&by_operator),
        [(
            "operator".to_owned(),
            "set_desired".to_owned(),
            Some("kbf".to_owned()),
            Some("2.0".to_owned())
        )]
    );

    let listed = AuditService::new(by_operator.coordinator.clone())
        .list(as_operator(&by_operator.coordinator, proto::ListAuditRequest::default()).await)
        .await
        .expect("the list")
        .into_inner();
    let at_unix = listed.entries.first().and_then(|entry| entry.at_unix);
    assert!(at_unix.is_some(), "a listed entry has the time of its line");
    assert_eq!(
        listed.entries,
        [proto::AuditEntry {
            seq: 1,
            by: i32::from(proto::Actor::Operator),
            action: "set_desired".to_owned(),
            service: Some("kbf".to_owned()),
            node_id: None,
            reason: Some("2.0".to_owned()),
            at_unix,
            by_name: Some("operator".to_owned()),
            host_group: None,
        }]
    );
}

/// Proves: the audit entry of an operator's set is by the name of the token (`alice`, not the
/// legacy `operator`), and its reason names the package digest when the call named one: `2.0
/// with package digest <digest>`; the same version set again is audited again.
#[tokio::test]
async fn an_operator_set_is_audited_under_the_token_name_and_names_the_digest() {
    let world = alice_world("od-alice");
    let alice = |message| {
        let mut request = Request::new(message);
        let coordinator = world.coordinator.clone();
        let caller = coordinator.operator(Some(ALICE)).expect("alice");
        request
            .extensions_mut()
            .insert(crate::grpc::Admitted::Operator {
                caller: crate::operator::Caller::Operator(caller),
            });
        request
    };
    let mut with_digest = set_request("kbf", "2.0");
    with_digest.package_digest = Some("ab".repeat(32));
    releases(&world)
        .set_desired(alice(with_digest))
        .await
        .expect("the set");
    releases(&world)
        .set_desired(alice(set_request("other", "3.0")))
        .await
        .expect("the set of another service");
    assert_eq!(
        audited(&world),
        [
            (
                "alice".to_owned(),
                "set_desired".to_owned(),
                Some("kbf".to_owned()),
                Some(format!("2.0 with package digest {}", "ab".repeat(32)))
            ),
            (
                "alice".to_owned(),
                "set_desired".to_owned(),
                Some("other".to_owned()),
                Some("3.0".to_owned())
            ),
        ]
    );
}

/// Proves: no allow-list stands in the operator's way: a service CI's entry does not name is
/// `forbidden` for CI and set for the operator, with the limits as sent.
#[tokio::test]
async fn an_operator_may_set_a_service_the_allow_list_does_not_name() {
    let world = world("od-no-allow-list");
    let by_ci = releases(&world)
        .set_desired(as_ci(&world.coordinator, set_request("other", "3.0")).await)
        .await;
    assert_refused(by_ci, Code::PermissionDenied, "forbidden");
    assert!(audited(&world).is_empty());

    let set = operator_sets(&world, "other", "3.0")
        .await
        .expect("the operator's set")
        .into_inner();
    assert_eq!(set.desired.expect("a state").version, "3.0");
    let status = world.coordinator.desired.get("other").expect("set");
    assert_eq!(status.version, version("3.0"));
}

/// Proves: an operator's set is refused as CI's is, in the same words, and a refusal changes no
/// file and writes no audit entry: an unknown service is `unknown_service`, a version below the
/// service's `min_version` is `below_min_version`, a version marked bad is `held_bad`, and the
/// package digest of a version set before is not changed (`invalid_request`).
#[tokio::test]
async fn an_operator_set_is_refused_as_ci_is_and_changes_nothing() {
    let world = world("od-refusals");
    operator_sets(&world, "floor", "1.5").await.expect("a set");
    let mut first = set_request("kbf", "2.0");
    first.package_digest = Some("ab".repeat(32));
    releases(&world)
        .set_desired(as_operator(&world.coordinator, first).await)
        .await
        .expect("the first set");
    releases(&world)
        .mark_bad(
            as_operator(
                &world.coordinator,
                proto::MarkBadRequest {
                    service: "kbf".to_owned(),
                    version: "3.0".to_owned(),
                    reason: "crashes".to_owned(),
                },
            )
            .await,
        )
        .await
        .expect("the mark");
    let (before_files, before_audit) = (files(&world), audited(&world));

    assert_refused(
        operator_sets(&world, "nowhere", "2.0").await,
        Code::NotFound,
        "unknown_service",
    );
    assert_refused(
        operator_sets(&world, "floor", "0.5").await,
        Code::FailedPrecondition,
        "below_min_version",
    );
    assert_refused(
        operator_sets(&world, "kbf", "3.0").await,
        Code::FailedPrecondition,
        "held_bad",
    );
    let mut changed = set_request("kbf", "2.0");
    changed.package_digest = Some("cd".repeat(32));
    assert_refused(
        releases(&world)
            .set_desired(as_operator(&world.coordinator, changed).await)
            .await,
        Code::InvalidArgument,
        "invalid_request",
    );
    assert_refused(
        releases(&world)
            .set_desired(as_operator(&world.coordinator, proto::SetDesiredRequest::default()).await)
            .await,
        Code::InvalidArgument,
        "invalid_request",
    );
    assert_eq!(files(&world), before_files);
    assert_eq!(audited(&world), before_audit);
}

/// Proves: an operator's set of another version than the force's ends the force, audited
/// `force_ended` by the operator's token name, after the `set_desired` entry; a set of the
/// force's own version leaves it standing and audits only the set.
#[tokio::test]
async fn an_operator_set_ends_a_force_of_another_version_only() {
    let world = world("od-force");
    stand_force(&world, "2.0");
    operator_sets(&world, "kbf", "2.0")
        .await
        .expect("the force's own version");
    assert!(
        marks(&world).force.is_some(),
        "the force's own version leaves it"
    );
    assert_eq!(
        audited(&world)
            .iter()
            .map(|e| e.1.as_str())
            .collect::<Vec<_>>(),
        ["force", "set_desired"]
    );

    operator_sets(&world, "kbf", "3.0")
        .await
        .expect("another version");
    assert!(marks(&world).force.is_none(), "another version ends it");
    let log = audited(&world);
    assert_eq!(
        log[2..],
        [
            (
                "operator".to_owned(),
                "set_desired".to_owned(),
                Some("kbf".to_owned()),
                Some("3.0".to_owned())
            ),
            (
                "operator".to_owned(),
                "force_ended".to_owned(),
                Some("kbf".to_owned()),
                Some("3.0 set by operator token operator; the force to 2.0 ends".to_owned())
            ),
        ]
    );
}

/// Proves: an operator's set of a version other than the target of a halt the updater raised
/// lifts it, audited `lift_halt` by the operator's token name with the host; a set of the
/// halt's own target, and any set over a halt a hook raised, leave it.
#[tokio::test]
async fn an_operator_set_lifts_an_updater_halt_for_another_version_only() {
    let world = world("od-halt");
    stand_halt(&world, HaltOrigin::Updater);
    operator_sets(&world, "kbf", "2.0")
        .await
        .expect("the halt's own target");
    assert!(
        marks(&world).halt.is_some(),
        "the halt's own target lifts nothing"
    );
    operator_sets(&world, "kbf", "3.0")
        .await
        .expect("a fix-forward version");
    let after = marks(&world);
    assert!(after.halt.is_none(), "a fix-forward version lifts it");
    assert_eq!(after.halt_generation, 1);
    let lift: AuditEntry = world
        .coordinator
        .ops()
        .state()
        .audit
        .into_iter()
        .find(|e| e.action == "lift_halt")
        .expect("the lift is audited");
    assert_eq!(lift.by, "operator");
    assert_eq!(lift.service.as_deref(), Some("kbf"));
    assert_eq!(lift.node_id.as_deref(), Some("d1"));
    assert_eq!(
        lift.reason.as_deref(),
        Some("3.0 set by operator token operator")
    );

    let hook = world_hooked("od-halt-hook");
    operator_sets(&hook, "kbf", "3.0").await.expect("a set");
    assert!(marks(&hook).halt.is_some(), "a hook's halt stays");
}

/// A world whose kbf is halted by a hook.
fn world_hooked(name: &str) -> World {
    let world = world(name);
    stand_halt(&world, HaltOrigin::Hook);
    world
}

/// Proves: when the ops store cannot be written, the operator's set is `store_failed`
/// (`INTERNAL`) and nothing is set: its audit entry is written before the desired state, so the
/// desired state is not written and no audit entry stands; sending the same set again once the
/// store writes lands the state and exactly one entry.
#[tokio::test]
async fn a_failed_audit_write_is_store_failed_and_a_resend_lands() {
    let rig = Rig::new("od-store-fails");
    let failing = Arc::new(AtomicBool::new(true));
    let switch = Arc::clone(&failing);
    let ops = OpsStore::refusing(
        OpsState::default(),
        Box::new(move |_| {
            if switch.load(Ordering::SeqCst) {
                Err(disk_full())
            } else {
                Ok(())
            }
        }),
    );
    let started = Rig::start(&rig.dir, &rig.clock).with_ops(ops);
    let world = World::raw(rig, started);

    assert_refused(
        operator_sets(&world, "kbf", "2.0").await,
        Code::Internal,
        "store_failed",
    );
    let unset = world.file("desired.json");
    assert!(
        unset.is_none_or(|text| !String::from_utf8_lossy(&text).contains("2.0")),
        "the desired state was not written"
    );
    assert!(audited(&world).is_empty(), "no entry stands");

    failing.store(false, Ordering::SeqCst);
    operator_sets(&world, "kbf", "2.0")
        .await
        .expect("the resend");
    let written = world
        .file("desired.json")
        .expect("the desired state was written");
    assert!(String::from_utf8_lossy(&written).contains("2.0"));
    assert_eq!(audited(&world).len(), 1);
}

/// Proves: `Converge` answers the operator's token and CI's through the router, each with the
/// convergence of the version the operator set, and refuses a call with no token or a token that
/// is neither as `unauthorized`; and the operator needs no issuer: with CI's keys unreachable the
/// operator's `SetDesired` and `Converge` are answered, while CI's are `keys_unavailable`.
#[tokio::test]
async fn converge_answers_the_operator_and_ci_and_the_operator_needs_no_issuer() {
    let world = world("od-converge");
    operator_sets(&world, "kbf", "2.0").await.expect("a set");
    let converge = |bearer: Option<String>| {
        let request = grpc_request(
            CONVERGE,
            proto::ConvergeRequest::default(),
            bearer.as_deref(),
        );
        let coordinator = world.coordinator.clone();
        async move { call(&coordinator, Surface::Control, &None, request).await }
    };
    for bearer in [OPERATOR.to_owned(), ci_token()] {
        let (code, message, body) = converge(Some(bearer.clone())).await;
        assert_eq!((code, message.as_str()), (Code::Ok, ""), "{bearer}");
        let reply: proto::ConvergeResponse = decode(&body.expect("a message"));
        assert_eq!(
            reply.convergence.expect("a convergence").desired,
            Some("2.0".to_owned()),
            "{bearer}"
        );
    }
    for bearer in [None, Some("not-a-token".to_owned())] {
        assert_eq!(
            converge(bearer.clone()).await,
            (Code::Unauthenticated, "unauthorized".to_owned(), None),
            "{bearer:?}"
        );
    }

    let served = served_unreachable("od-unreachable");
    let sets = |bearer: String| {
        let request = grpc_request(SET_DESIRED, set_request("kbf", "2.0"), Some(&bearer));
        let coordinator = served.coordinator.clone();
        async move { call(&coordinator, Surface::Control, &None, request).await }
    };
    let (code, message, _) = sets(OPERATOR.to_owned()).await;
    assert_eq!((code, message.as_str()), (Code::Ok, ""));
    let (code, message, _) = sets(ci_token()).await;
    assert_eq!(
        (code, message.as_str()),
        (Code::Unavailable, "keys_unavailable")
    );
    let converge_with = |bearer: String| {
        let request = grpc_request(CONVERGE, proto::ConvergeRequest::default(), Some(&bearer));
        let coordinator = served.coordinator.clone();
        async move { call(&coordinator, Surface::Control, &None, request).await }
    };
    let (code, message, _) = converge_with(ci_token()).await;
    assert_eq!(
        (code, message.as_str()),
        (Code::Unavailable, "keys_unavailable")
    );
    let (code, message, body) = converge_with(OPERATOR.to_owned()).await;
    assert_eq!((code, message.as_str()), (Code::Ok, ""));
    let reply: proto::ConvergeResponse = decode(&body.expect("a message"));
    assert_eq!(
        reply.convergence.expect("a convergence").desired,
        Some("2.0".to_owned())
    );
}

/// `POST /hostkeeper.operator.v1.Releases/Converge` with `body` and the operator's token, as the
/// Rollout page's "Check convergence" button sends it: the status and the JSON answer.
async fn operator_converges_over_the_gateway(world: &World, body: &str) -> (StatusCode, Value) {
    let request = hyper::Request::builder()
        .method("POST")
        .uri(CONVERGE)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {OPERATOR}"))
        .body(Full::new(Bytes::from(body.to_owned())))
        .expect("a request");
    let (status, _, answer) = raw(&world.coordinator, Surface::Control, &None, request).await;
    (status, serde_json::from_slice(&answer).expect("JSON"))
}

/// Proves: the gateway call the Rollout page makes, `Releases/Converge` with `{"service": ...}`
/// and the operator's token, is answered 200 with the convergence of the service the operator
/// set, and for a service the coordinator does not manage reaches the handler and is 404
/// `unknown_service`, not refused at the token.
#[tokio::test]
async fn converge_over_the_gateway_answers_the_operator_as_the_rollout_page_calls_it() {
    let world = world("od-converge-gateway");
    operator_sets(&world, "other", "3.0").await.expect("a set");
    let (status, answer) =
        operator_converges_over_the_gateway(&world, r#"{"service":"other"}"#).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(answer["convergence"]["desired"], "3.0");

    let (status, answer) =
        operator_converges_over_the_gateway(&world, r#"{"service":"nope"}"#).await;
    assert_eq!(
        (status, answer),
        (
            StatusCode::NOT_FOUND,
            json!({"code": "NOT_FOUND", "reason": "unknown_service"})
        )
    );
}

/// Proves: the operator's `Converge` changes nothing: after a set, a `Converge` over gRPC and
/// one over the gateway are answered, and the state file, the ops file, the audit file and the
/// audit log are exactly what the set left.
#[tokio::test]
async fn an_operator_converge_writes_nothing() {
    let world = world("od-converge-writes-nothing");
    operator_sets(&world, "kbf", "2.0").await.expect("a set");
    let before = (files(&world), audited(&world));
    assert_eq!(before.1.len(), 1, "the set is one audit entry");

    let request = grpc_request(
        CONVERGE,
        proto::ConvergeRequest {
            service: "kbf".to_owned(),
            host_group: String::new(),
        },
        Some(OPERATOR),
    );
    let (code, message, _) = call(&world.coordinator, Surface::Control, &None, request).await;
    assert_eq!((code, message.as_str()), (Code::Ok, ""));
    let (status, _) = operator_converges_over_the_gateway(&world, r#"{"service":"kbf"}"#).await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!((files(&world), audited(&world)), before);
}
