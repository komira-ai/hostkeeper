//! `Rollouts.Advance`, `Rollouts.Pause` and `Rollouts.Resume` end to end on a real coordinator,
//! through the router as the private listener serves it, once over gRPC and once over the JSON
//! gateway: the operator's token moves a plan and is named in the audit entry, each refusal is
//! the status of its value and changes nothing, and CI's token and no token are refused.

use std::sync::Arc;

use bytes::Bytes;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use http_body_util::Full;
use hyper::{Request, StatusCode};
use prost::Message;
use prost_reflect::{DynamicMessage, SerializeOptions};
use serde_json::Value;
use tonic::Code;

use super::operator::World;
use super::support::{call, decode, framed, grpc_request_raw, raw};
use crate::grpc::gateway::{STATUS_TABLE, is_method};
use crate::http::Surface;
use crate::marks_support::{NEW_DAEMON, beat_of, runs, start, who};
use crate::service::Coordinator;
use crate::testutil::{OPERATOR, Rig, ci_token};

const ADVANCE: &str = "/hostkeeper.operator.v1.Rollouts/Advance";
const PAUSE: &str = "/hostkeeper.operator.v1.Rollouts/Pause";
const RESUME: &str = "/hostkeeper.operator.v1.Rollouts/Resume";

/// One plan stage of all the hosts, which waits for an operator to enter it.
const MANUAL_FIRST_STAGE: &str = "stages = [{ percent = 100, advance = \"manual\" }]";

/// The transports of the operator API that a test drives.
#[derive(Clone, Copy, Debug)]
pub(super) enum Wire {
    Grpc,
    Gateway,
}

pub(super) const WIRES: [Wire; 2] = [Wire::Grpc, Wire::Gateway];

/// A refusal as the caller reads it: the status code and the token.
pub(super) type Refused = (Code, String);

/// The call `path` with `request` and `bearer` over `wire`, answered as the message `R` or as
/// the refusal. The gateway's request and answer are the canonical JSON of the messages, as
/// `hkctl` and the web app's client send and read them.
pub(super) async fn rpc<R: Message + Default>(
    coordinator: &Arc<Coordinator>,
    wire: Wire,
    path: &str,
    bearer: Option<&str>,
    request: &impl Message,
) -> Result<R, Refused> {
    match wire {
        Wire::Grpc => {
            let call_request = grpc_request_raw(path, framed(&request.encode_to_vec()), bearer);
            let (code, said, body) = call(coordinator, Surface::Control, &None, call_request).await;
            match body {
                Some(bytes) => Ok(decode(&bytes)),
                None => Err((code, said)),
            }
        }
        Wire::Gateway => {
            let method = is_method(path).expect("a path of the gateway");
            let message =
                DynamicMessage::decode(method.input(), request.encode_to_vec().as_slice())
                    .expect("the request");
            let mut json = serde_json::Serializer::new(Vec::new());
            message
                .serialize_with_options(&mut json, &SerializeOptions::new())
                .expect("canonical JSON");
            let mut builder = Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json");
            if let Some(token) = bearer {
                builder = builder.header("authorization", format!("Bearer {token}"));
            }
            let http = builder
                .body(Full::new(Bytes::from(json.into_inner())))
                .expect("a request");
            let (status, _, body) = raw(coordinator, Surface::Control, &None, http).await;
            if status == StatusCode::OK {
                let mut reader = serde_json::Deserializer::from_slice(&body);
                let reply = DynamicMessage::deserialize(method.output(), &mut reader)
                    .expect("canonical JSON of the response");
                Ok(decode(&reply.encode_to_vec()))
            } else {
                let error: Value = serde_json::from_slice(&body).expect("a JSON error");
                let name = error["code"].as_str().expect("a code name");
                let (code, _, _) = *STATUS_TABLE
                    .iter()
                    .find(|(_, known, _)| *known == name)
                    .expect("a code the gateway answers with");
                Err((code, error["reason"].as_str().expect("a reason").to_owned()))
            }
        }
    }
}

