//! `GrpcEnroll` against a loopback tonic server with TLS (127.0.0.1, port 0) whose `Coordinator`
//! service is a recording double (`server.rs`), with a test PKI made at run time. The enroller
//! runs over it with its real ports where the test is about what it sends.

use std::fmt;
use std::io;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use hostkeeper_config::daemon::Sha256Hex;
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_proto::hostkeeper::agent::v1::coordinator_server::Coordinator;
use hostkeeper_steps::SystemClock;
use hostkeeper_testkit::Scratch;
use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{DerSignature, VerifyingKey};
use p256::pkcs8::DecodePublicKey;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection};
use tonic::{Request, Status};

use super::server::{Call, Double, Loopback, NONCE, Pki, Seen, pki};
use super::{GrpcEnroll, convert, describe, trust_only_last};
use crate::claims::{ClaimsError, ClaimsSource};
use crate::enroller::{Enroller, Outcome, Ports, Settings};
use crate::key::{HostKey, OsEntropy};
use crate::store::{CurrentPaths, PkiDir, Store};
use crate::validity::SystemWallClock;
use crate::verify::sha256_hex;
use crate::wire::{
    Claims, CoordinatorEnroll, EnrollAnswer, EnrollRequest, EnrollState, HostId, Issued, Poll,
    PollAnswer, Reason, RenewAnswer, RenewRequest, RequestId, TransportError, poll_message,
};

/// Bounds the connections of the tests; no test waits for it.
const TIMEOUT: Duration = Duration::from_secs(30);

/// A scratch directory of the unit tests, beside the test binary in the target directory.
fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let base = exe
        .parent()
        .expect("its directory")
        .join("hostkeeper-enroll-unit");
    Scratch::under(&base, name).expect("a scratch directory")
}

fn write(dir: &Scratch, name: &str, contents: &[u8]) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, contents).expect("write a fixture file");
    path
}

/// A port nothing listens on.
fn dead_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("a listener")
        .local_addr()
        .expect("its address")
        .port()
}

/// Files that are never read.
fn nowhere() -> CurrentPaths {
    CurrentPaths {
        cert: PathBuf::from("cert.pem"),
        ca: PathBuf::from("ca.pem"),
        key: PathBuf::from("key"),
    }
}

/// A server that asks for a client certificate and serves a client without one, a client of it
/// whose current generation holds the PKI's host certificate (PEM) and key (PKCS#8 DER, as the
/// store keeps it), and the double behind the server.
struct Fixture {
    pki: Pki,
    double: Double,
    server: Loopback,
    grpc: GrpcEnroll,
    dir: Scratch,
}

fn fixture(name: &str) -> Fixture {
    let pki = pki();
    let double = Double::default();
    let server = Loopback::start(
        &pki.chain_pem,
        &pki.server_key_pem,
        Some(pki.ca_pem.as_str()),
        double.clone(),
    );
    let dir = scratch(name);
    let current = CurrentPaths {
        cert: write(&dir, "cert.pem", pki.client_cert_pem.as_bytes()),
        ca: write(&dir, "ca.pem", pki.ca_pem.as_bytes()),
        key: write(&dir, "key", &pki.client_key_der),
    };
    let grpc =
        GrpcEnroll::new(&format!("127.0.0.1:{}", server.port), current, TIMEOUT).expect("a client");
    Fixture {
        pki,
        double,
        server,
        grpc,
        dir,
    }
}

/// [`fixture`], with the CA of the server read.
fn probed(name: &str) -> Fixture {
    let mut f = fixture(name);
    assert_eq!(
        f.grpc.presented_ca_der().expect("the CA"),
        f.pki.ca_der,
        "the CA the chain ends in"
    );
    f
}

fn the_id() -> RequestId {
    RequestId::of_spki(b"a key")
}

fn host() -> HostId {
    HostId::new("web-1").expect("a host id")
}

fn full_request() -> EnrollRequest {
    EnrollRequest {
        csr_der: b"csr-1".to_vec(),
        claims: Claims {
            machine_id: Some("machine-1".to_owned()),
            product_serial: Some("serial-1".to_owned()),
            product_uuid: Some("uuid-1".to_owned()),
            network_addresses: vec![
                "02:00:00:00:00:01".to_owned(),
                "02:00:00:00:00:02".to_owned(),
            ],
            mac_serial: Some("mac-serial-1".to_owned()),
            platform_uuid: Some("platform-uuid-1".to_owned()),
            bmc_mac: Some("0c:c4:7a:00:00:01".to_owned()),
        },
        image: "image-1".to_owned(),
        renewal_of: Some(host()),
    }
}

