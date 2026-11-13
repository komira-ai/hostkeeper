//! `HostGroups` end to end on a real coordinator, through the router as the private listener
//! serves it, once over gRPC and once over the JSON gateway: the five calls agree on both, every
//! refusal of every call is the status of its value and changes nothing, CI's token and no token
//! are refused before the request is read, and `Put` reads up to 64 KiB where the other calls
//! read 4 KiB.

use std::sync::Arc;

use bytes::Bytes;
use hostkeeper_enrollment::enroll::Membership;
use hostkeeper_enrollment::enroll::records::MemoryRecords;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use http_body_util::Full;
use hyper::{Request, StatusCode};
use prost::Message;
use tonic::Code;

use super::rollouts::{Refused, WIRES, Wire, rpc};
use super::support::{call, framed, grpc_request_raw, raw};
use super::targets::padded;
use crate::desired::Held;
use crate::enrollment::tests::support::Rig as EnrollmentRig;
use crate::grpc::operator_limit;
use crate::host_groups_support::{
    SELF, body, coordinator_on, enroll_host, move_of, put_of, remove_of,
};
use crate::http::{MAX_HOST_GROUPS_PUT_BYTES, MAX_OPERATOR_BYTES, Surface};
use crate::ops::OpsState;
use crate::service::Coordinator;
use crate::store::{Fake, audit_failure, store_failure};
use crate::testutil::{OPERATOR, Rig, ci_token};

const LIST: &str = "/hostkeeper.operator.v1.HostGroups/List";
const GET: &str = "/hostkeeper.operator.v1.HostGroups/Get";
const PUT: &str = "/hostkeeper.operator.v1.HostGroups/Put";
const REMOVE: &str = "/hostkeeper.operator.v1.HostGroups/Remove";
const MOVE: &str = "/hostkeeper.operator.v1.HostGroups/Move";

/// A coordinator the tests call through the router, with its enrollment and the store under it.
struct Wired {
    coordinator: Arc<Coordinator>,
    fake: Fake,
    enrollment: EnrollmentRig,
    _rig: Rig,
}

impl Wired {
    fn new(name: &str) -> Self {
        let enrollment = EnrollmentRig::over_records(
            &format!("{name}-enrollment"),
            Arc::new(MemoryRecords::new()),
        );
        let fake = Fake::new();
        let rig = Rig::new(name);
        let coordinator =
            coordinator_on(&fake, &rig.clock, Some(Arc::clone(&enrollment.enrollment)));
        rig.at(2_000);
        Self {
            coordinator: Arc::new(coordinator),
            fake,
            enrollment,
            _rig: rig,
        }
    }

    /// Everything a refused call must leave as it was.
    fn state(&self) -> (Held, OpsState, Vec<(String, Membership)>) {
        let memberships = self
            .enrollment
            .enrollment
            .store()
            .memberships()
            .expect("read")
            .into_iter()
            .map(|(id, membership)| (id.to_string(), membership))
            .collect();
        (self.fake.desired(), self.fake.ops(), memberships)
    }

    /// The call `path` with `request` and the operator's token over `wire`.
    async fn ask<R: Message + Default>(
        &self,
        wire: Wire,
        path: &str,
        request: &impl Message,
    ) -> Result<R, Refused> {
        rpc(&self.coordinator, wire, path, Some(OPERATOR), request).await
    }

    /// The refusal of `path` for `request` over `wire` with `bearer`.
    async fn refused(
        &self,
        wire: Wire,
        path: &str,
        bearer: Option<&str>,
        request: &impl Message,
    ) -> Refused {
        rpc::<proto::ListHostGroupsResponse>(&self.coordinator, wire, path, bearer, request)
            .await
            .expect_err("refused")
    }
}

fn list_request() -> proto::ListHostGroupsRequest {
    proto::ListHostGroupsRequest::default()
}

fn get_request(group: &str) -> proto::GetHostGroupRequest {
    proto::GetHostGroupRequest {
        host_group: group.to_owned(),
        ..proto::GetHostGroupRequest::default()
    }
}

fn token(code: Code, token: &str) -> Refused {
    (code, token.to_owned())
}