fn pause_request(service: &str, reason: &str) -> proto::PauseRequest {
    proto::PauseRequest {
        service: service.to_owned(),
        reason: reason.to_owned(),
        host_group: String::new(),
    }
}

fn resume_request(service: &str, reason: &str) -> proto::ResumeRequest {
    proto::ResumeRequest {
        service: service.to_owned(),
        reason: reason.to_owned(),
        host_group: String::new(),
    }
}

fn advance_request(service: &str, reason: &str) -> proto::AdvanceRequest {
    proto::AdvanceRequest {
        service: service.to_owned(),
        reason: reason.to_owned(),
        host_group: String::new(),
    }
}

async fn pause(
    coordinator: &Arc<Coordinator>,
    wire: Wire,
    service: &str,
    reason: &str,
) -> Result<proto::PauseResponse, Refused> {
    rpc(
        coordinator,
        wire,
        PAUSE,
        Some(OPERATOR),
        &pause_request(service, reason),
    )
    .await
}

async fn resume(
    coordinator: &Arc<Coordinator>,
    wire: Wire,
    service: &str,
    reason: &str,
) -> Result<proto::ResumeResponse, Refused> {
    rpc(
        coordinator,
        wire,
        RESUME,
        Some(OPERATOR),
        &resume_request(service, reason),
    )
    .await
}

async fn advance(
    coordinator: &Arc<Coordinator>,
    wire: Wire,
    service: &str,
    reason: &str,
) -> Result<proto::AdvanceResponse, Refused> {
    rpc(
        coordinator,
        wire,
        ADVANCE,
        Some(OPERATOR),
        &advance_request(service, reason),
    )
    .await
}

/// A refusal of `code` with exactly `token`.
fn refused(code: Code, token: &str) -> Refused {
    (code, token.to_owned())
}

/// The rollout entries of the audit ring, as who made them, what they were, about which service
/// and why.
fn rollout_entries(
    coordinator: &Coordinator,
) -> Vec<(String, String, Option<String>, Option<String>)> {
    coordinator
        .ops()
        .state()
        .audit
        .iter()
        .filter(|entry| entry.action.starts_with("rollout_"))
        .map(|entry| {
            (
                entry.by.clone(),
                entry.action.clone(),
                entry.service.clone(),
                entry.reason.clone(),
            )
        })
        .collect()
}

/// The bytes of the ops file and its halts file, which a refused call leaves as they were.
fn ops_files(dir: &std::path::Path) -> [Option<Vec<u8>>; 2] {
    ["desired.json.ops", "desired.json.ops.halts"].map(|name| std::fs::read(dir.join(name)).ok())
}

/// A coordinator of `web` and `db`, past its quiet start, with `web` at 2.0 and a rollout file
/// for `web` that `extra` adds to; and `d1`, a daemon running `web` 1.0, has heartbeated.
async fn world_with_one_host(test: &str, extra: &str) -> World {
    let rig = Rig::new(test);
    let coordinator = start(&rig.dir, &rig.clock, extra);
    rig.at(2_000);
    let world = World::raw(rig, coordinator);
    let body = format!(
        r#"{{"service":"web","version":"2.0","release_digest":"{}"}}"#,
        "01".repeat(32)
    );
    world
        .coordinator
        .set_desired(Some(&ci_token()), body.as_bytes())
        .await
        .expect("set");
    world
        .coordinator
        .heartbeat(
            who("d1", "daemon"),
            beat_of(NEW_DAEMON, vec![runs("web", "1.0")]),
        )
        .expect("a heartbeat");
    world
}