fn bare_request() -> EnrollRequest {
    EnrollRequest {
        csr_der: b"csr-2".to_vec(),
        claims: Claims::default(),
        image: String::new(),
        renewal_of: None,
    }
}

fn unsigned() -> Poll {
    Poll {
        request_id: the_id(),
        nonce: Vec::new(),
        signature: Vec::new(),
    }
}

fn renew_request() -> RenewRequest {
    RenewRequest {
        host_id: host(),
        csr_der: b"csr-3".to_vec(),
    }
}

fn pb_enroll(id: &str, state: i32) -> Result<pb::EnrollResponse, Status> {
    Ok(pb::EnrollResponse {
        request_id: id.to_owned(),
        state,
    })
}

fn pb_poll(state: pb::PollState) -> pb::PollResponse {
    pb::PollResponse {
        state: state.into(),
        ..pb::PollResponse::default()
    }
}

/// The text of the transport error of a call that failed.
fn error_of<T: fmt::Debug>(result: Result<T, TransportError>) -> String {
    result.expect_err("a transport error").0
}

/// The requests the double received, without the certificates their clients presented.
fn seen(double: &Double) -> Vec<Seen> {
    double.calls().into_iter().map(|call| call.seen).collect()
}

/// The claims of the tests' host.
struct FixedClaims;

impl ClaimsSource for FixedClaims {
    fn claims(&self) -> Result<Claims, ClaimsError> {
        Ok(Claims {
            machine_id: Some("machine-1".to_owned()),
            ..Claims::default()
        })
    }
}

/// An enroller over a pki directory in `dir`, with a [`GrpcEnroll`] to the server on `port`, that
/// pins the CA whose DER is `pinned`.
fn open_enroller(dir: &Scratch, port: u16, pinned: &[u8]) -> Enroller {
    let store = PkiDir::new(&dir.path().join("pki"));
    let grpc = GrpcEnroll::new(&format!("127.0.0.1:{port}"), store.current_paths(), TIMEOUT)
        .expect("a client");
    let settings = Settings {
        ca_sha256: Sha256Hex::parse(&sha256_hex(pinned)).expect("a SHA-256"),
        image: "image-1".to_owned(),
    };
    let ports = Ports {
        coordinator: Box::new(grpc),
        store: Box::new(store),
        claims: Box::new(FixedClaims),
        entropy: Box::new(OsEntropy),
        wall: Box::new(SystemWallClock),
        clock: Box::new(SystemClock),
    };
    Enroller::open(settings, ports).expect("open the enroller")
}

/// Catches an enroller that sends a request to a coordinator whose chain ends in another CA than
/// the pinned one, or that makes the host's key first. Drives the real enroller over the real
/// `GrpcEnroll`: the error is `UntrustedCoordinator` naming the hash of the CA presented, and the
/// coordinator's recorder holds no call.
#[test]
fn grpc_enroll_posts_nothing_to_a_coordinator_whose_ca_hash_differs() {
    let pinned = pki();
    let presented = pki();
    let double = Double::default();
    let server = Loopback::start(
        &presented.chain_pem,
        &presented.server_key_pem,
        None,
        double.clone(),
    );
    let dir = scratch("untrusted");
    let mut enroller = open_enroller(&dir, server.port, &pinned.ca_der);
    let error = enroller.run_once().expect_err("an untrusted coordinator");
    assert_eq!(
        error.to_string(),
        format!(
            "the coordinator presents a CA whose SHA-256 is {}, not the pinned ca_sha256; nothing was sent",
            sha256_hex(&presented.ca_der)
        )
    );
    assert!(double.calls().is_empty(), "the coordinator was sent a call");
    drop(enroller);
    let store = PkiDir::new(&dir.path().join("pki"));
    assert_eq!(store.pending_key().expect("the pending key"), None);
}

