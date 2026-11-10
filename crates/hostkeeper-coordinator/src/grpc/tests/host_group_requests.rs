//! A request that names a host group, to a coordinator that holds none: each call that gained a
//! `host_group` field refuses it with the token that says no group has the name, over gRPC and
//! over the gateway, and changes nothing; it is never read as naming no group, which would act on
//! every host. `Audit.List` with a `host_group` filter lists no entry. A caller with no token is
//! still refused `unauthorized` first.

use std::sync::Arc;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use prost::Message;
use prost_reflect::DynamicMessage;
use tonic::Code;

use super::rollouts::{Refused, WIRES, Wire, rpc};
use super::support::served;
use crate::grpc::gateway::is_method;
use crate::service::Coordinator;
use crate::testutil::{OPERATOR, ci_token};

/// The group every request names.
const GROUP: &str = "web";

/// Calls `path` with `request` as an answer of the empty message: only the refusal is read.
async fn refusal_of(
    coordinator: &Arc<Coordinator>,
    wire: Wire,
    path: &str,
    bearer: Option<&str>,
    request: &impl Message,
) -> Option<Refused> {
    rpc::<()>(coordinator, wire, path, bearer, request)
        .await
        .err()
}

/// Every entry the audit log holds, as the operator lists it with no filter.
async fn audit(coordinator: &Arc<Coordinator>, wire: Wire) -> Vec<proto::AuditEntry> {
    let listed: proto::ListAuditResponse = rpc(
        coordinator,
        wire,
        "/hostkeeper.operator.v1.Audit/List",
        Some(OPERATOR),
        &proto::ListAuditRequest::default(),
    )
    .await
    .expect("the operator lists the audit log");
    listed.entries
}

/// Each call that gained `host_group`, with the field set to [`GROUP`], and the refusal it gets.
fn named_requests() -> Vec<(&'static str, Vec<u8>, Refused)> {
    let releases = |token: &str| (Code::FailedPrecondition, token.to_owned());
    let not_in = releases("not_in_host_group");
    let unknown = (Code::NotFound, "unknown_host_group".to_owned());
    let group = GROUP.to_owned();
    vec![
        (
            "/hostkeeper.operator.v1.Releases/SetDesired",
            proto::SetDesiredRequest {
                service: "kbf".to_owned(),
                desired: Some(proto::DesiredState {
                    version: "2.0.0".to_owned(),
                    ..proto::DesiredState::default()
                }),
                host_group: group.clone(),
                ..proto::SetDesiredRequest::default()
            }
            .encode_to_vec(),
            not_in.clone(),
        ),
        (
            "/hostkeeper.operator.v1.Releases/Converge",
            proto::ConvergeRequest {
                service: "kbf".to_owned(),
                host_group: group.clone(),
            }
            .encode_to_vec(),
            not_in.clone(),
        ),
        (
            "/hostkeeper.operator.v1.Releases/WaitConverged",
            proto::WaitConvergedRequest {
                service: "kbf".to_owned(),
                version: "2.0.0".to_owned(),
                wait_secs: 1,
                host_group: group.clone(),
            }
            .encode_to_vec(),
            not_in.clone(),
        ),
        (
            "/hostkeeper.operator.v1.Releases/GetRelease",
            proto::GetReleaseRequest {
                service: "kbf".to_owned(),
                version: "2.0.0".to_owned(),
                host_group: group.clone(),
            }
            .encode_to_vec(),
            not_in.clone(),
        ),
        (
            "/hostkeeper.operator.v1.Releases/Force",
            proto::ForceRequest {
                service: "kbf".to_owned(),
                version: "2.0.0".to_owned(),
                reason: "incident".to_owned(),
                targets: Some(proto::force_request::Targets::Fleet(proto::FleetTarget {
                    confirm_service: "kbf".to_owned(),
                })),
                host_group: group.clone(),
                ..proto::ForceRequest::default()
            }
            .encode_to_vec(),
            not_in.clone(),
        ),
        (
            "/hostkeeper.operator.v1.Rollouts/Advance",
            proto::AdvanceRequest {
                service: "kbf".to_owned(),
                reason: "go".to_owned(),
                host_group: group.clone(),
            }
            .encode_to_vec(),
            not_in.clone(),
        ),
        (
            "/hostkeeper.operator.v1.Rollouts/Pause",
            proto::PauseRequest {
                service: "kbf".to_owned(),
                reason: "stop".to_owned(),
                host_group: group.clone(),
            }
            .encode_to_vec(),
            not_in.clone(),
        ),
        (
            "/hostkeeper.operator.v1.Rollouts/Resume",
            proto::ResumeRequest {
                service: "kbf".to_owned(),
                reason: "go".to_owned(),
                host_group: group.clone(),
            }
            .encode_to_vec(),
            not_in,
        ),
        (
            "/hostkeeper.operator.v1.Enrollment/Approve",
            proto::ApproveRequest {
                request_id: "ab".repeat(32),
                key_sha256: "cd".repeat(32),
                host_id: "web-04".to_owned(),
                reason: "built".to_owned(),
                host_group: group.clone(),
                ..proto::ApproveRequest::default()
            }
            .encode_to_vec(),
            unknown.clone(),
        ),
        (
            "/hostkeeper.operator.v1.Enrollment/AddRecord",
            proto::AddRecordRequest {
                host_id: "web-04".to_owned(),
                data_macs: vec!["0c:c4:7a:00:00:02".to_owned()],
                host_group: group,
                ..proto::AddRecordRequest::default()
            }
            .encode_to_vec(),
            unknown,
        ),
    ]
}