/// Proves: the operator's `Pause` and `Resume`, over gRPC and over the gateway, pause and
/// resume the plan of `web`. Each answers an audit entry under the token's name (`operator`)
/// for its action, `rollout_pause` and `rollout_resume`, about `web`, with the reason given;
/// the plan answered after the pause says it is paused, by whom and why, and waits as `paused`;
/// the plan answered after the resume is not paused; and the audit ring holds the two entries.
///
/// Catches: a pause that is not kept, one that names no one, an entry under another name or
/// action, and a resume that leaves the plan paused.
#[tokio::test]
async fn pause_then_resume_move_the_plan_and_are_audited_under_the_token_name() {
    for wire in WIRES {
        let world = world_with_one_host(&format!("rollouts-pause-{wire:?}"), "").await;
        let coordinator = &world.coordinator;

        let paused = pause(coordinator, wire, "web", "watching the first host")
            .await
            .expect("paused");
        let entry = paused.entry.expect("an entry");
        assert_eq!(
            (entry.action.as_str(), entry.service.as_deref()),
            ("rollout_pause", Some("web")),
            "{wire:?}"
        );
        assert_eq!(entry.by_name.as_deref(), Some("operator"), "{wire:?}");
        assert_eq!(entry.reason.as_deref(), Some("watching the first host"));
        let plan = paused.plan.expect("a plan");
        assert!(plan.paused, "{wire:?}");
        assert_eq!(plan.paused_by.as_deref(), Some("operator"), "{wire:?}");
        assert_eq!(
            plan.pause_reason.as_deref(),
            Some("watching the first host"),
            "{wire:?}"
        );
        assert_eq!(plan.waiting(), proto::PlanWait::Paused, "{wire:?}");
        assert_eq!(plan.version, "2.0", "{wire:?}");

        let resumed = resume(coordinator, wire, "web", "the first host is fine")
            .await
            .expect("resumed");
        let entry = resumed.entry.expect("an entry");
        assert_eq!(
            (entry.action.as_str(), entry.service.as_deref()),
            ("rollout_resume", Some("web")),
            "{wire:?}"
        );
        assert_eq!(entry.by_name.as_deref(), Some("operator"), "{wire:?}");
        assert_eq!(entry.reason.as_deref(), Some("the first host is fine"));
        let plan = resumed.plan.expect("a plan");
        assert!(!plan.paused, "{wire:?}");
        assert_eq!(plan.paused_by, None, "{wire:?}");
        assert_eq!(plan.pause_reason, None, "{wire:?}");
        assert_ne!(plan.waiting(), proto::PlanWait::Paused, "{wire:?}");

        assert_eq!(
            rollout_entries(coordinator),
            [
                (
                    "operator".to_owned(),
                    "rollout_pause".to_owned(),
                    Some("web".to_owned()),
                    Some("watching the first host".to_owned())
                ),
                (
                    "operator".to_owned(),
                    "rollout_resume".to_owned(),
                    Some("web".to_owned()),
                    Some("the first host is fine".to_owned())
                ),
            ],
            "{wire:?}"
        );
    }
}

/// Proves: a second `Pause` of a paused plan is `already_paused` and a `Resume` of a plan that
/// is not paused is `not_paused`, both `FAILED_PRECONDITION`; an `Advance` of a plan that does
/// not wait for an operator is `not_waiting`, also `FAILED_PRECONDITION`; over gRPC and over the
/// gateway. A refused call writes nothing: the ops file and its halts file keep their bytes and
/// the audit ring gains no entry.
///
/// Catches: a repeated pause that replaces the first operator's name and reason, a resume that
/// succeeds on a plan nobody paused, an advance that enters a stage no one waits to enter, and
/// a refusal that still writes.
#[tokio::test]
async fn a_second_pause_an_unpaused_resume_and_an_advance_not_waiting_are_refused() {
    for wire in WIRES {
        let world = world_with_one_host(&format!("rollouts-state-{wire:?}"), "").await;
        let coordinator = &world.coordinator;
        let dir = world.rig.dir.path();

        let before = (ops_files(dir), rollout_entries(coordinator));
        assert_eq!(
            resume(coordinator, wire, "web", "nothing to resume")
                .await
                .map(|_| ()),
            Err(refused(Code::FailedPrecondition, "not_paused")),
            "{wire:?}"
        );
        assert_eq!(
            advance(coordinator, wire, "web", "nothing waits")
                .await
                .map(|_| ()),
            Err(refused(Code::FailedPrecondition, "not_waiting")),
            "{wire:?}"
        );
        assert_eq!(before, (ops_files(dir), rollout_entries(coordinator)));

        pause(coordinator, wire, "web", "first pause")
            .await
            .expect("paused");
        let before = (ops_files(dir), rollout_entries(coordinator));
        assert_eq!(
            pause(coordinator, wire, "web", "second pause")
                .await
                .map(|_| ()),
            Err(refused(Code::FailedPrecondition, "already_paused")),
            "{wire:?}"
        );
        assert_eq!(
            advance(coordinator, wire, "web", "nothing waits")
                .await
                .map(|_| ()),
            Err(refused(Code::FailedPrecondition, "not_waiting")),
            "{wire:?}"
        );
        assert_eq!(before, (ops_files(dir), rollout_entries(coordinator)));
        let kept = rollout_entries(coordinator);
        assert_eq!(kept.len(), 1, "{wire:?}");
        assert_eq!(kept[0].3.as_deref(), Some("first pause"), "{wire:?}");
    }
}