/// Catches an enroller over `GrpcEnroll` that does not post, poll for a nonce and poll signed
/// once the coordinator's CA is the pinned one: the double receives the request with the
/// enroller's image and claims, an unsigned poll, then a poll carrying the nonce it gave and a
/// signature by the request key over the documented message, all without a client certificate.
#[test]
fn the_real_enroller_posts_and_polls_a_coordinator_of_the_pinned_ca() {
    let pki = pki();
    let double = Double::default();
    let server = Loopback::start(&pki.chain_pem, &pki.server_key_pem, None, double.clone());
    let dir = scratch("pinned");
    let mut enroller = open_enroller(&dir, server.port, &pki.ca_der);
    assert_eq!(enroller.run_once().expect("a step"), Outcome::Pending);
    drop(enroller);
    // The posted request is a function of the key, and so is the signature (RFC 6979): both are
    // rebuilt from the key the enroller kept.
    let kept = PkiDir::new(&dir.path().join("pki"))
        .pending_key()
        .expect("the pending key")
        .expect("a kept key");
    let key = HostKey::from_pkcs8(&kept).expect("a P-256 key");
    let id = key.request_id();
    assert_eq!(
        double.calls(),
        vec![
            Call {
                seen: Seen::Enroll(pb::EnrollRequest {
                    csr_der: key.csr_der().expect("a CSR"),
                    claims: Some(pb::Claims {
                        machine_id: "machine-1".to_owned(),
                        ..pb::Claims::default()
                    }),
                    image: "image-1".to_owned(),
                    renewal_of: String::new(),
                    mac_nonce: Vec::new(),
                }),
                client_certs: Vec::new(),
            },
            Call {
                seen: Seen::Poll(pb::PollRequest {
                    request_id: id.to_string(),
                    nonce: Vec::new(),
                    signature: Vec::new(),
                }),
                client_certs: Vec::new(),
            },
            Call {
                seen: Seen::Poll(pb::PollRequest {
                    request_id: id.to_string(),
                    nonce: NONCE.to_vec(),
                    signature: key.sign(&poll_message(&id, &NONCE)),
                }),
                client_certs: Vec::new(),
            },
        ]
    );
}

/// Catches a client certificate sent with `Enroll` or `Poll` (the host has none yet), or left out
/// of `Renew`: the double, which serves a client with no certificate too, sees none with the
/// first two and exactly the host certificate of the current generation with the third.
#[test]
fn enroll_and_poll_send_no_client_certificate_and_renew_sends_one() {
    let mut f = probed("client-certificates");
    f.double.script_enroll(pb_enroll(
        the_id().as_str(),
        pb::EnrollState::Pending.into(),
    ));
    f.grpc.enroll(&bare_request()).expect("an Enroll");
    f.grpc.poll(&unsigned()).expect("a Poll");
    f.grpc.renew(&renew_request()).expect("a Renew");
    let presented: Vec<Vec<Vec<u8>>> = f
        .double
        .calls()
        .into_iter()
        .map(|call| call.client_certs)
        .collect();
    assert_eq!(
        presented,
        vec![Vec::new(), Vec::new(), vec![f.pki.client_cert_der.clone()]]
    );
}

/// Catches a poll that does not follow the documented challenge: an unsigned poll must reach the
/// coordinator with an empty nonce and signature and be answered a nonce, and the signed poll
/// must carry that nonce and a signature over `poll_message` byte for byte.
#[test]
fn a_poll_round_trips_the_documented_challenge() {
    let mut f = probed("challenge");
    let key = HostKey::generate(&mut OsEntropy).expect("a key");
    let id = key.request_id();
    let challenge = Poll {
        request_id: id.clone(),
        ..unsigned()
    };
    assert_eq!(
        f.grpc.poll(&challenge).expect("a challenge"),
        PollAnswer::Challenge(NONCE.to_vec())
    );
    let signature = key.sign(&poll_message(&id, &NONCE));
    let signed = Poll {
        request_id: id.clone(),
        nonce: NONCE.to_vec(),
        signature: signature.clone(),
    };
    assert_eq!(f.grpc.poll(&signed).expect("pending"), PollAnswer::Pending);
    assert_eq!(
        seen(&f.double),
        vec![
            Seen::Poll(pb::PollRequest {
                request_id: id.to_string(),
                nonce: Vec::new(),
                signature: Vec::new(),
            }),
            Seen::Poll(pb::PollRequest {
                request_id: id.to_string(),
                nonce: NONCE.to_vec(),
                signature: signature.clone(),
            }),
        ]
    );
    // The bytes the contract fixes, written out here and not by the crate.
    let mut message = b"hostkeeper-enroll-poll-v1\x00".to_vec();
    message.extend_from_slice(id.as_str().as_bytes());
    message.push(0);
    message.extend_from_slice(&NONCE);
    VerifyingKey::from_public_key_der(key.spki_der())
        .expect("a P-256 key")
        .verify(
            &message,
            &DerSignature::from_bytes(&signature).expect("a DER signature"),
        )
        .expect("the signature is over the documented message");
}

