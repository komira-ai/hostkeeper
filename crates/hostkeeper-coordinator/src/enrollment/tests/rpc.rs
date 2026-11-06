//! `Enroll` and `Poll`: who may call them, what each answers, the signed nonce, and the calls
//! that fail inside the coordinator.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use bytes::Bytes;
use hostkeeper_enrollment::fs::FaultFs;
use hostkeeper_enrollment::names::KeyHash;
use hostkeeper_proto::hostkeeper::agent::v1::{
    CoordinatorRefusal, EnrollRequest, EnrollResponse, EnrollState, PollRequest, PollResponse,
    PollState,
};
use hostkeeper_wire::wire::HEARTBEAT_PATH;
use http_body_util::{BodyExt as _, Full};
use hyper::{Request, StatusCode};
use prost::Message;
use x509_parser::prelude::{FromDer, X509Certificate};

use super::support::{
    HostKey, PATIENCE, Rig, approve, body_of, coordinator_over, enroll_request, enrolled,
    signed_poll, watched,
};
use crate::enrollment::{MAX_MESSAGE_BYTES, Method, NONCE_BYTES, RpcError, call, os_random};
use crate::http::{Surface, handle};

fn refused(refusal: CoordinatorRefusal) -> RpcError {
    RpcError::Refused(refusal)
}