/// Proves: the five calls make the same answers over gRPC and over the gateway: on a coordinator
/// each, a group is put, listed, moved into by a host, got with its host, replaced at the
/// generation the answer named, refused its removal while the host is in it, and removed once the
/// host moved on; every answer of the one wire is the answer of the other.
///
/// Catches: an adapter that reads a field another way than the message, or that answers a call
/// with the answer of another.
#[tokio::test]
async fn the_five_calls_answer_the_same_over_both_wires() {
    let mut answers: Vec<Vec<Vec<u8>>> = Vec::new();
    for wire in WIRES {
        let wired = Wired::new(&format!("hg-wires-{wire:?}"));
        enroll_host(&wired.enrollment.enrollment, "d1");
        let mut seen: Vec<Vec<u8>> = Vec::new();
        let put: proto::PutHostGroupResponse = wired
            .ask(
                wire,
                PUT,
                &put_of(body("alpha", &[SELF, "svc"], None), None),
            )
            .await
            .expect("put");
        assert_eq!(put.generation, 1, "{wire:?}");
        seen.push(put.encode_to_vec());
        let listed: proto::ListHostGroupsResponse = wired
            .ask(wire, LIST, &list_request())
            .await
            .expect("listed");
        assert_eq!(listed.host_groups.len(), 1, "{wire:?}");
        seen.push(listed.encode_to_vec());
        let moved: proto::MoveHostResponse = wired
            .ask(wire, MOVE, &move_of("d1", "alpha"))
            .await
            .expect("moved");
        assert_eq!(moved.from_host_group, "", "{wire:?}");
        seen.push(moved.encode_to_vec());
        let got: proto::GetHostGroupResponse = wired
            .ask(wire, GET, &get_request("alpha"))
            .await
            .expect("got");
        assert_eq!(got.node_ids, ["d1"], "{wire:?}");
        seen.push(got.encode_to_vec());
        let replaced: proto::PutHostGroupResponse = wired
            .ask(
                wire,
                PUT,
                &put_of(body("alpha", &[SELF, "svc", "cfg"], None), Some(1)),
            )
            .await
            .expect("put");
        assert_eq!(replaced.generation, 2, "{wire:?}");
        seen.push(replaced.encode_to_vec());
        let kept = wired
            .refused(wire, REMOVE, Some(OPERATOR), &remove_of("alpha"))
            .await;
        assert_eq!(
            kept,
            token(Code::FailedPrecondition, "not_empty"),
            "{wire:?}"
        );
        wired
            .ask::<proto::MoveHostResponse>(wire, MOVE, &move_of("d1", "beta"))
            .await
            .expect_err("beta is not held");
        wired
            .ask::<proto::PutHostGroupResponse>(
                wire,
                PUT,
                &put_of(body("beta", &[SELF], None), None),
            )
            .await
            .expect("put");
        let moved: proto::MoveHostResponse = wired
            .ask(wire, MOVE, &move_of("d1", "beta"))
            .await
            .expect("moved");
        seen.push(moved.encode_to_vec());
        let removed: proto::RemoveHostGroupResponse = wired
            .ask(wire, REMOVE, &remove_of("alpha"))
            .await
            .expect("removed");
        assert_eq!(
            removed.entry.as_ref().expect("an entry").action,
            "host_group_remove",
            "{wire:?}"
        );
        seen.push(removed.encode_to_vec());
        answers.push(seen);
    }
    assert_eq!(answers[0], answers[1]);
}

/// Proves: CI's token is `PERMISSION_DENIED` `forbidden` and no token, a wrong one or a bearer
/// of another scheme `UNAUTHENTICATED` `unauthorized`, for each of the five calls over both
/// wires, before the request is read (a body that is not a message gets the same refusal), and
/// nothing changes.
///
/// Catches: a call of `HostGroups` open to CI, or one that reads the request before it knows the
/// caller.
#[tokio::test]
async fn ci_and_no_token_are_refused_by_each_call_on_both_wires() {
    let wired = Wired::new("hg-wires-credentials");
    let before = wired.state();
    for wire in WIRES {
        for path in [LIST, GET, PUT, REMOVE, MOVE] {
            assert_eq!(
                wired
                    .refused(wire, path, Some(&ci_token()), &list_request())
                    .await,
                token(Code::PermissionDenied, "forbidden"),
                "{wire:?} {path}"
            );
            for bearer in [None, Some("not-a-token-0123456789")] {
                assert_eq!(
                    wired.refused(wire, path, bearer, &list_request()).await,
                    token(Code::Unauthenticated, "unauthorized"),
                    "{wire:?} {path} {bearer:?}"
                );
            }
        }
    }
    assert_eq!(wired.state(), before);
}

/// A request of one of the five calls.
enum Req {
    List(proto::ListHostGroupsRequest),
    Get(proto::GetHostGroupRequest),
    Put(proto::PutHostGroupRequest),
    Remove(proto::RemoveHostGroupRequest),
    Move(proto::MoveHostRequest),
}

impl Req {
    /// The refusal of the call over `wire` with the operator's token.
    async fn refused(&self, wired: &Wired, wire: Wire) -> (&'static str, Refused) {
        match self {
            Self::List(r) => (LIST, wired.refused(wire, LIST, Some(OPERATOR), r).await),
            Self::Get(r) => (GET, wired.refused(wire, GET, Some(OPERATOR), r).await),
            Self::Put(r) => (PUT, wired.refused(wire, PUT, Some(OPERATOR), r).await),
            Self::Remove(r) => (REMOVE, wired.refused(wire, REMOVE, Some(OPERATOR), r).await),
            Self::Move(r) => (MOVE, wired.refused(wire, MOVE, Some(OPERATOR), r).await),
        }
    }
}

