//! Service identity through the router as the listeners serve it: `IssueServiceCertificate` and
//! `ListDeny` over gRPC on the heartbeat listener, `GET /v1/deny`, what a heartbeat that carries reports
//! shows in `Hosts.Get`, a service certificate refused wherever a host's is
//! needed, and `HostIdentity` over gRPC and the JSON gateway on the private one.

use std::sync::Arc;

use bytes::Bytes;
use hostkeeper_enrollment::fs::FaultFs;
use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use http_body_util::Full;
use hyper::{Request, StatusCode};
use prost::Message;
use prost_reflect::{DynamicMessage, SerializeOptions};
use serde_json::{Value, json};
use tonic::Code;

use super::heartbeat::{beat, json_beat, proto_message, svc};
use super::support::{call, decode, grpc_request, raw};
use crate::enrollment::os_random;
use crate::enrollment::tests::support::{
    HostKey, Rig as EnrollmentRig, coordinator_over, enrolled,
};
use crate::grpc::gateway::pool;
use crate::http::{Peer, Surface};
use crate::service_identity_support::{Fix, Held, SERVICE, Wired, csr_of, request_for};
use crate::testutil::{OPERATOR, ci_token};

const ISSUE: &str = "/hostkeeper.agent.v1.Coordinator/IssueServiceCertificate";
const LIST_DENY: &str = "/hostkeeper.agent.v1.Coordinator/ListDeny";
const RENEW: &str = "/hostkeeper.agent.v1.Coordinator/Renew";
const HOSTS_GET: &str = "/hostkeeper.operator.v1.Hosts/Get";
const IDENTITY_GET: &str = "/hostkeeper.operator.v1.HostIdentity/Get";
const IDENTITY_APPROVE: &str = "/hostkeeper.operator.v1.HostIdentity/Approve";

/// The code and the message of a call of the heartbeat listener's service.
async fn agent_call(
    w: &Wired,
    peer: &Peer,
    path: &str,
    message: impl Message,
) -> (Code, String, Option<Bytes>) {
    call(
        &w.coordinator,
        Surface::Heartbeat,
        peer,
        grpc_request(path, message, None),
    )
    .await
}

/// The code and the message of a call of the private listener's service by the bearer `bearer`.
async fn operator_call(
    w: &Wired,
    path: &str,
    bearer: Option<&str>,
    message: impl Message,
) -> (Code, String, Option<Bytes>) {
    call(
        &w.coordinator,
        Surface::Control,
        &None,
        grpc_request(path, message, bearer),
    )
    .await
}

fn token(answer: &(Code, String, Option<Bytes>)) -> (Code, &str) {
    (answer.0, answer.1.as_str())
}

/// The hosts `Coordinator.ListDeny` answers to `peer`.
async fn denied_by_grpc(w: &Wired, peer: &Peer) -> Vec<String> {
    let answer = agent_call(w, peer, LIST_DENY, agent::ListDenyRequest {}).await;
    assert_eq!(token(&answer), (Code::Ok, ""));
    decode::<agent::ListDenyResponse>(&answer.2.expect("a message")).host_ids
}

/// The status, the content type and the body of `GET /v1/deny` from `peer`.
async fn deny_route(
    w: &Wired,
    peer: &Peer,
    method: &str,
    surface: Surface,
) -> (StatusCode, String, String) {
    let request = Request::builder()
        .method(method)
        .uri("/v1/deny")
        .body(Full::new(Bytes::new()))
        .expect("a request");
    let (status, headers, body) = raw(&w.coordinator, surface, peer, request).await;
    let kind = headers
        .get("content-type")
        .map(|value| value.to_str().expect("text").to_owned())
        .unwrap_or_default();
    (
        status,
        kind,
        String::from_utf8(body.to_vec()).expect("text"),
    )
}

/// The farm with the hosts of the tests: two an operator approved (`node-01`, `node-02`), one the
/// inventory approved (`node-03`), one retired (`node-04`) and one in no group (`node-05`).
struct Hosts {
    ok: Held,
    spare: Held,
    auto: Held,
    gone: Held,
    lone: Held,
    service_cert: Peer,
}