/// Catches an `Enroll` request whose fields reach the coordinator changed (claims, image,
/// `renewal_of`, the absent claims as empty strings, no Mac nonce), and an answer of PENDING or
/// DUPLICATE_CLAIM not read as pending.
#[test]
fn an_enroll_request_goes_field_for_field_and_its_answer_is_read() {
    let mut f = probed("enroll");
    let id = the_id();
    for state in [pb::EnrollState::Pending, pb::EnrollState::DuplicateClaim] {
        f.double.script_enroll(pb_enroll(id.as_str(), state.into()));
    }
    for request in [full_request(), bare_request()] {
        assert_eq!(
            f.grpc.enroll(&request).expect("an answer"),
            EnrollAnswer {
                request_id: id.clone(),
                state: EnrollState::Pending,
            }
        );
    }
    assert_eq!(
        seen(&f.double),
        vec![
            Seen::Enroll(pb::EnrollRequest {
                csr_der: b"csr-1".to_vec(),
                claims: Some(pb::Claims {
                    machine_id: "machine-1".to_owned(),
                    dmi_serial: "serial-1".to_owned(),
                    product_uuid: "uuid-1".to_owned(),
                    addresses: vec![
                        "02:00:00:00:00:01".to_owned(),
                        "02:00:00:00:00:02".to_owned()
                    ],
                    mac_serial: "mac-serial-1".to_owned(),
                    platform_uuid: "platform-uuid-1".to_owned(),
                    bmc_mac: "0c:c4:7a:00:00:01".to_owned(),
                }),
                image: "image-1".to_owned(),
                renewal_of: "web-1".to_owned(),
                mac_nonce: Vec::new(),
            }),
            Seen::Enroll(pb::EnrollRequest {
                csr_der: b"csr-2".to_vec(),
                claims: Some(pb::Claims::default()),
                image: String::new(),
                renewal_of: String::new(),
                mac_nonce: Vec::new(),
            }),
        ]
    );
}

/// Catches an `Enroll` answer the port cannot represent (a request id that is not lower-case
/// hex, the UNSPECIFIED state, a state this daemon does not know) taken for an answer, or given a
/// default.
#[test]
fn an_enroll_answer_the_port_cannot_represent_is_a_transport_error() {
    let mut f = probed("enroll-unrepresentable");
    let id = the_id();
    let upper = id.to_string().to_uppercase();
    f.double.script_enroll(pb_enroll(&upper, 1));
    f.double.script_enroll(pb_enroll(id.as_str(), 0));
    f.double.script_enroll(pb_enroll(id.as_str(), 99));
    let request = bare_request();
    assert_eq!(
        error_of(f.grpc.enroll(&request)),
        format!("Enroll answered the request id {upper:?}, which is not 64 lower-case hex digits")
    );
    assert_eq!(
        error_of(f.grpc.enroll(&request)),
        "Enroll answered state 0, which this daemon does not know"
    );
    assert_eq!(
        error_of(f.grpc.enroll(&request)),
        "Enroll answered state 99, which this daemon does not know"
    );
}