/// A `Put` of `group` listing `packages`, over `generation`.
fn put(group: &str, packages: &[&str], generation: Option<u64>) -> proto::PutHostGroupRequest {
    put_of(body(group, packages, None), generation)
}

/// A `Put` of `beta` whose policy `change` edits.
fn put_policy(change: fn(&mut proto::RolloutPolicy)) -> proto::PutHostGroupRequest {
    let mut policy = proto::RolloutPolicy::default();
    change(&mut policy);
    put_of(body("beta", &[SELF], Some(policy)), None)
}

/// Proves: every refusal of every call of `HostGroups` is the status of its value on both wires
/// (its code, and its token as the message) and changes nothing: `List` and `Get` of a bad page
/// token, a name that is none and a group not held; `Put` of a name, a list, a policy, a
/// generation and a reason that break their rules; `Remove` of a name that is none, a group not
/// held, a group a host is in; `Move` of a node id that is none, a host not enrolled, a group not
/// held, the group the host is in; and a store that fails, or whose audit cannot be written.
///
/// Catches: a refusal that changes code on the way out, or one that changes the store.
#[tokio::test]
async fn every_refusal_of_every_call_is_its_status_on_both_wires() {
    let wired = Wired::new("hg-wires-refusals");
    enroll_host(&wired.enrollment.enrollment, "d1");
    wired
        .ask::<proto::PutHostGroupResponse>(Wire::Grpc, PUT, &put("alpha", &[SELF], None))
        .await
        .expect("put");
    wired
        .ask::<proto::MoveHostResponse>(Wire::Grpc, MOVE, &move_of("d1", "alpha"))
        .await
        .expect("moved");
    let before = wired.state();
    let invalid = Code::InvalidArgument;
    let precondition = Code::FailedPrecondition;
    let not_found = Code::NotFound;
    let many: Vec<String> = std::iter::once(SELF.to_owned())
        .chain((1..65).map(|n| format!("p{n}")))
        .collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    let cases: Vec<(Req, Refused)> = vec![
        (
            Req::List(proto::ListHostGroupsRequest {
                limit: 0,
                page_token: "Bad".to_owned(),
            }),
            token(invalid, "invalid_request"),
        ),
        (
            Req::Get(get_request("Alpha")),
            token(invalid, "invalid_request"),
        ),
        (
            Req::Get(proto::GetHostGroupRequest {
                page_token: "Up".to_owned(),
                ..get_request("alpha")
            }),
            token(invalid, "invalid_request"),
        ),
        (
            Req::Get(get_request("beta")),
            token(not_found, "unknown_host_group"),
        ),
        (
            Req::Put(put("Beta", &[SELF], None)),
            token(invalid, "invalid_host_group"),
        ),
        (
            Req::Put(put("all", &[], None)),
            token(precondition, "hostkeeper_not_listed"),
        ),
        (
            Req::Put(put("all", &["svc"], None)),
            token(precondition, "hostkeeper_not_listed"),
        ),
        (
            Req::Put(put("all", &[SELF], None)),
            token(precondition, "reserved_host_group"),
        ),
        (
            Req::Remove(remove_of("all")),
            token(precondition, "reserved_host_group"),
        ),
        (
            Req::Move(move_of("d1", "all")),
            token(precondition, "reserved_host_group"),
        ),
        (
            Req::Put(put("beta", &many, None)),
            token(invalid, "too_many_packages"),
        ),
        (
            Req::Put(put("beta", &[SELF, "bad name"], None)),
            token(invalid, "invalid_package"),
        ),
        (
            Req::Put(put("beta", &[SELF, "svc", "svc"], None)),
            token(invalid, "invalid_package"),
        ),
        (
            Req::Put(put_policy(|policy| policy.min_up = 10_001)),
            token(invalid, "invalid_policy"),
        ),
        (
            Req::Put(put("alpha", &[SELF], None)),
            token(precondition, "generation_mismatch"),
        ),
        (
            Req::Put(put("beta", &[SELF], Some(1))),
            token(precondition, "generation_mismatch"),
        ),
        (
            Req::Put(proto::PutHostGroupRequest {
                reason: String::new(),
                ..put("beta", &[SELF], None)
            }),
            token(invalid, "invalid_request"),
        ),
        (
            Req::Put(proto::PutHostGroupRequest {
                host_group: None,
                ..put("beta", &[SELF], None)
            }),
            token(invalid, "invalid_request"),
        ),
        (
            Req::Remove(remove_of("Alpha")),
            token(invalid, "invalid_request"),
        ),
        (
            Req::Remove(remove_of("beta")),
            token(not_found, "unknown_host_group"),
        ),
        (
            Req::Remove(remove_of("alpha")),
            token(precondition, "not_empty"),
        ),
        (
            Req::Move(move_of("Bad_Id", "alpha")),
            token(invalid, "invalid_request"),
        ),
        (
            Req::Move(move_of("ghost", "alpha")),
            token(not_found, "unknown_host"),
        ),
        (
            Req::Move(move_of("d1", "beta")),
            token(not_found, "unknown_host_group"),
        ),
        (
            Req::Move(move_of("d1", "alpha")),
            token(precondition, "same_host_group"),
        ),
    ];
    for wire in WIRES {
        for (request, want) in &cases {
            let (path, got) = request.refused(&wired, wire).await;
            assert_eq!(&got, want, "{wire:?} {path}");
        }
        for (failure, want) in [
            (
                store_failure as fn() -> _,
                token(Code::Internal, "store_failed"),
            ),
            (audit_failure, token(Code::Unavailable, "audit_unavailable")),
        ] {
            wired.fake.refuse(move |_| Some(failure()));
            let (_, got) = Req::Put(put("beta", &[SELF], None))
                .refused(&wired, wire)
                .await;
            assert_eq!(got, want, "{wire:?}");
            wired.fake.heal();
        }
        assert_eq!(wired.state(), before, "{wire:?}");
    }
}