async fn hosts(name: &str) -> (Hosts, Wired) {
    let fix = Fix::new(name).await;
    let ok = fix.operator_host("node-01");
    let spare = fix.operator_host("node-02");
    let auto = fix.auto_host("node-03");
    let gone = fix.operator_host("node-04");
    let lone = fix.lone_host("node-05");
    let service_cert = Some(Arc::<[u8]>::from(
        fix.ask(&ok, SERVICE).expect("issued").certificate_der,
    ));
    fix.coordinator()
        .retire_host(
            &crate::marks_support::operator(),
            &proto::RetireRequest {
                node_id: "node-04".to_owned(),
                reason: String::new(),
            },
        )
        .expect("retired");
    let held = Hosts {
        ok,
        spare,
        auto,
        gone,
        lone,
        service_cert,
    };
    (held, fix.wired())
}

/// Proves: `IssueServiceCertificate` over gRPC answers the certificate (read back by the CA's
/// reader: the service, the host and the key of the CSR) to an operator's host whose group lists
/// the service with an identity, and each refusal is the status of its token, with the code the
/// proto names: `forbidden` and `denied` are `PERMISSION_DENIED`, `invalid_service`, `bad_csr`
/// and `key_not_p256` are `INVALID_ARGUMENT`, `service_not_assigned` and `identity_not_approved`
/// are `FAILED_PRECONDITION`, and a refusal changes nothing.
///
/// Catches: a refusal told with another code or token, and the answer of a certificate that does not
/// read back as the host's service.
#[tokio::test]
async fn issue_service_certificate_answers_a_certificate_or_the_token_of_its_refusal() {
    let (h, w) = hosts("grpc-sid-issue").await;
    let key = HostKey::new();
    let issue = |peer: &Peer, request: agent::IssueServiceCertificateRequest| {
        let w = &w;
        let peer = peer.clone();
        async move { agent_call(w, &peer, ISSUE, request).await }
    };

    let answer = issue(&h.spare.peer(), request_for(SERVICE, &key)).await;
    assert_eq!(token(&answer), (Code::Ok, ""));
    let issued: agent::IssueServiceCertificateResponse = decode(&answer.2.expect("a message"));
    let read = w
        .enrollment
        .enrollment
        .ca_bundle()
        .service_certificate(&issued.certificate_der)
        .expect("a service certificate");
    assert_eq!(
        (read.service.as_str(), read.host_id.as_str(), read.key_hash),
        (SERVICE, "node-02", key.key_hash())
    );

    let bad = |service: &str, csr: Vec<u8>| agent::IssueServiceCertificateRequest {
        service: service.to_owned(),
        csr_der: csr,
    };
    let good = || request_for(SERVICE, &HostKey::new());
    let p384 = csr_of(&HostKey::of(&rcgen::PKCS_ECDSA_P384_SHA384), "service:x");
    let cases = [
        (None, good(), (Code::PermissionDenied, "forbidden")),
        (h.gone.peer(), good(), (Code::PermissionDenied, "denied")),
        (
            h.ok.peer(),
            bad("a b", Vec::new()),
            (Code::InvalidArgument, "invalid_service"),
        ),
        (
            h.ok.peer(),
            bad(SERVICE, b"no".to_vec()),
            (Code::InvalidArgument, "bad_csr"),
        ),
        (
            h.ok.peer(),
            bad(SERVICE, p384),
            (Code::InvalidArgument, "key_not_p256"),
        ),
        (
            h.lone.peer(),
            good(),
            (Code::FailedPrecondition, "service_not_assigned"),
        ),
        (
            h.auto.peer(),
            good(),
            (Code::FailedPrecondition, "identity_not_approved"),
        ),
        (
            h.service_cert.clone(),
            good(),
            (Code::PermissionDenied, "forbidden"),
        ),
    ];
    for (peer, request, want) in cases {
        let answer = issue(&peer, request).await;
        assert_eq!(token(&answer), want);
        assert_eq!(answer.2, None);
    }
}