/// Catches a `Poll` answer read as another state: PENDING is pending, DENIED is denied with the
/// reason `denied`, ISSUED carries the host id, the certificate as PEM of the DER the coordinator
/// sent and the bundle, and a state this daemon does not know is an error.
#[test]
fn poll_answers_are_read_into_the_ports_types() {
    let mut f = probed("poll-answers");
    f.double.script_poll(Ok(pb_poll(pb::PollState::Pending)));
    f.double.script_poll(Ok(pb_poll(pb::PollState::Denied)));
    f.double.script_poll(Ok(pb::PollResponse {
        certificate_der: f.pki.client_cert_der.clone(),
        host_id: "web-1".to_owned(),
        ca_bundle_pem: f.pki.ca_pem.clone(),
        ..pb_poll(pb::PollState::Issued)
    }));
    f.double.script_poll(Ok(pb::PollResponse {
        state: 7,
        ..pb::PollResponse::default()
    }));
    let poll = unsigned();
    assert_eq!(f.grpc.poll(&poll).expect("pending"), PollAnswer::Pending);
    assert_eq!(
        f.grpc.poll(&poll).expect("denied"),
        PollAnswer::Denied(Reason::new("denied").expect("a token"))
    );
    assert_eq!(
        f.grpc.poll(&poll).expect("issued"),
        PollAnswer::Issued(Issued {
            host_id: "web-1".to_owned(),
            cert_pem: f.pki.client_cert_pem.clone(),
            ca_bundle_pem: f.pki.ca_pem.clone(),
        })
    );
    assert_eq!(
        error_of(f.grpc.poll(&poll)),
        "Poll answered state 7, which this daemon does not know"
    );
}

/// Catches a refusal of `Enroll` or `Poll` (a gRPC status) lost or turned into an answer, and a
/// long message from the coordinator repeated whole in an error.
#[test]
fn a_status_answering_enroll_or_poll_is_a_transport_error_naming_it() {
    let mut f = probed("statuses");
    f.double
        .script_enroll(Err(Status::resource_exhausted("pending_full")));
    f.double
        .script_enroll(Err(Status::invalid_argument("x".repeat(100))));
    f.double
        .script_poll(Err(Status::not_found("unknown_request")));
    assert_eq!(
        error_of(f.grpc.enroll(&bare_request())),
        "Enroll was answered with the status ResourceExhausted: \"pending_full\""
    );
    assert_eq!(
        error_of(f.grpc.enroll(&bare_request())),
        format!(
            "Enroll was answered with the status InvalidArgument: {:?}",
            "x".repeat(64)
        )
    );
    assert_eq!(
        error_of(f.grpc.poll(&unsigned())),
        "Poll was answered with the status NotFound: \"unknown_request\""
    );
}

/// Catches a renewed certificate not handed on with the host id of the request and its PEM, a
/// refusal (a status whose message is one reason token) not carried as the reason, and any other
/// status taken for a refusal.
#[test]
fn renew_answers_are_read_into_the_ports_types() {
    let mut f = probed("renew-answers");
    f.double.script_renew(Ok(pb::RenewResponse {
        certificate_der: f.pki.client_cert_der.clone(),
        ca_bundle_pem: f.pki.ca_pem.clone(),
    }));
    f.double
        .script_renew(Err(Status::permission_denied("denied")));
    f.double
        .script_renew(Err(Status::unavailable("try again later")));
    let request = renew_request();
    assert_eq!(
        f.grpc.renew(&request).expect("issued"),
        RenewAnswer::Issued(Issued {
            host_id: "web-1".to_owned(),
            cert_pem: f.pki.client_cert_pem.clone(),
            ca_bundle_pem: f.pki.ca_pem.clone(),
        })
    );
    assert_eq!(
        f.grpc.renew(&request).expect("refused"),
        RenewAnswer::Refused(Reason::new("denied").expect("a token"))
    );
    assert_eq!(
        error_of(f.grpc.renew(&request)),
        "Renew was answered with the status Unavailable: \"try again later\""
    );
    assert_eq!(
        seen(&f.double)[0],
        Seen::Renew(pb::RenewRequest {
            csr_der: b"csr-3".to_vec()
        })
    );
}

/// Catches a call made before the coordinator's CA was read reaching the network: each of the
/// three is a transport error that says so, and the coordinator receives nothing.
#[test]
fn a_call_before_the_ca_was_read_sends_nothing() {
    let mut f = fixture("before-the-probe");
    let said = "the coordinator's CA has not been read: presented_ca_der comes first";
    assert_eq!(error_of(f.grpc.enroll(&bare_request())), said);
    assert_eq!(error_of(f.grpc.poll(&unsigned())), said);
    assert_eq!(error_of(f.grpc.renew(&renew_request())), said);
    assert!(f.double.calls().is_empty());
}