/// The request `bytes` of the method at `path`, as the message its descriptor reads.
fn encoded(path: &str, bytes: &[u8]) -> DynamicMessage {
    let method = is_method(path).expect("a method of the operator API");
    DynamicMessage::decode(method.input(), bytes).expect("the request decodes")
}

/// Catches: a request that names a host group read as naming none by a coordinator that holds
/// no group, which would set, force, pause or advance the service on every host, or approve and
/// record a host with no group: each of `SetDesired`, `Converge`, `WaitConverged`, `GetRelease`,
/// `Force`, `Advance`, `Pause`, `Resume`, `Approve` and `AddRecord` naming `web` is refused with
/// the code and token its refusal enum gives (`not_in_host_group`, `unknown_host_group`), over
/// gRPC and the gateway, and the audit log holds no entry more afterwards.
#[tokio::test]
async fn a_request_naming_a_host_group_is_refused_and_changes_nothing() {
    let served = served("grpc-host-group-named");
    for wire in WIRES {
        let before = audit(&served.coordinator, wire).await;
        for (path, bytes, want) in named_requests() {
            let refused = refusal_of(
                &served.coordinator,
                wire,
                path,
                Some(OPERATOR),
                &encoded(path, &bytes),
            )
            .await;
            assert_eq!(refused, Some(want), "{path} over {wire:?}");
        }
        assert_eq!(audit(&served.coordinator, wire).await, before, "{wire:?}");
    }
}

/// Catches: the host-group refusal given before the caller is known, which would tell a caller
/// with no token something of the request: with no token each call is `unauthorized`, and CI's
/// token on `SetDesired` naming a group is `not_in_host_group` like the operator's, setting
/// nothing.
#[tokio::test]
async fn the_caller_is_checked_before_the_host_group() {
    let served = served("grpc-host-group-caller");
    for wire in WIRES {
        for (path, bytes, _) in named_requests() {
            let refused = refusal_of(
                &served.coordinator,
                wire,
                path,
                None,
                &encoded(path, &bytes),
            )
            .await;
            assert_eq!(
                refused,
                Some((Code::Unauthenticated, "unauthorized".to_owned())),
                "{path} over {wire:?}"
            );
        }
        let (path, bytes, want) = named_requests().swap_remove(0);
        let refused = refusal_of(
            &served.coordinator,
            wire,
            path,
            Some(&ci_token()),
            &encoded(path, &bytes),
        )
        .await;
        assert_eq!(refused, Some(want), "CI's set over {wire:?}");
    }
}

/// Catches: a `host_group` filter of `Audit.List` ignored, which would list every entry as if
/// each were about the group: the log holds entries (a refused call writes none, so a hold makes
/// one), the filter set to `web` lists none and answers `next_seq` as the request's `since_seq`,
/// and the same request with no filter lists them.
#[tokio::test]
async fn an_audit_filter_by_host_group_lists_no_entry() {
    let served = served("grpc-host-group-audit");
    let held: Result<proto::HoldResponse, Refused> = rpc(
        &served.coordinator,
        Wire::Grpc,
        "/hostkeeper.operator.v1.Services/Hold",
        Some(OPERATOR),
        &proto::HoldRequest {
            scope: Some(proto::HoldScope {
                scope: Some(proto::hold_scope::Scope::Fleet(proto::WholeFleet {})),
            }),
            service: "kbf".to_owned(),
            reason: "incident".to_owned(),
        },
    )
    .await;
    assert!(held.is_ok(), "{held:?}");
    for wire in WIRES {
        assert!(!audit(&served.coordinator, wire).await.is_empty());
        let filtered: proto::ListAuditResponse = rpc(
            &served.coordinator,
            wire,
            "/hostkeeper.operator.v1.Audit/List",
            Some(OPERATOR),
            &proto::ListAuditRequest {
                host_group: Some(GROUP.to_owned()),
                ..proto::ListAuditRequest::default()
            },
        )
        .await
        .expect("listed");
        assert_eq!(
            filtered,
            proto::ListAuditResponse {
                entries: Vec::new(),
                next_seq: 0,
            },
            "{wire:?}"
        );
    }
}
