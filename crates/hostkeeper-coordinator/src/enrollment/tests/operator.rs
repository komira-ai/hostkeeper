//! `Enrollment.List` and `Enrollment.Approve` as operator calls: who may make them, a
//! coordinator without enrollment, and the whole chain from a host's `Enroll` to a heartbeat
//! with the certificate an approval gave it.

use std::sync::Arc;

use bytes::Bytes;
use hostkeeper_enrollment::enroll::{RequestState, poll_message};
use hostkeeper_proto::hostkeeper::agent::v1::{
    EnrollResponse, EnrollState, PollRequest, PollResponse, PollState,
};
use hostkeeper_proto::hostkeeper::operator::v1::{
    ApproveRequest, EnrollmentRefusal, ListPendingRequest,
};
use hostkeeper_wire::wire::HEARTBEAT_PATH;
use http_body_util::{BodyExt as _, Full};
use hyper::{Request, StatusCode};
use prost::Message;
use serde_json::Value;

use super::support::{
    HostKey, PATIENCE, Rig, approval, audit_of, body_of, coordinator_over, enroll_request, enrolled,
};
use crate::enrollment::{Method, call, pem};
use crate::http::{Surface, handle};
use crate::operator::Caller;
use crate::testutil::{OPERATOR, ci_token};

/// Catches either call reached without the operator token, or the token checked after the
/// request is read: no token, a token that is none, a host's certificate given as a bearer,
/// and a valid CI token are each refused on both calls, and nothing changes.
#[tokio::test]
async fn enrollment_methods_refuse_every_token_but_an_operators() {
    let enrollment = Rig::new("operator-callers");
    let rig = coordinator_over("operator-callers-coordinator", &enrollment).await;
    let e = &enrollment.enrollment;
    let host = enrolled(e, &HostKey::new(), "machine-0", "s0");
    let key = HostKey::new();
    e.enroll(enroll_request(&key, "machine-1"))
        .expect("enrolled");
    let ci = ci_token();
    let host_bearer = pem("CERTIFICATE", &host);
    let callers = [
        (
            rig.coordinator.caller(None).await,
            EnrollmentRefusal::Unauthorized,
        ),
        (
            rig.coordinator.caller(Some("not-a-token")).await,
            EnrollmentRefusal::Unauthorized,
        ),
        (
            rig.coordinator.caller(Some(host_bearer.as_str())).await,
            EnrollmentRefusal::Unauthorized,
        ),
        (
            rig.coordinator.caller(Some(ci.as_str())).await,
            EnrollmentRefusal::Forbidden,
        ),
    ];
    assert!(matches!(callers[3].0, Caller::Ci), "{:?}", callers[3].0);
    let before = audit_of(&rig.coordinator);
    for (caller, refusal) in &callers {
        assert_eq!(
            rig.coordinator
                .enrollment_list(caller, &ListPendingRequest::default()),
            Err(*refusal)
        );
        // A request that is no approval at all is not looked at either.
        for request in [approval(&key, "s1"), ApproveRequest::default()] {
            assert_eq!(
                rig.coordinator.enrollment_approve(caller, request),
                Err(*refusal)
            );
        }
    }
    assert_eq!(audit_of(&rig.coordinator), before, "nothing is audited");
    assert_eq!(
        e.store()
            .request(&key.key_hash())
            .expect("read")
            .expect("held")
            .state,
        RequestState::Pending
    );
    assert!(e.store().recorded_approvals().expect("read").is_empty());
}

/// Catches a coordinator without `--enrollment-dir` failing the list or approving anything.
#[tokio::test]
async fn a_coordinator_without_enrollment_lists_nothing_and_approves_nothing() {
    let rig = crate::testutil::Rig::new("operator-no-enrollment");
    let operator = rig.coordinator.caller(Some(OPERATOR)).await;
    let listed = rig
        .coordinator
        .enrollment_list(&operator, &ListPendingRequest::default())
        .expect("listed");
    assert!(listed.requests.is_empty() && listed.next_page_token.is_empty());
    assert_eq!(
        rig.coordinator
            .enrollment_approve(&operator, approval(&HostKey::new(), "s1")),
        Err(EnrollmentRefusal::UnknownRequest)
    );
}

/// One call of `method` with no client certificate, its answer decoded.
async fn unauthenticated<M: Message + Default>(
    rig: &Rig,
    method: Method,
    body: &impl Message,
) -> M {
    let answer = call(&rig.enrollment, method, &None, body_of(body), PATIENCE)
        .await
        .expect("answered");
    M::decode(answer.as_slice()).expect("the method's answer")
}

/// Catches a link of the chain that does not hold: a host's `Enroll` through the in-process
/// transport is listed with the key hash the approval then names, the host's signed poll then
/// takes a certificate for the approved host id, and the coordinator admits a heartbeat with
/// that certificate.
#[tokio::test]
async fn an_operator_enrolls_a_real_daemon_request_end_to_end() {
    let enrollment = Rig::new("operator-end-to-end");
    let rig = coordinator_over("operator-end-to-end-coordinator", &enrollment).await;
    let operator = rig.coordinator.caller(Some(OPERATOR)).await;
    let key = HostKey::new();

    let enrolled: EnrollResponse = unauthenticated(
        &enrollment,
        Method::Enroll,
        &enroll_request(&key, "machine-1"),
    )
    .await;
    assert_eq!(enrolled.state, EnrollState::Pending as i32);
    let listed = rig
        .coordinator
        .enrollment_list(&operator, &ListPendingRequest::default())
        .expect("listed");
    let pending = &listed.requests[0];
    assert_eq!(pending.request_id, enrolled.request_id);
    let answer = rig
        .coordinator
        .enrollment_approve(
            &operator,
            ApproveRequest {
                request_id: pending.request_id.clone(),
                key_sha256: pending.key_sha256.clone(),
                host_id: "s1".to_owned(),
                role: "storage".to_owned(),
                services: vec!["kbf".to_owned()],
                replace: false,
                reason: "racked in row 4".to_owned(),
                host_group: String::new(),
            },
        )
        .expect("approved");
    assert!(answer.certificate_issued_at_poll);

    let challenge: PollResponse = unauthenticated(
        &enrollment,
        Method::Poll,
        &PollRequest {
            request_id: enrolled.request_id.clone(),
            ..PollRequest::default()
        },
    )
    .await;
    let signed = PollRequest {
        request_id: enrolled.request_id,
        signature: key.sign(&poll_message(&key.key_hash(), &challenge.nonce)),
        nonce: challenge.nonce,
    };
    let issued: PollResponse = unauthenticated(&enrollment, Method::Poll, &signed).await;
    assert_eq!(issued.state, PollState::Issued as i32);
    assert_eq!(issued.host_id, "s1");

    let beat = r#"{"platform":"linux-x86_64","version":"1.0","health":"healthy"}"#;
    let request = Request::post(HEARTBEAT_PATH)
        .body(Full::new(Bytes::from_static(beat.as_bytes())))
        .expect("a request");
    let peer = Some(Arc::from(issued.certificate_der.as_slice()));
    let response = handle(
        &rig.coordinator,
        Surface::Heartbeat,
        &peer,
        request,
        PATIENCE,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response
        .into_body()
        .collect()
        .await
        .expect("a body")
        .to_bytes();
    let reply: Value = serde_json::from_slice(&body).expect("JSON");
    assert!(reply.get("error").is_none(), "{reply}");
}