/// Catches the chain's last certificate not being what `presented_ca_der` returns: a coordinator
/// that presents its certificate and then the CA gives the CA, and one that presents its
/// certificate alone gives that certificate (whose hash is not the CA's).
#[test]
fn presented_ca_der_is_the_last_certificate_of_the_chain() {
    let pki = pki();
    for (chain, last) in [
        (&pki.chain_pem, &pki.ca_der),
        (&pki.leaf_pem, &pki.leaf_der),
    ] {
        let server = Loopback::start(chain, &pki.server_key_pem, None, Double::default());
        let mut grpc = GrpcEnroll::new(&format!("127.0.0.1:{}", server.port), nowhere(), TIMEOUT)
            .expect("a client");
        assert_eq!(grpc.presented_ca_der().expect("a chain"), *last);
    }
}

/// Catches a coordinator that cannot be reached reported as anything but a transport error that
/// names it, and the CA of an earlier probe still trusted after a probe that failed: the call
/// that follows must say the CA was not read, and one made with a CA at hand to a coordinator
/// that is down must say it could not connect.
#[test]
fn an_unreachable_coordinator_is_a_transport_error_and_clears_the_trust() {
    let port = dead_port();
    let pki = pki();
    let mut grpc =
        GrpcEnroll::new(&format!("127.0.0.1:{port}"), nowhere(), TIMEOUT).expect("a client");
    grpc.trusted = Some(
        trust_only_last(vec![CertificateDer::from(pki.ca_der.clone())])
            .expect("anchors")
            .1,
    );
    let said = error_of(grpc.enroll(&bare_request()));
    assert!(
        said.starts_with(&format!("cannot connect to 127.0.0.1:{port}: ")),
        "{said}"
    );
    let said = error_of(grpc.presented_ca_der());
    assert!(
        said.starts_with(&format!("cannot read the CA of 127.0.0.1:{port}: ")),
        "{said}"
    );
    assert!(grpc.trusted.is_none());
    assert_eq!(
        error_of(grpc.poll(&unsigned())),
        "the coordinator's CA has not been read: presented_ca_der comes first"
    );
}

/// Catches a peer that takes the connection and ends it without a TLS handshake being taken for
/// a coordinator: the probe fails, naming the coordinator.
#[test]
fn a_peer_that_does_not_speak_tls_fails_the_probe() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a listener");
    let port = listener.local_addr().expect("its address").port();
    let peer = std::thread::spawn(move || drop(listener.accept()));
    let mut grpc =
        GrpcEnroll::new(&format!("127.0.0.1:{port}"), nowhere(), TIMEOUT).expect("a client");
    let said = error_of(grpc.presented_ca_der());
    peer.join().expect("the peer ended");
    assert!(
        said.starts_with(&format!("cannot read the CA of 127.0.0.1:{port}: ")),
        "{said}"
    );
}

/// Catches a connection after the probe to a coordinator whose chain ends in a CA other than the
/// one read being made. The other CA has the same name as the one read and another key, as an
/// impostor's would: the handshake fails on the signature of the coordinator's certificate, and the
/// coordinator of the other CA receives nothing.
#[test]
fn a_new_connection_to_a_coordinator_of_another_ca_fails_the_handshake() {
    let read = probed("trusts-the-first-ca");
    let other = pki();
    let double = Double::default();
    let server = Loopback::start(
        &other.chain_pem,
        &other.server_key_pem,
        None,
        double.clone(),
    );
    let mut grpc = GrpcEnroll::new(&format!("127.0.0.1:{}", server.port), nowhere(), TIMEOUT)
        .expect("a client");
    assert_eq!(grpc.presented_ca_der().expect("a chain"), other.ca_der);
    grpc.trusted = read.grpc.trusted.clone();
    let said = error_of(grpc.enroll(&bare_request()));
    assert!(
        said.starts_with(&format!("cannot connect to 127.0.0.1:{}: ", server.port)),
        "{said}"
    );
    assert!(
        said.ends_with("invalid peer certificate: BadSignature"),
        "{said}"
    );
    assert!(double.calls().is_empty());
}