/// Proves: an `IssueServiceCertificate` whose audit entry cannot be written is `UNAVAILABLE`
/// `store_failed` with no message, and the host is answered once the store takes writes.
///
/// Catches: a store failure told as an internal error, which a daemon would not retry the same.
#[tokio::test]
async fn a_certificate_that_cannot_be_recorded_is_unavailable_and_nothing_is_answered() {
    let (h, w) = hosts("grpc-sid-store").await;
    w.fake.refuse(|_| Some(crate::store::audit_failure()));
    let answer = agent_call(
        &w,
        &h.ok.peer(),
        ISSUE,
        request_for(SERVICE, &HostKey::new()),
    )
    .await;
    assert_eq!(token(&answer), (Code::Unavailable, "store_failed"));
    assert_eq!(answer.2, None);
    w.fake.heal();
    let answer = agent_call(
        &w,
        &h.ok.peer(),
        ISSUE,
        request_for(SERVICE, &HostKey::new()),
    )
    .await;
    assert_eq!(token(&answer), (Code::Ok, ""));
}

/// Proves: a request of more than 8 KiB is refused by the service's message limit with no
/// certificate.
///
/// Catches: a CSR of any size read.
#[tokio::test]
async fn a_request_over_the_message_limit_is_refused() {
    let (h, w) = hosts("grpc-sid-limit").await;
    let large = agent::IssueServiceCertificateRequest {
        service: SERVICE.to_owned(),
        csr_der: vec![0; crate::enrollment::MAX_MESSAGE_BYTES + 1],
    };
    let answer = agent_call(&w, &h.ok.peer(), ISSUE, large).await;
    assert_ne!(answer.0, Code::Ok);
    assert_eq!(answer.2, None);
}

/// Proves: `ListDeny` over gRPC answers the hosts the coordinator denies to a host's certificate
/// and to a service certificate, and refuses a call with no certificate `forbidden` and one by a
/// denied host `denied`.
///
/// Catches: a list that anyone reaches, or that a relying party cannot read with its certificate.
#[tokio::test]
async fn list_deny_answers_a_host_or_a_service_certificate_and_no_one_else() {
    let (h, w) = hosts("grpc-sid-list").await;
    assert_eq!(denied_by_grpc(&w, &h.ok.peer()).await, ["node-04"]);
    assert_eq!(denied_by_grpc(&w, &h.service_cert).await, ["node-04"]);
    for (peer, want) in [
        (None, (Code::PermissionDenied, "forbidden")),
        (h.gone.peer(), (Code::PermissionDenied, "denied")),
    ] {
        let answer = agent_call(&w, &peer, LIST_DENY, agent::ListDenyRequest {}).await;
        assert_eq!(token(&answer), want);
        assert_eq!(answer.2, None);
    }
}