/// Proves: all three calls refuse a service the coordinator does not manage as `NOT_FOUND`
/// `unknown_service`, and a missing reason (empty) or one over 256 bytes as `INVALID_ARGUMENT`
/// `invalid_request`; `Advance` of a service with no desired version is `FAILED_PRECONDITION`
/// `no_desired`; over gRPC and over the gateway. None of them writes.
///
/// Catches: a call that creates a plan position for a service that is not managed, a pause with
/// no stated reason, and an advance of a service nothing is set for.
#[tokio::test]
async fn an_unknown_service_a_missing_reason_and_no_desired_version_are_refused() {
    for wire in WIRES {
        let world = world_with_one_host(&format!("rollouts-invalid-{wire:?}"), "").await;
        let coordinator = &world.coordinator;
        let dir = world.rig.dir.path();
        let before = (ops_files(dir), rollout_entries(coordinator));
        let long = "x".repeat(257);

        let unknown = refused(Code::NotFound, "unknown_service");
        assert_eq!(
            pause(coordinator, wire, "nope", "why").await.map(|_| ()),
            Err(unknown.clone()),
            "{wire:?}"
        );
        assert_eq!(
            resume(coordinator, wire, "nope", "why").await.map(|_| ()),
            Err(unknown.clone()),
            "{wire:?}"
        );
        assert_eq!(
            advance(coordinator, wire, "nope", "why").await.map(|_| ()),
            Err(unknown),
            "{wire:?}"
        );

        let invalid = refused(Code::InvalidArgument, "invalid_request");
        for reason in ["", long.as_str()] {
            assert_eq!(
                pause(coordinator, wire, "web", reason).await.map(|_| ()),
                Err(invalid.clone()),
                "{wire:?} pause {}",
                reason.len()
            );
            assert_eq!(
                resume(coordinator, wire, "web", reason).await.map(|_| ()),
                Err(invalid.clone()),
                "{wire:?} resume {}",
                reason.len()
            );
            assert_eq!(
                advance(coordinator, wire, "web", reason).await.map(|_| ()),
                Err(invalid.clone()),
                "{wire:?} advance {}",
                reason.len()
            );
        }

        // `db` has no desired version.
        assert_eq!(
            advance(coordinator, wire, "db", "go on").await.map(|_| ()),
            Err(refused(Code::FailedPrecondition, "no_desired")),
            "{wire:?}"
        );
        assert_eq!(before, (ops_files(dir), rollout_entries(coordinator)));
    }
}