/// Catches a renewal that starts without the host's certificate or key, or with a key that is not
/// one: each is a transport error that names the file or the TLS setup, and the coordinator
/// receives nothing.
#[test]
fn a_renewal_without_a_usable_certificate_and_key_sends_nothing() {
    let mut f = probed("renew-files");
    let good = f.grpc.current.clone();
    let missing = f.dir.path().join("missing");
    let garbage = write(&f.dir, "garbage", b"not a key");
    f.grpc.current = CurrentPaths {
        cert: missing.clone(),
        ..good.clone()
    };
    assert_eq!(
        error_of(f.grpc.renew(&renew_request())),
        format!(
            "cannot read the host certificate {}: No such file or directory (os error 2)",
            missing.display()
        )
    );
    f.grpc.current = CurrentPaths {
        key: missing.clone(),
        ..good.clone()
    };
    assert_eq!(
        error_of(f.grpc.renew(&renew_request())),
        format!(
            "cannot read the host key {}: No such file or directory (os error 2)",
            missing.display()
        )
    );
    f.grpc.current = CurrentPaths {
        key: garbage,
        ..good
    };
    let said = error_of(f.grpc.renew(&renew_request()));
    assert!(
        said.starts_with(&format!(
            "cannot set up TLS for 127.0.0.1:{}: ",
            f.server.port
        )),
        "{said}"
    );
    assert!(f.double.calls().is_empty());
}

/// The message of the error for the endpoint `text`.
fn endpoint_error(text: &str) -> String {
    format!(
        "the coordinator endpoint {text:?} is not host:port, or an https:// URL of a host with an optional port, with a DNS name or an IP address as the host"
    )
}

/// Catches an endpoint taken that is not a host and a port (a bare host with no port, a scheme
/// other than https, a user, a path, a query, a space, a host that is not a name or an address)
/// and one that is refused: `host:port`, the same under a leading `https://`, a bracketed IPv6
/// address, and an `https://` URL with no port, which is port 443, with or without a final `/`.
#[test]
fn an_endpoint_is_a_host_and_a_port() {
    for text in [
        "",
        "has space:1",
        "127.0.0.1",
        "127.0.0.1:x",
        "http://127.0.0.1:1",
        "user@127.0.0.1:1",
        "127.0.0.1:1/path",
        "127.0.0.1:1?query",
        "-x:1",
        "https://127.0.0.1/path",
        "https://user@127.0.0.1",
    ] {
        let error = GrpcEnroll::new(text, nowhere(), TIMEOUT).expect_err(text);
        assert_eq!(error.to_string(), endpoint_error(text));
    }
    for (text, host, port, shown) in [
        ("127.0.0.1:8443", "127.0.0.1", 8443, "127.0.0.1:8443"),
        (
            "https://127.0.0.1:8443",
            "127.0.0.1",
            8443,
            "127.0.0.1:8443",
        ),
        ("[::1]:8443", "::1", 8443, "[::1]:8443"),
        ("localhost:1", "localhost", 1, "localhost:1"),
    ] {
        let grpc = GrpcEnroll::new(text, nowhere(), TIMEOUT).expect(text);
        let target = &grpc.target;
        assert_eq!(
            (target.host.as_str(), target.port, target.shown.as_str()),
            (host, port, shown),
            "{text}"
        );
        assert_eq!(target.uri.to_string(), format!("https://{shown}/"));
    }
    for text in [
        "https://coordinator.example",
        "https://coordinator.example/",
    ] {
        let grpc = GrpcEnroll::new(text, nowhere(), TIMEOUT).expect(text);
        let target = &grpc.target;
        assert_eq!(
            (target.host.as_str(), target.port, target.shown.as_str()),
            ("coordinator.example", 443, "coordinator.example:443"),
            "{text}"
        );
        assert_eq!(target.uri.to_string(), "https://coordinator.example/");
    }
}

/// Catches a runtime that could not be started going unreported, and the endpoint no longer
/// checked first.
#[test]
fn a_runtime_that_cannot_start_is_reported() {
    let error = GrpcEnroll::with_runtime(
        Err(io::Error::other("no threads")),
        "127.0.0.1:1",
        nowhere(),
        TIMEOUT,
    )
    .expect_err("no runtime");
    assert_eq!(
        error.to_string(),
        "cannot start the runtime of the coordinator client: no threads"
    );
    let error = GrpcEnroll::with_runtime(
        Err(io::Error::other("no threads")),
        "no port",
        nowhere(),
        TIMEOUT,
    )
    .expect_err("no endpoint");
    assert_eq!(error.to_string(), endpoint_error("no port"));
}