/// Proves: `GET /v1/deny` on the heartbeat listener answers `{"hostIds":[...]}` as JSON, the list
/// `ListDeny` answers, in the proto's canonical JSON (the gateway's serialisation of the same
/// message), to a host's certificate and to a service certificate; `{"hostIds":[]}` when none is
/// denied; 403 `forbidden` with no certificate; 403 `denied` for a denied host; 405 for another
/// method; and no route on the private listener.
///
/// Catches: a JSON route that says something other than the gRPC call, answers a caller with no
/// certificate, or is served where the operator's calls are.
#[tokio::test]
async fn get_deny_answers_the_list_as_json() {
    let (h, w) = hosts("grpc-sid-route").await;
    let heartbeat = Surface::Heartbeat;
    let (status, kind, body) = deny_route(&w, &h.ok.peer(), "GET", heartbeat).await;
    assert_eq!(
        (status, kind.as_str()),
        (StatusCode::OK, "application/json")
    );
    assert_eq!(body, r#"{"hostIds":["node-04"]}"#);
    let canonical = {
        let descriptor = pool(hostkeeper_proto::FILE_DESCRIPTOR_SET)
            .expect("the pool")
            .get_message_by_name("hostkeeper.agent.v1.ListDenyResponse")
            .expect("the message");
        let message = agent::ListDenyResponse {
            host_ids: vec!["node-04".to_owned()],
        };
        let dynamic = DynamicMessage::decode(descriptor, message.encode_to_vec().as_slice())
            .expect("decoded");
        let mut writer = serde_json::Serializer::new(Vec::new());
        dynamic
            .serialize_with_options(&mut writer, &SerializeOptions::new())
            .expect("JSON");
        String::from_utf8(writer.into_inner()).expect("text")
    };
    assert_eq!(body, canonical);
    assert_eq!(
        deny_route(&w, &h.service_cert, "GET", heartbeat).await.2,
        body
    );

    for (peer, status, error) in [
        (None, StatusCode::FORBIDDEN, "forbidden"),
        (h.gone.peer(), StatusCode::FORBIDDEN, "denied"),
    ] {
        let (got, _, body) = deny_route(&w, &peer, "GET", heartbeat).await;
        assert_eq!(got, status);
        assert_eq!(
            serde_json::from_str::<Value>(&body).expect("JSON"),
            json!({"error": error})
        );
    }
    assert_eq!(
        deny_route(&w, &h.ok.peer(), "POST", heartbeat).await.0,
        StatusCode::METHOD_NOT_ALLOWED
    );
    assert_eq!(
        deny_route(&w, &h.ok.peer(), "GET", Surface::Control)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

/// Proves: with no host denied the route answers `{"hostIds":[]}`, and a coordinator with no
/// enrollment answers 503 `unavailable` and `ListDeny` `UNAVAILABLE`.
///
/// Catches: a list that reads `{}` to a reader that wants the key, and a coordinator without
/// enrollment that answers an empty list as if no host were denied.
#[tokio::test]
async fn the_list_is_empty_when_none_is_denied_and_unavailable_without_enrollment() {
    let fix = Fix::new("grpc-sid-empty").await;
    let held = fix.operator_host("node-01");
    let w = fix.wired();
    let (status, _, body) = deny_route(&w, &held.peer(), "GET", Surface::Heartbeat).await;
    assert_eq!(
        (status, body.as_str()),
        (StatusCode::OK, r#"{"hostIds":[]}"#)
    );

    let rig = crate::testutil::Rig::new("grpc-sid-no-enrollment");
    let bare = Wired::bare(rig);
    let (status, _, body) = deny_route(&bare, &held.peer(), "GET", Surface::Heartbeat).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body, r#"{"error":"unavailable"}"#);
    let answer = agent_call(&bare, &held.peer(), LIST_DENY, agent::ListDenyRequest {}).await;
    assert_eq!(token(&answer), (Code::Unavailable, ""));
    let answer = agent_call(
        &bare,
        &held.peer(),
        ISSUE,
        request_for(SERVICE, &HostKey::new()),
    )
    .await;
    assert_eq!(token(&answer), (Code::Unavailable, ""));
}

/// Proves: a service certificate presented as a host's is refused `forbidden` by the gRPC
/// `Heartbeat`, by the JSON heartbeat, by `Renew` and by a read of a repository, each before the
/// request is read; the host certificate it was issued under is not.
///
/// Catches: a service that can act as the daemon of the host it runs on.
#[tokio::test]
async fn a_service_certificate_is_no_host_certificate_on_any_route() {
    let (h, w) = hosts("grpc-sid-no-host").await;
    let message = proto_message(&[svc("hostkeeper", "1.0")]);

    let by_service = beat(&w.coordinator, &h.service_cert, &message).await;
    assert_eq!(by_service.token(), (Code::PermissionDenied, "forbidden"));
    let by_host = beat(&w.coordinator, &h.ok.peer(), &message).await;
    assert_eq!(by_host.token(), (Code::Ok, ""));

    let (status, body) = json_beat(&w.coordinator, &h.service_cert, "{}").await;
    assert_eq!((status, body), (403, json!({"error": "forbidden"})));

    let renew = agent::RenewRequest {
        csr_der: h.ok.key.csr(),
    };
    let answer = agent_call(&w, &h.service_cert, RENEW, renew).await;
    assert_eq!(token(&answer), (Code::PermissionDenied, "forbidden"));

    let read = |peer: Peer| {
        let request = Request::builder()
            .method("GET")
            .uri("/repositories/komira/dists/farm/Release")
            .body(Full::new(Bytes::new()))
            .expect("a request");
        let w = &w;
        async move { raw(&w.coordinator, Surface::Heartbeat, &peer, request).await }
    };
    let (status, _, body) = read(h.service_cert.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body, Bytes::from_static(br#"{"error":"forbidden"}"#));
    assert_ne!(read(h.ok.peer()).await.0, StatusCode::FORBIDDEN);
}

/// Proves: `Enroll` and `Poll`, which take no certificate, answer a call that presents a service
/// certificate exactly as one that presents none: it gets no more.
///
/// Catches: a service certificate that is read by a route that has no use for one.
#[tokio::test]
async fn enroll_and_poll_answer_a_service_certificate_as_no_certificate() {
    let (h, w) = hosts("grpc-sid-enroll").await;
    let enroll = |peer: &Peer| {
        let request = crate::enrollment::tests::support::enroll_request(&HostKey::new(), "m-9");
        let peer = peer.clone();
        let w = &w;
        async move {
            let answer =
                agent_call(w, &peer, "/hostkeeper.agent.v1.Coordinator/Enroll", request).await;
            (answer.0, answer.1)
        }
    };
    assert_eq!(enroll(&h.service_cert).await, enroll(&None).await);
    let poll = |peer: &Peer| {
        let peer = peer.clone();
        let w = &w;
        async move {
            let request = agent::PollRequest {
                request_id: "0".repeat(64),
                ..agent::PollRequest::default()
            };
            let answer =
                agent_call(w, &peer, "/hostkeeper.agent.v1.Coordinator/Poll", request).await;
            (answer.0, answer.1)
        }
    };
    assert_eq!(poll(&h.service_cert).await, poll(&None).await);
    assert_eq!(
        poll(&None).await,
        (Code::NotFound, "unknown_request".to_owned())
    );
}

/// The list of one service as a daemon reports it.
fn report(
    service: &str,
    not_after: Option<u64>,
    error: Option<&str>,
) -> agent::ServiceIdentityReport {
    agent::ServiceIdentityReport {
        service: service.to_owned(),
        not_after_unix: not_after,
        last_error: error.map(str::to_owned),
    }
}

/// A heartbeat of the farm's daemon that carries `list` and announces no feature for it.
fn carrying(list: Vec<agent::ServiceIdentityReport>) -> agent::HeartbeatRequest {
    let mut message = proto_message(&[svc("hostkeeper", "1.0")]);
    message.service_identities = list;
    message
}

/// What `Hosts.Get` shows of `node`'s service identities.
async fn shown(w: &Wired, node: &str) -> Vec<agent::ServiceIdentityReport> {
    let get = proto::GetHostRequest {
        node_id: node.to_owned(),
    };
    let answer = operator_call(w, HOSTS_GET, Some(OPERATOR), get).await;
    assert_eq!(token(&answer), (Code::Ok, ""));
    let host: proto::GetHostResponse = decode(&answer.2.expect("a message"));
    host.host
        .expect("the host")
        .heartbeat
        .expect("its heartbeat")
        .service_identities
}

/// What the status counts of dropped lists.
fn dropped(w: &Wired) -> Option<u64> {
    let status = w.coordinator.status(Some(OPERATOR)).expect("status");
    status.extras_dropped.get("service_identities").copied()
}

/// Proves: the list a gRPC heartbeat carries (it announces no feature for it) is what `Hosts.Get`
/// shows in the host's heartbeat, with each `last_error` cut to 256 bytes at a character boundary;
/// a later list replaces it; and an empty list (what a daemon that predates the field sends)
/// clears what was shown. Nothing is counted as dropped.
///
/// Catches: a list that needs a feature no daemon announces, an error text kept whole, and a
/// service that stays shown after the daemon no longer has it.
#[tokio::test]
async fn hosts_show_the_last_list_a_heartbeat_reported() {
    let (h, w) = hosts("grpc-sid-shown").await;
    let peer = h.ok.peer();
    let send = |message: agent::HeartbeatRequest| {
        let w = &w;
        let peer = peer.clone();
        async move {
            assert_eq!(
                beat(&w.coordinator, &peer, &message).await.token(),
                (Code::Ok, "")
            );
        }
    };
    assert_eq!(shown(&w, "node-01").await, []);

    // 100 characters of three bytes: 300 bytes, cut to 255 (85 characters) at a boundary.
    let long = "\u{20ac}".repeat(100);
    send(carrying(vec![
        report("js-agent", Some(1_800_000_000), None),
        report("tierstore", None, Some(&long)),
    ]))
    .await;
    assert_eq!(
        shown(&w, "node-01").await,
        [
            report("js-agent", Some(1_800_000_000), None),
            report("tierstore", None, Some(&"\u{20ac}".repeat(85))),
        ]
    );

    send(carrying(vec![report(
        "js-agent",
        Some(1_800_028_800),
        None,
    )]))
    .await;
    let second = [report("js-agent", Some(1_800_028_800), None)];
    assert_eq!(shown(&w, "node-01").await, second);

    send(carrying(Vec::new())).await;
    assert_eq!(shown(&w, "node-01").await, []);
    assert_eq!(dropped(&w), None);
}

/// Proves: a list that breaks a rule (17 services, a name that is no service name, a service
/// twice) is counted once each as a dropped fact and not shown, the heartbeat is answered all the
/// same, and the last good list stays shown; a list of exactly 16 is good.
///
/// Catches: a heartbeat refused for a bad list, a bad list shown or replacing the good one, a
/// bound off by one, and a bad list not counted.
#[tokio::test]
async fn a_list_that_breaks_a_rule_is_dropped_and_the_last_one_kept() {
    let (h, w) = hosts("grpc-sid-dropped").await;
    let peer = h.ok.peer();
    let send = |list: Vec<agent::ServiceIdentityReport>| {
        let w = &w;
        let peer = peer.clone();
        async move {
            let answer = beat(&w.coordinator, &peer, &carrying(list)).await;
            assert_eq!(answer.token(), (Code::Ok, ""));
        }
    };
    let named = |names: &[String]| -> Vec<agent::ServiceIdentityReport> {
        names
            .iter()
            .map(|name| report(name, Some(5), None))
            .collect()
    };
    let sixteen: Vec<String> = (0..16).map(|n| format!("svc-{n}")).collect();
    send(named(&sixteen)).await;
    assert_eq!(shown(&w, "node-01").await.len(), 16);
    assert_eq!(dropped(&w), None);

    let mut seventeen = sixteen.clone();
    seventeen.push("svc-16".to_owned());
    let bad_name = vec!["fine".to_owned(), "not a name".to_owned()];
    let twice = vec!["js-agent".to_owned(), "js-agent".to_owned()];
    for (count, list) in [seventeen, bad_name, twice].into_iter().enumerate() {
        send(named(&list)).await;
        assert_eq!(
            shown(&w, "node-01").await.len(),
            16,
            "after list {}",
            count + 1
        );
        assert_eq!(dropped(&w), Some(count as u64 + 1));
    }

    send(named(&["js-agent".to_owned()])).await;
    assert_eq!(shown(&w, "node-01").await.len(), 1);
}

/// Proves: `HostIdentity` over gRPC refuses no credential `unauthorized` and a CI token
/// `forbidden` before it reads the request, answers an operator's `Get` and `Approve` of an auto
/// host (which is then allowed), and refuses a wrong key `key_mismatch` with the code the proto
/// names.
///
/// Catches: a CI token that can approve a host, and a refusal with another code than the proto's.
#[tokio::test]
async fn host_identity_over_grpc_takes_the_operator_alone() {
    let (h, w) = hosts("grpc-sid-hid-grpc").await;
    let get = || proto::GetHostIdentityRequest {
        node_id: "node-03".to_owned(),
    };
    let approve = |key: &HostKey| proto::ApproveHostIdentityRequest {
        node_id: "node-03".to_owned(),
        key_sha256: key.key_hash().to_hex(),
        reason: "racked".to_owned(),
    };
    for (ci, want) in [
        (false, (Code::Unauthenticated, "unauthorized")),
        (true, (Code::PermissionDenied, "forbidden")),
    ] {
        for path in [IDENTITY_GET, IDENTITY_APPROVE] {
            // A CI token is good once: each call is made with a new one.
            let bearer = ci.then(ci_token);
            let answer = if path == IDENTITY_GET {
                operator_call(&w, path, bearer.as_deref(), get()).await
            } else {
                operator_call(&w, path, bearer.as_deref(), approve(&h.auto.key)).await
            };
            assert_eq!(token(&answer), want, "{path}");
        }
    }

    let answer = operator_call(&w, IDENTITY_GET, Some(OPERATOR), get()).await;
    assert_eq!(token(&answer), (Code::Ok, ""));
    let read: proto::GetHostIdentityResponse = decode(&answer.2.expect("a message"));
    assert_eq!(
        (read.assurance, read.allowed),
        (proto::HostAssurance::Auto as i32, false)
    );

    let wrong = operator_call(
        &w,
        IDENTITY_APPROVE,
        Some(OPERATOR),
        approve(&HostKey::new()),
    )
    .await;
    assert_eq!(token(&wrong), (Code::FailedPrecondition, "key_mismatch"));
    let right = operator_call(&w, IDENTITY_APPROVE, Some(OPERATOR), approve(&h.auto.key)).await;
    assert_eq!(token(&right), (Code::Ok, ""));
    let done: proto::ApproveHostIdentityResponse = decode(&right.2.expect("a message"));
    assert_eq!(done.approval.expect("an approval").approver, "operator");
    let answer = operator_call(&w, IDENTITY_GET, Some(OPERATOR), get()).await;
    let read: proto::GetHostIdentityResponse = decode(&answer.2.expect("a message"));
    assert!(read.allowed);
    let missing = operator_call(
        &w,
        IDENTITY_GET,
        Some(OPERATOR),
        proto::GetHostIdentityRequest {
            node_id: "node-99".to_owned(),
        },
    )
    .await;
    assert_eq!(token(&missing), (Code::NotFound, "unknown_host"));
}

/// Proves: `HostIdentity` on the JSON gateway is the same call: `Get` answers canonical JSON, a
/// CI token is refused `{"code":"PERMISSION_DENIED","reason":"forbidden"}` with a 403, and an
/// `Approve` with a malformed key hash is a 400 `invalid_request`.
///
/// Catches: a service that the gateway does not reach or reaches with another status table.
#[tokio::test]
async fn host_identity_is_served_on_the_gateway() {
    let (h, w) = hosts("grpc-sid-hid-gateway").await;
    let post = |path: &'static str, bearer: String, body: Value| {
        let w = &w;
        async move {
            let request = Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {bearer}"))
                .body(Full::new(Bytes::from(body.to_string())))
                .expect("a request");
            let (status, _, bytes) = raw(&w.coordinator, Surface::Control, &None, request).await;
            (
                status,
                serde_json::from_slice::<Value>(&bytes).expect("JSON"),
            )
        }
    };
    let (status, body) = post(
        IDENTITY_GET,
        OPERATOR.to_owned(),
        json!({"nodeId": "node-03"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["assurance"], "HOST_ASSURANCE_AUTO");
    assert_eq!(body["keySha256"], h.auto.key.key_hash().to_hex());
    assert_eq!(body["nodeId"], "node-03");

    let (status, body) = post(IDENTITY_GET, ci_token(), json!({"nodeId": "node-03"})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        body,
        json!({"code": "PERMISSION_DENIED", "reason": "forbidden"})
    );

    let (status, body) = post(
        IDENTITY_APPROVE,
        OPERATOR.to_owned(),
        json!({"nodeId": "node-03", "keySha256": "xyz"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body,
        json!({"code": "INVALID_ARGUMENT", "reason": "invalid_request"})
    );
}

/// Proves: a deny row the coordinator cannot read makes `GET /v1/deny` a 500 `internal` or, when the
/// caller's own record cannot be read, a 403 `forbidden`: never a 200 with a short list; once the
/// store reads, the list is answered.
///
/// Catches: a failure of the store told as an empty list, which a relying party would read as
/// "no host is denied".
#[tokio::test]
async fn a_deny_list_the_store_cannot_read_is_never_a_short_list() {
    let fs = Arc::new(FaultFs::new());
    let enrollment = EnrollmentRig::over("grpc-sid-fault", fs.clone(), os_random());
    let certificate = enrolled(&enrollment.enrollment, &HostKey::new(), "machine-1", "s1");
    enrolled(&enrollment.enrollment, &HostKey::new(), "machine-2", "s2");
    enrollment
        .enrollment
        .store()
        .deny(&hostkeeper_enrollment::names::HostId::parse("s2").expect("a host id"))
        .expect("denied");
    let rig = coordinator_over("grpc-sid-fault-coordinator", &enrollment).await;
    let peer: Peer = Some(Arc::from(certificate.as_slice()));
    let mut seen = std::collections::BTreeSet::new();
    let mut failures = 0;
    loop {
        fs.fail_at(fs.count() + 1 + failures);
        let request = Request::builder()
            .method("GET")
            .uri("/v1/deny")
            .body(Full::new(Bytes::new()))
            .expect("a request");
        let answer = crate::http::handle(
            &rig.coordinator,
            Surface::Heartbeat,
            &peer,
            request,
            super::support::PATIENCE,
        )
        .await;
        let status = answer.status();
        if status == StatusCode::OK {
            let body = http_body_util::BodyExt::collect(answer.into_body())
                .await
                .expect("a body")
                .to_bytes();
            assert_eq!(body, Bytes::from_static(br#"{"hostIds":["s2"]}"#));
            break;
        }
        seen.insert(status);
        failures += 1;
        assert!(failures < 64, "the route keeps failing");
    }
    assert!(
        seen.contains(&StatusCode::INTERNAL_SERVER_ERROR),
        "{seen:?}"
    );
    assert!(
        seen.iter().all(
            |status| [StatusCode::INTERNAL_SERVER_ERROR, StatusCode::FORBIDDEN].contains(status)
        ),
        "{seen:?}"
    );
}

/// Proves: each value of `HostIdentityRefusal` becomes the status its proto comment names: the code
/// of the comment and the value's name, without its prefix and in lower case, as the whole message,
/// with no details; `UNSPECIFIED`, which no caller is refused with, is `INTERNAL`.
///
/// Catches: a value the enum gains that no code is chosen for.
#[test]
fn every_host_identity_refusal_has_the_code_and_token_its_proto_comment_names() {
    use hostkeeper_proto::hostkeeper::operator::v1::HostIdentityRefusal as R;
    use tonic::Code::{
        FailedPrecondition, Internal, InvalidArgument, NotFound, PermissionDenied, Unauthenticated,
        Unavailable,
    };

    let table = [
        (R::Unspecified, Internal, "unspecified"),
        (R::Unauthorized, Unauthenticated, "unauthorized"),
        (R::Forbidden, PermissionDenied, "forbidden"),
        (R::InvalidRequest, InvalidArgument, "invalid_request"),
        (R::UnknownHost, NotFound, "unknown_host"),
        (R::KeyMismatch, FailedPrecondition, "key_mismatch"),
        (R::Denied, FailedPrecondition, "denied"),
        (R::StoreFailed, Internal, "store_failed"),
        (R::AuditUnavailable, Unavailable, "audit_unavailable"),
    ];
    let values: Vec<R> = (0_i32..).map_while(|n| R::try_from(n).ok()).collect();
    assert_eq!(
        values,
        table.map(|(value, _, _)| value),
        "every value is named"
    );
    for (value, code, token) in table {
        let status = crate::grpc::refusal::host_identity(value);
        assert_eq!(
            (status.code(), status.message()),
            (code, token),
            "{value:?}"
        );
        assert!(status.details().is_empty());
    }
}