/// Proves: the operator limit of a path is 64 KiB for `HostGroups.Put` and 4 KiB for the other
/// four methods of `HostGroups`; over gRPC a `Put` message of 64 KiB is read and one byte more
/// is refused `OUT_OF_RANGE` with the limit in the message, and a `Move` of 4 KiB plus one byte is
/// refused at 4 KiB; on the gateway a `Put` body of 64 KiB is read and one byte more is
/// `invalid_request`.
///
/// Catches: a `Put` limited to 4 KiB, which would refuse a group of 64 long package names, or
/// one that reads as much as `SetDesired`'s.
#[tokio::test]
async fn put_reads_64_kib_and_the_other_calls_4_kib() {
    assert_eq!(MAX_HOST_GROUPS_PUT_BYTES, 64 * 1024);
    assert_eq!(operator_limit(PUT), 65_536);
    for path in [LIST, GET, REMOVE, MOVE] {
        assert_eq!(operator_limit(path), MAX_OPERATOR_BYTES, "{path}");
    }
    let wired = Wired::new("hg-wires-limits");
    let base = put_of(body("alpha", &[SELF], None), None).encode_to_vec();
    let grpc = |path: &str, bytes: Vec<u8>| {
        let request = grpc_request_raw(path, framed(&bytes), Some(OPERATOR));
        let coordinator = Arc::clone(&wired.coordinator);
        async move { call(&coordinator, Surface::Control, &None, request).await }
    };
    let (code, message, _) = grpc(PUT, padded(base.clone(), MAX_HOST_GROUPS_PUT_BYTES)).await;
    assert_eq!((code, message.as_str()), (Code::Ok, ""));
    let (code, message, _) = grpc(
        PUT,
        padded(
            put_of(body("beta", &[SELF], None), None).encode_to_vec(),
            MAX_HOST_GROUPS_PUT_BYTES + 1,
        ),
    )
    .await;
    assert_eq!(code, Code::OutOfRange);
    assert!(message.contains("the limit is: 65536 bytes"), "{message}");
    let (code, message, _) = grpc(
        MOVE,
        padded(
            move_of("d1", "alpha").encode_to_vec(),
            MAX_OPERATOR_BYTES + 1,
        ),
    )
    .await;
    assert_eq!(code, Code::OutOfRange);
    assert!(message.contains("the limit is: 4096 bytes"), "{message}");

    let json = r#"{"hostGroup":{"name":"gamma","packages":["hostkeeper"]},"reason":"r"}"#;
    let spaced = |size: usize| format!("{json}{}", " ".repeat(size - json.len()));
    let post = |text: String| {
        let request = Request::builder()
            .method("POST")
            .uri(PUT)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {OPERATOR}"))
            .body(Full::new(Bytes::from(text)))
            .expect("a request");
        let coordinator = Arc::clone(&wired.coordinator);
        async move { raw(&coordinator, Surface::Control, &None, request).await }
    };
    let (status, _, _) = post(spaced(MAX_HOST_GROUPS_PUT_BYTES)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, text) = post(spaced(MAX_HOST_GROUPS_PUT_BYTES + 1)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        String::from_utf8(text.to_vec()).expect("text"),
        r#"{"code":"INVALID_ARGUMENT","reason":"invalid_request"}"#
    );
}