/// Catches a chain with no certificate, or whose last certificate is not a certificate, trusted:
/// both are errors, and a chain gives the DER of its last certificate and the anchors of it alone.
#[test]
fn only_the_last_certificate_of_a_usable_chain_is_trusted() {
    let pki = pki();
    assert_eq!(
        trust_only_last(Vec::new()).expect_err("no certificate").0,
        "the coordinator presented no certificate"
    );
    let said = trust_only_last(vec![CertificateDer::from(vec![1, 2, 3])])
        .expect_err("not a certificate")
        .0;
    assert!(
        said.starts_with(
            "the last certificate the coordinator presented is not a CA certificate this daemon can use: "
        ),
        "{said}"
    );
    let chain = vec![
        CertificateDer::from(pki.leaf_der.clone()),
        CertificateDer::from(pki.ca_der.clone()),
    ];
    let (der, anchors) = trust_only_last(chain).expect("a chain");
    assert_eq!((der, anchors.len()), (pki.ca_der.clone(), 1));
}

/// Catches an error shown without its causes, or with a cause that is missing: an error with none
/// is its own text, and one with a cause is `outer: inner`.
#[test]
fn an_error_is_shown_with_its_causes() {
    #[derive(Debug)]
    struct Outer(io::Error);

    impl fmt::Display for Outer {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("outer")
        }
    }

    impl std::error::Error for Outer {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }

    assert_eq!(describe(&io::Error::other("alone")), "alone");
    assert_eq!(describe(&Outer(io::Error::other("inner"))), "outer: inner");
}

/// Catches a reason that is not a token taken as one, with the coordinator's text repeated only
/// in part and quoted.
#[test]
fn a_reason_that_is_not_a_token_is_an_error() {
    assert_eq!(
        convert::reason("Poll", "Not A Token")
            .expect_err("not a token")
            .0,
        "Poll answered the reason \"Not A Token\", which is not a reason token"
    );
}

/// Catches a certificate or key that cannot be written as PEM being handed on in a default: the
/// error names the type and what PEM refused.
#[test]
fn a_pem_that_cannot_be_written_is_a_transport_error() {
    assert_eq!(
        convert::pem("BAD\nLABEL", b"der")
            .expect_err("a bad type label")
            .0,
        format!(
            "cannot write a \"BAD\\nLABEL\" as PEM: {}",
            x509_cert::der::pem::Error::Label
        )
    );
}

/// Catches a coordinator that speaks only TLS 1.2 not being read: its handshake signature is
/// accepted by the probe as a TLS 1.3 one is, and the chain it presented gives its CA.
#[test]
fn a_coordinator_that_speaks_only_tls_1_2_is_read() {
    let pki = pki();
    let chain = vec![
        CertificateDer::from(pki.leaf_der.clone()),
        CertificateDer::from(pki.ca_der.clone()),
    ];
    let key = PrivateKeyDer::from(PrivatePkcs8KeyDer::from(pki.server_key_der.clone()));
    let config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS12])
            .expect("TLS 1.2")
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .expect("a certificate and its key");
    let listener = TcpListener::bind("127.0.0.1:0").expect("a listener");
    let port = listener.local_addr().expect("its address").port();
    let config = Arc::new(config);
    let peer = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("a connection");
        let mut connection = ServerConnection::new(config).expect("a TLS connection");
        connection
            .complete_io(&mut stream)
            .expect("the handshake ends");
    });
    let mut grpc =
        GrpcEnroll::new(&format!("127.0.0.1:{port}"), nowhere(), TIMEOUT).expect("a client");
    assert_eq!(grpc.presented_ca_der().expect("a chain"), pki.ca_der);
    peer.join().expect("the peer ended");
}

/// Catches the double answering a heartbeat, which the coordinator of these tests does not
/// serve: the answer is the status that says so.
#[test]
fn the_double_serves_no_heartbeat() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a runtime");
    let status = runtime
        .block_on(Coordinator::heartbeat(
            &Double::default(),
            Request::new(pb::HeartbeatRequest::default()),
        ))
        .expect_err("no heartbeat");
    assert_eq!(status.message(), "this coordinator serves no heartbeat");
}