/// Proves: CI's token is refused `PERMISSION_DENIED` `forbidden`, and no token
/// `UNAUTHENTICATED` `unauthorized`, by each of the three calls over gRPC and over the gateway;
/// the refused calls write nothing.
///
/// Catches: a rollout control that CI's token, which only sets versions, can reach.
#[tokio::test]
async fn rollouts_take_the_operator_token_only() {
    for wire in WIRES {
        let world = world_with_one_host(&format!("rollouts-callers-{wire:?}"), "").await;
        let coordinator = &world.coordinator;
        let dir = world.rig.dir.path();
        let before = (ops_files(dir), rollout_entries(coordinator));
        // A CI token is spent by its first check: each call carries a new one.
        for (is_ci, want) in [
            (true, refused(Code::PermissionDenied, "forbidden")),
            (false, refused(Code::Unauthenticated, "unauthorized")),
        ] {
            let bearer = || is_ci.then(ci_token);
            let paused: Result<proto::PauseResponse, Refused> = rpc(
                coordinator,
                wire,
                PAUSE,
                bearer().as_deref(),
                &pause_request("web", "why"),
            )
            .await;
            assert_eq!(paused.map(|_| ()), Err(want.clone()), "{wire:?} pause");
            let resumed: Result<proto::ResumeResponse, Refused> = rpc(
                coordinator,
                wire,
                RESUME,
                bearer().as_deref(),
                &resume_request("web", "why"),
            )
            .await;
            assert_eq!(resumed.map(|_| ()), Err(want.clone()), "{wire:?} resume");
            let advanced: Result<proto::AdvanceResponse, Refused> = rpc(
                coordinator,
                wire,
                ADVANCE,
                bearer().as_deref(),
                &advance_request("web", "why"),
            )
            .await;
            assert_eq!(advanced.map(|_| ()), Err(want), "{wire:?} advance");
        }
        assert_eq!(before, (ops_files(dir), rollout_entries(coordinator)));
    }
}

/// Proves: with a first stage that waits for an operator (`stages = [{ percent = 100, advance =
/// "manual" }]`), the plan of `web` waits as `manual_advance` with no stage entered, and the
/// operator's `Advance` over gRPC and over the gateway enters stage 1: it answers an audit entry
/// `rollout_advance` under the token's name whose reason ends with the operator's reason, and a
/// plan whose current stage is 1; the audit ring holds that entry; and a second `Advance` is
/// `not_waiting`, since the stage entered is not done.
///
/// Catches: an Advance that answers success without entering the stage, one that does not name
/// the operator, and one that can be repeated for the same stage.
#[tokio::test]
async fn advance_enters_the_manual_first_stage_and_a_second_advance_is_refused() {
    for wire in WIRES {
        let world =
            world_with_one_host(&format!("rollouts-advance-{wire:?}"), MANUAL_FIRST_STAGE).await;
        let coordinator = &world.coordinator;
        let waiting = pause(coordinator, wire, "web", "look before the first host")
            .await
            .expect("paused")
            .plan
            .expect("a plan");
        assert_eq!(waiting.current, 0, "{wire:?}");
        let resumed = resume(coordinator, wire, "web", "go")
            .await
            .expect("resumed")
            .plan
            .expect("a plan");
        assert_eq!(
            (resumed.current, resumed.waiting()),
            (0, proto::PlanWait::ManualAdvance),
            "{wire:?}"
        );

        let advanced = advance(coordinator, wire, "web", "the canary is approved")
            .await
            .expect("advanced");
        let entry = advanced.entry.expect("an entry");
        assert_eq!(
            (entry.action.as_str(), entry.service.as_deref()),
            ("rollout_advance", Some("web")),
            "{wire:?}"
        );
        assert_eq!(entry.by_name.as_deref(), Some("operator"), "{wire:?}");
        assert!(
            entry
                .reason
                .as_deref()
                .is_some_and(|reason| reason.ends_with("the canary is approved")),
            "{wire:?}: {:?}",
            entry.reason
        );
        let plan = advanced.plan.expect("a plan");
        assert_eq!((plan.current, plan.eligible), (1, 1), "{wire:?}");
        assert_ne!(plan.waiting(), proto::PlanWait::ManualAdvance, "{wire:?}");
        assert!(
            rollout_entries(coordinator)
                .iter()
                .any(
                    |(by, action, service, _)| (by.as_str(), action.as_str(), service.as_deref())
                        == ("operator", "rollout_advance", Some("web"))
                ),
            "{wire:?}"
        );

        assert_eq!(
            advance(coordinator, wire, "web", "again").await.map(|_| ()),
            Err(refused(Code::FailedPrecondition, "not_waiting")),
            "{wire:?}"
        );
    }
}