/// Catches `Enroll` or `Poll` refusing a caller with no client certificate, or `Heartbeat` or
/// `Renew` accepting one, or reading its body before refusing it.
#[tokio::test]
async fn enroll_and_poll_work_without_a_client_certificate_and_heartbeat_does_not() {
    let enrollment = Rig::new("rpc-no-certificate");
    let rig = coordinator_over("rpc-no-certificate-coordinator", &enrollment).await;
    let key = HostKey::new();

    let answer = call(
        &enrollment.enrollment,
        Method::Enroll,
        &None,
        body_of(&enroll_request(&key, "machine-1")),
        PATIENCE,
    )
    .await
    .expect("Enroll needs no certificate");
    let enrolled = EnrollResponse::decode(answer.as_slice()).expect("an EnrollResponse");
    assert_eq!(enrolled.request_id, key.key_hash().to_hex());
    assert_eq!(enrolled.state, EnrollState::Pending as i32);

    let poll = PollRequest {
        request_id: enrolled.request_id,
        ..PollRequest::default()
    };
    let answer = call(
        &enrollment.enrollment,
        Method::Poll,
        &None,
        body_of(&poll),
        PATIENCE,
    )
    .await
    .expect("Poll needs no certificate");
    let challenge = PollResponse::decode(answer.as_slice()).expect("a PollResponse");
    assert_eq!(challenge.state, PollState::Unspecified as i32);
    assert_eq!(challenge.nonce.len(), NONCE_BYTES);
    assert!(challenge.certificate_der.is_empty() && challenge.host_id.is_empty());

    let (body, read) = watched();
    assert_eq!(
        call(&enrollment.enrollment, Method::Renew, &None, body, PATIENCE).await,
        Err(refused(CoordinatorRefusal::Forbidden))
    );
    assert!(!read.load(Ordering::SeqCst), "a Renew's body was read");

    let (body, read) = watched();
    let request = Request::post(HEARTBEAT_PATH).body(body).expect("a request");
    let response = handle(
        &rig.coordinator,
        Surface::Heartbeat,
        &None,
        request,
        PATIENCE,
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let text = response
        .into_body()
        .collect()
        .await
        .expect("a body")
        .to_bytes();
    assert_eq!(text, Bytes::from_static(br#"{"error":"forbidden"}"#));
    assert!(!read.load(Ordering::SeqCst), "a heartbeat's body was read");
}

/// Catches a call that answers before it reads its body, a body over the limit or not of the
/// method's message being read as one, and an answer that is not the method's message encoded.
#[tokio::test]
async fn a_call_reads_and_decodes_its_message_within_the_limit() {
    let enrollment = Rig::new("rpc-call");
    let (body, read) = watched();
    assert_eq!(
        call(
            &enrollment.enrollment,
            Method::Enroll,
            &None,
            body,
            PATIENCE
        )
        .await,
        Err(refused(CoordinatorRefusal::BadCsr)),
        "an empty Enroll has no CSR"
    );
    assert!(read.load(Ordering::SeqCst), "the body was not read");

    let garbage = Full::new(Bytes::from_static(&[0xff, 0xff, 0xff]));
    assert_eq!(
        call(
            &enrollment.enrollment,
            Method::Poll,
            &None,
            garbage,
            PATIENCE
        )
        .await,
        Err(RpcError::Malformed)
    );
    let large = EnrollRequest {
        image: "i".repeat(MAX_MESSAGE_BYTES),
        ..EnrollRequest::default()
    };
    assert_eq!(
        call(
            &enrollment.enrollment,
            Method::Enroll,
            &None,
            body_of(&large),
            PATIENCE
        )
        .await,
        Err(RpcError::Malformed)
    );
    assert_eq!(
        Method::ALL.map(Method::path),
        [
            "/hostkeeper.agent.v1.Coordinator/Enroll",
            "/hostkeeper.agent.v1.Coordinator/Poll",
            "/hostkeeper.agent.v1.Coordinator/Renew",
        ]
    );
}

/// Catches `Enroll` taking the request id from the client, a repost making a second request,
/// a second request for one machine id not flagged, and a request already issued answered as
/// pending.
#[test]
fn enroll_answers_pending_a_duplicate_claim_or_where_a_known_request_stands() {
    let rig = Rig::new("rpc-enroll");
    let enrollment = &rig.enrollment;
    let key = HostKey::new();
    let first = enrollment
        .enroll(enroll_request(&key, "machine-1"))
        .expect("enrolled");
    assert_eq!(first.request_id, key.key_hash().to_hex());
    assert_eq!(first.state, EnrollState::Pending as i32);
    let again = enrollment
        .enroll(enroll_request(&key, "machine-1"))
        .expect("reposted");
    assert_eq!(again, first);
    assert_eq!(enrollment.store().pending().expect("pending").len(), 1);

    let clone = HostKey::new();
    let second = enrollment
        .enroll(enroll_request(&clone, "machine-1"))
        .expect("enrolled");
    assert_eq!(second.state, EnrollState::DuplicateClaim as i32);

    let issued = HostKey::new();
    enrolled(enrollment, &issued, "machine-2", "s2");
    let reposted = enrollment
        .enroll(enroll_request(&issued, "machine-2"))
        .expect("reposted");
    assert_eq!(reposted.state, EnrollState::Unspecified as i32);
}

/// Catches a CSR that is not one, a key that is not P-256, or a `renewal_of` the coordinator
/// cannot allow (not a host id, or no host it knows) being recorded as a request.
#[test]
fn enroll_refuses_a_bad_csr_a_foreign_key_and_a_renewal_it_cannot_allow() {
    let rig = Rig::new("rpc-enroll-refusals");
    let enrollment = &rig.enrollment;
    let key = HostKey::new();
    let bad_csr = EnrollRequest {
        csr_der: b"not a CSR".to_vec(),
        ..enroll_request(&key, "machine-1")
    };
    assert_eq!(
        enrollment.enroll(bad_csr),
        Err(refused(CoordinatorRefusal::BadCsr))
    );
    let p384 = HostKey::of(&rcgen::PKCS_ECDSA_P384_SHA384);
    assert_eq!(
        enrollment.enroll(enroll_request(&p384, "machine-1")),
        Err(refused(CoordinatorRefusal::KeyNotP256))
    );
    for renewal_of in ["Not_An_Id", "nobody"] {
        let request = EnrollRequest {
            renewal_of: renewal_of.to_owned(),
            ..enroll_request(&key, "machine-1")
        };
        assert_eq!(
            enrollment.enroll(request),
            Err(refused(CoordinatorRefusal::RenewalNotAllowed)),
            "{renewal_of}"
        );
    }
    assert_eq!(enrollment.store().pending().expect("pending"), Vec::new());
}

/// Catches a poll answered before approval as anything but pending, one after approval without
/// the certificate, the host id or the CA bundle, a second poll minting another certificate,
/// and a denied request answered as pending.
#[test]
fn a_poll_answers_where_the_request_stands_and_after_issue_the_same_certificate() {
    let rig = Rig::new("rpc-poll-states");
    let enrollment = &rig.enrollment;
    let key = HostKey::new();
    enrollment
        .enroll(enroll_request(&key, "machine-1"))
        .expect("enrolled");
    let pending = signed_poll(enrollment, &key, &key).expect("polled");
    assert_eq!(pending.state, PollState::Pending as i32);
    assert_eq!(pending.nonce.len(), NONCE_BYTES);
    assert!(pending.certificate_der.is_empty());

    approve(enrollment, &key, "s1", false);
    let issued = signed_poll(enrollment, &key, &key).expect("polled");
    assert_eq!(issued.state, PollState::Issued as i32);
    assert_eq!(issued.host_id, "s1");
    let (_, certificate) =
        X509Certificate::from_der(&issued.certificate_der).expect("a certificate");
    assert_eq!(certificate.subject().to_string(), "CN=host:s1");
    let (_, ca) =
        x509_parser::pem::parse_x509_pem(issued.ca_bundle_pem.as_bytes()).expect("a PEM bundle");
    assert_eq!(ca.label, "CERTIFICATE");
    assert_eq!(ca.contents, enrollment.ca_bundle().certificate_der());

    let again = signed_poll(enrollment, &key, &key).expect("polled again");
    assert_eq!(again.certificate_der, issued.certificate_der);
    assert_ne!(again.nonce, issued.nonce);

    enrollment
        .store()
        .deny(&super::support::host_id("s1"))
        .expect("denied");
    let denied = signed_poll(enrollment, &key, &key).expect("polled");
    assert_eq!(denied.state, PollState::Denied as i32);
    assert!(denied.certificate_der.is_empty());
}

/// Catches a poll signed by another key accepted.
#[test]
fn a_poll_signed_by_another_key_is_refused() {
    let rig = Rig::new("rpc-poll-other-key");
    let enrollment = &rig.enrollment;
    let key = HostKey::new();
    enrollment
        .enroll(enroll_request(&key, "machine-1"))
        .expect("enrolled");
    approve(enrollment, &key, "s1", false);
    assert_eq!(
        signed_poll(enrollment, &key, &HostKey::new()),
        Err(refused(CoordinatorRefusal::BadSignature))
    );
}

/// Catches a nonce answered twice, a nonce given for another request accepted, a signed poll
/// with no nonce or no signature accepted, and a poll for a request id that is unknown or not
/// one being answered with a nonce.
#[test]
fn a_nonce_is_good_for_one_poll_of_its_own_request() {
    let rig = Rig::new("rpc-poll-nonce");
    let enrollment = &rig.enrollment;
    let (key, other) = (HostKey::new(), HostKey::new());
    for (one, machine) in [(&key, "machine-1"), (&other, "machine-2")] {
        enrollment
            .enroll(enroll_request(one, machine))
            .expect("enrolled");
    }
    let unsigned = |key: &HostKey| PollRequest {
        request_id: key.key_hash().to_hex(),
        ..PollRequest::default()
    };
    let nonce = enrollment.poll(unsigned(&key)).expect("a nonce").nonce;
    let message = hostkeeper_enrollment::enroll::poll_message(&key.key_hash(), &nonce);
    let signed = PollRequest {
        signature: key.sign(&message),
        nonce,
        ..unsigned(&key)
    };
    assert!(enrollment.poll(signed.clone()).is_ok());
    assert_eq!(
        enrollment.poll(signed.clone()),
        Err(refused(CoordinatorRefusal::BadSignature)),
        "a nonce answered twice"
    );

    let theirs = enrollment.poll(unsigned(&other)).expect("a nonce").nonce;
    let message = hostkeeper_enrollment::enroll::poll_message(&key.key_hash(), &theirs);
    let borrowed = PollRequest {
        signature: key.sign(&message),
        nonce: theirs,
        ..unsigned(&key)
    };
    assert_eq!(
        enrollment.poll(borrowed),
        Err(refused(CoordinatorRefusal::BadSignature))
    );

    let no_nonce = PollRequest {
        nonce: Vec::new(),
        ..signed
    };
    assert_eq!(
        enrollment.poll(no_nonce),
        Err(refused(CoordinatorRefusal::BadSignature))
    );
    let fresh = enrollment.poll(unsigned(&key)).expect("a nonce").nonce;
    let no_signature = PollRequest {
        nonce: fresh,
        ..unsigned(&key)
    };
    assert_eq!(
        enrollment.poll(no_signature),
        Err(refused(CoordinatorRefusal::BadSignature))
    );

    let unknown = KeyHash::of_spki(b"no such key").to_hex();
    let stranger = PollRequest {
        request_id: unknown.clone(),
        nonce: vec![1; NONCE_BYTES],
        signature: vec![1],
    };
    assert_eq!(
        enrollment.poll(stranger),
        Err(refused(CoordinatorRefusal::BadSignature)),
        "a nonce never given"
    );
    for request_id in [unknown.as_str(), "not a request id"] {
        let poll = PollRequest {
            request_id: request_id.to_owned(),
            ..PollRequest::default()
        };
        assert_eq!(
            enrollment.poll(poll),
            Err(refused(CoordinatorRefusal::UnknownRequest)),
            "{request_id}"
        );
    }
}

/// Catches a failure inside the coordinator (no random bytes, an unreadable request file, a
/// store that cannot issue) answered as a refusal or as anything but an internal error.
#[test]
fn a_poll_the_coordinator_cannot_answer_is_an_internal_error() {
    let no_random = Rig::over(
        "rpc-poll-no-random",
        Arc::new(hostkeeper_enrollment::fs::RealFs),
        Box::new(|_: &mut [u8]| false),
    );
    let key = HostKey::new();
    no_random
        .enrollment
        .enroll(enroll_request(&key, "machine-1"))
        .expect("enrolled");
    let unsigned = PollRequest {
        request_id: key.key_hash().to_hex(),
        ..PollRequest::default()
    };
    assert_eq!(
        no_random.enrollment.poll(unsigned.clone()),
        Err(RpcError::Internal)
    );

    let broken = Rig::new("rpc-poll-broken-request");
    broken
        .enrollment
        .enroll(enroll_request(&key, "machine-1"))
        .expect("enrolled");
    let path = broken
        .files
        .store
        .join(format!("requests/{}.pb", key.key_hash().to_hex()));
    std::fs::write(&path, b"\x08\x02").expect("a newer schema");
    assert_eq!(
        broken.enrollment.poll(unsigned.clone()),
        Err(RpcError::Internal)
    );

    let fs = Arc::new(FaultFs::new());
    let faulty = Rig::over("rpc-poll-fault", fs.clone(), os_random());
    faulty
        .enrollment
        .enroll(enroll_request(&key, "machine-1"))
        .expect("enrolled");
    approve(&faulty.enrollment, &key, "s1", false);
    let nonce = faulty
        .enrollment
        .poll(unsigned.clone())
        .expect("a nonce")
        .nonce;
    let message = hostkeeper_enrollment::enroll::poll_message(&key.key_hash(), &nonce);
    // The signed poll reads the request (1), reads it again to tell a first issue for the audit
    // (2), then issuing takes the store's lock (3): it fails.
    fs.fail_at(fs.count() + 3);
    let signed = PollRequest {
        signature: key.sign(&message),
        nonce,
        ..unsigned
    };
    assert_eq!(faulty.enrollment.poll(signed), Err(RpcError::Internal));
}
