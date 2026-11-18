//! `GrpcIssuer` against the loopback coordinator of `server.rs`: what it sends, which coordinator
//! it sends it to, and how each answer is read.

use std::path::PathBuf;
use std::time::Duration;

use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_testkit::Scratch;
use tonic::Status;

use super::server::{Double, Loopback, Pki, Seen, pki};
use super::{GrpcIssuer, convert};
use crate::service_identity::{IssueAnswer, ServiceCertIssuer};
use crate::store::CurrentPaths;
use crate::verify::sha256_hex;
use crate::wire::TransportError;

const TIMEOUT: Duration = Duration::from_secs(30);

struct Fixture {
    pki: Pki,
    double: Double,
    issuer: GrpcIssuer,
    _server: Loopback,
    _dir: Scratch,
}

/// An issuer for a loopback coordinator that asks for a client certificate, pinned to the SHA-256
/// `pin` of the CA when given, else of the coordinator's own.
fn fixture(name: &str, pin: Option<&str>) -> Fixture {
    let pki = pki();
    let double = Double::default();
    let server = Loopback::start(
        &pki.chain_pem,
        &pki.server_key_pem,
        Some(pki.ca_pem.as_str()),
        double.clone(),
    );
    let exe = std::env::current_exe().expect("the test binary");
    let base = exe
        .parent()
        .expect("its directory")
        .join("hostkeeper-enroll-unit");
    let dir = Scratch::under(&base, name).expect("a scratch directory");
    let write = |file: &str, bytes: &[u8]| -> PathBuf {
        let path = dir.path().join(file);
        std::fs::write(&path, bytes).expect("write a fixture file");
        path
    };
    let current = CurrentPaths {
        cert: write("cert.pem", pki.client_cert_pem.as_bytes()),
        ca: write("ca.pem", pki.ca_pem.as_bytes()),
        key: write("key", &pki.client_key_der),
    };
    let pin = pin.map_or_else(|| sha256_hex(&pki.ca_der), str::to_owned);
    let issuer = GrpcIssuer::new(
        &format!("127.0.0.1:{}", server.port),
        current,
        TIMEOUT,
        &pin,
    )
    .expect("an issuer");
    Fixture {
        pki,
        double,
        issuer,
        _server: server,
        _dir: dir,
    }
}

/// Catches the request not carrying the service and the CSR byte for byte, or being sent without
/// the host's certificate: the double sees the request and exactly the host certificate.
#[test]
fn an_issue_carries_the_service_and_csr_over_the_hosts_certificate() {
    let mut f = fixture("issue-request", None);
    f.double
        .script_issue(Ok(pb::IssueServiceCertificateResponse {
            certificate_der: b"a certificate".to_vec(),
            ca_bundle_pem: "a bundle".to_owned(),
        }));
    let answer = f.issuer.issue("js-agent", b"a csr").expect("an answer");
    assert_eq!(
        answer,
        IssueAnswer::Issued {
            cert_der: b"a certificate".to_vec(),
            ca_bundle_pem: "a bundle".to_owned(),
        }
    );
    let calls = f.double.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].seen,
        Seen::Issue(pb::IssueServiceCertificateRequest {
            service: "js-agent".to_owned(),
            csr_der: b"a csr".to_vec(),
        })
    );
    assert_eq!(calls[0].client_certs, vec![f.pki.client_cert_der.clone()]);
}

/// Catches a request sent to a coordinator whose CA is not the pinned one: the issuer errors,
/// naming the CA it saw, and the double sees no call.
#[test]
fn an_issue_to_a_coordinator_of_another_ca_sends_nothing() {
    let pin = "0".repeat(64);
    let mut f = fixture("issue-pin", Some(&pin));
    let seen = sha256_hex(&f.pki.ca_der);
    let error = f.issuer.issue("js-agent", b"a csr").expect_err("refused");
    assert_eq!(
        error,
        TransportError(format!(
            "the coordinator presents a CA whose SHA-256 is {seen}, not the pinned ca_sha256; nothing was sent"
        ))
    );
    assert_eq!(f.double.calls(), []);
}

/// Catches the answers being misread: a refusal is its token, a coordinator without the call is
/// `Unimplemented` (the double's own answer when nothing is scripted), a status that is no
/// token is a transport error that names the call.
#[test]
fn the_answers_of_the_coordinator_are_read_into_the_ports_types() {
    let mut f = fixture("issue-answers", None);
    f.double
        .script_issue(Err(Status::failed_precondition("service_not_assigned")));
    f.double
        .script_issue(Err(Status::permission_denied("not a token!")));
    assert_eq!(
        f.issuer.issue("js-agent", b"c"),
        Ok(IssueAnswer::Refused(
            crate::wire::Reason::new("service_not_assigned").expect("a token")
        ))
    );
    assert_eq!(
        f.issuer.issue("js-agent", b"c"),
        Err(TransportError(
            "IssueServiceCertificate was answered with the status PermissionDenied: \"not a token!\""
                .to_owned()
        ))
    );
    assert_eq!(
        f.issuer.issue("js-agent", b"c"),
        Ok(IssueAnswer::Unimplemented)
    );
}

/// Catches a coordinator that cannot be reached being taken for a refusal: it is a transport
/// error, so the keeper retries and reports it.
#[test]
fn an_unreachable_coordinator_is_a_transport_error() {
    let mut issuer = GrpcIssuer::new(
        "127.0.0.1:1",
        CurrentPaths {
            cert: PathBuf::from("cert.pem"),
            ca: PathBuf::from("ca.pem"),
            key: PathBuf::from("key"),
        },
        Duration::from_secs(5),
        &"0".repeat(64),
    )
    .expect("an issuer");
    let error = issuer.issue("js-agent", b"c").expect_err("no coordinator");
    assert!(
        error.0.starts_with("cannot read the CA of 127.0.0.1:1: "),
        "{error}"
    );
}

/// Catches an endpoint the client cannot use being accepted.
#[test]
fn an_endpoint_that_is_not_one_is_refused() {
    let error = GrpcIssuer::new(
        "not an endpoint",
        CurrentPaths {
            cert: PathBuf::from("cert.pem"),
            ca: PathBuf::from("ca.pem"),
            key: PathBuf::from("key"),
        },
        TIMEOUT,
        &"0".repeat(64),
    )
    .expect_err("refused");
    assert_eq!(
        error.to_string(),
        "the coordinator endpoint \"not an endpoint\" is not host:port, or an https:// URL of a host with an optional port, with a DNS name or an IP address as the host"
    );
}

/// Catches the conversion of a request changing a field.
#[test]
fn the_issue_request_has_the_service_and_the_csr() {
    assert_eq!(
        convert::issue_request("svc", b"csr"),
        pb::IssueServiceCertificateRequest {
            service: "svc".to_owned(),
            csr_der: b"csr".to_vec(),
        }
    );
}
