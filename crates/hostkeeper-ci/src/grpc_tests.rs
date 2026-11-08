//! The real Releases client against a loopback gRPC server with TLS (127.0.0.1, port 0), whose
//! certificates are made at run time.

use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hostkeeper_proto::hostkeeper::operator::v1::releases_server::{
    Releases as ReleasesService, ReleasesServer,
};
use hostkeeper_proto::hostkeeper::operator::v1::{
    ClearHaltRequest, ClearHaltResponse, ConvergeRequest, ConvergeResponse, Convergence,
    ConvergenceState, DesiredState, ForceRequest, ForceResponse, GetReleaseRequest,
    GetReleaseResponse, HaltRequest, HaltResponse, MarkBadRequest, MarkBadResponse, RoleCounts,
    SetDesiredRequest, SetDesiredResponse, StatusRequest, StatusResponse, UnmarkBadRequest,
    UnmarkBadResponse, WaitConvergedRequest, WaitConvergedResponse,
};
use tonic::transport::{Identity, Server, ServerTlsConfig};
use tonic::{Code, Request, Response, Status};

use crate::flow::execute;
use crate::grpc::{Failure, Releases, Target, Tonic};
use crate::ports::Ports;
use crate::testing::{Fake, flags, scratch, settings, stored};
use crate::token::TokenError;

/// A CA, and a server certificate it signed for `localhost` and `127.0.0.1`.
struct Pki {
    ca_pem: String,
    server_cert: String,
    server_key: String,
}

fn pki() -> Pki {
    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("CA params");
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let ca_key = rcgen::KeyPair::generate().expect("a CA key");
    let ca = rcgen::CertifiedIssuer::self_signed(ca_params, ca_key).expect("a CA");
    let server_key = rcgen::KeyPair::generate().expect("a server key");
    let server =
        rcgen::CertificateParams::new(vec!["localhost".to_owned(), "127.0.0.1".to_owned()])
            .expect("server params")
            .signed_by(&server_key, &*ca)
            .expect("a server certificate");
    Pki {
        ca_pem: ca.pem(),
        server_cert: server.pem(),
        server_key: server_key.serialize_pem(),
    }
}

/// What the server saw.
#[derive(Default)]
struct Seen {
    /// The `authorization` metadata of each call, in order.
    bearers: Vec<String>,
    sets: Vec<SetDesiredRequest>,
    converges: Vec<ConvergeRequest>,
}

/// A Releases service that stores the version of a `SetDesired` and answers a `Converge` with a
/// converged service, except for the service `refused`, whose `SetDesired` is refused
/// FAILED_PRECONDITION `held_bad`. Its other methods are not implemented.
#[derive(Clone)]
struct Probe {
    seen: Arc<Mutex<Seen>>,
}

impl Probe {
    fn note<T>(&self, request: &Request<T>) {
        let bearer = request
            .metadata()
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("none")
            .to_owned();
        self.seen.lock().expect("lock").bearers.push(bearer);
    }
}

fn unimplemented<T>() -> Result<Response<T>, Status> {
    Err(Status::unimplemented("not part of the test"))
}

#[tonic::async_trait]
impl ReleasesService for Probe {
    async fn set_desired(
        &self,
        request: Request<SetDesiredRequest>,
    ) -> Result<Response<SetDesiredResponse>, Status> {
        self.note(&request);
        let request = request.into_inner();
        self.seen.lock().expect("lock").sets.push(request.clone());
        if request.service == "refused" {
            return Err(Status::failed_precondition("held_bad"));
        }
        Ok(Response::new(SetDesiredResponse {
            desired: request.desired,
            ..SetDesiredResponse::default()
        }))
    }

    async fn converge(
        &self,
        request: Request<ConvergeRequest>,
    ) -> Result<Response<ConvergeResponse>, Status> {
        self.note(&request);
        let request = request.into_inner();
        self.seen.lock().expect("lock").converges.push(request);
        Ok(Response::new(ConvergeResponse {
            convergence: Some(Convergence {
                desired: Some("1.2.3".to_owned()),
                state: ConvergenceState::Converged as i32,
                roles: vec![RoleCounts {
                    role: "web".to_owned(),
                    at_desired: 2,
                    ..RoleCounts::default()
                }],
                ..Convergence::default()
            }),
        }))
    }

    async fn force(&self, _: Request<ForceRequest>) -> Result<Response<ForceResponse>, Status> {
        unimplemented()
    }

    async fn unmark_bad(
        &self,
        _: Request<UnmarkBadRequest>,
    ) -> Result<Response<UnmarkBadResponse>, Status> {
        unimplemented()
    }

    async fn clear_halt(
        &self,
        _: Request<ClearHaltRequest>,
    ) -> Result<Response<ClearHaltResponse>, Status> {
        unimplemented()
    }

    async fn status(&self, _: Request<StatusRequest>) -> Result<Response<StatusResponse>, Status> {
        unimplemented()
    }

    async fn mark_bad(
        &self,
        _: Request<MarkBadRequest>,
    ) -> Result<Response<MarkBadResponse>, Status> {
        unimplemented()
    }

    async fn halt(&self, _: Request<HaltRequest>) -> Result<Response<HaltResponse>, Status> {
        unimplemented()
    }

    async fn wait_converged(
        &self,
        _: Request<WaitConvergedRequest>,
    ) -> Result<Response<WaitConvergedResponse>, Status> {
        unimplemented()
    }

    async fn get_release(
        &self,
        _: Request<GetReleaseRequest>,
    ) -> Result<Response<GetReleaseResponse>, Status> {
        unimplemented()
    }
}

/// A loopback server serving [`Probe`] with the server certificate of a [`Pki`]; it stops when
/// dropped.
struct Loopback {
    _runtime: tokio::runtime::Runtime,
    port: u16,
    seen: Arc<Mutex<Seen>>,
}

impl Loopback {
    fn start(pki: &Pki) -> Loopback {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("a runtime");
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .expect("a loopback listener");
        let port = listener.local_addr().expect("its address").port();
        let seen = Arc::new(Mutex::new(Seen::default()));
        let tls =
            ServerTlsConfig::new().identity(Identity::from_pem(&pki.server_cert, &pki.server_key));
        let server = Server::builder()
            .tls_config(tls)
            .expect("TLS")
            .serve_with_incoming(
                ReleasesServer::new(Probe {
                    seen: Arc::clone(&seen),
                }),
                tokio_stream::wrappers::TcpListenerStream::new(listener),
            );
        runtime.spawn(server);
        Loopback {
            _runtime: runtime,
            port,
            seen,
        }
    }
}

/// The target `host` of `server`, with the CA of `pki` written in a file of the scratch
/// directory `dir`.
fn target(dir: &std::path::Path, pki: &Pki, host: &str, server: &Loopback) -> Target {
    let ca_file = dir.join("ca.pem");
    std::fs::write(&ca_file, &pki.ca_pem).expect("write the CA");
    Target {
        endpoint: format!("https://{host}:{}", server.port),
        ca_file,
    }
}

fn request(service: &str) -> SetDesiredRequest {
    SetDesiredRequest {
        service: service.to_owned(),
        desired: Some(DesiredState {
            version: "1.2.3".to_owned(),
            ..DesiredState::default()
        }),
        package_digest: Some("a".repeat(64)),
        ..SetDesiredRequest::default()
    }
}

/// Proves: a `SetDesired` and a `Converge` reach the server over TLS, by its DNS name and by its
/// IP address, with the token as `Bearer <token>` and every field of the typed request, and the
/// replies come back typed.
///
/// Catches: a call sent without the token or with it spelled otherwise, a request that loses a
/// field, a client that trusts another root than the CA file or checks another name than the
/// endpoint's host.
#[test]
fn calls_reach_the_server_over_tls_with_the_bearer_and_the_typed_request() {
    let pki = pki();
    let server = Loopback::start(&pki);
    let dir = scratch("grpc-calls");
    for host in ["localhost", "127.0.0.1"] {
        let target = target(dir.path(), &pki, host, &server);
        let client = Tonic::new();
        let set = client
            .set_desired(&target, "tok-1", request("svc"))
            .unwrap_or_else(|e| panic!("{host}: {e:?}"));
        assert_eq!(set.desired.expect("desired").version, "1.2.3");
        let read = client
            .converge(
                &target,
                "tok-2",
                ConvergeRequest {
                    service: "svc".to_owned(),
                },
            )
            .unwrap_or_else(|e| panic!("{host}: {e:?}"));
        let convergence = read.convergence.expect("a convergence");
        assert_eq!(convergence.state, ConvergenceState::Converged as i32);
        assert_eq!(convergence.roles[0].at_desired, 2);
    }
    let seen = server.seen.lock().expect("lock");
    assert_eq!(
        seen.bearers,
        [
            "Bearer tok-1",
            "Bearer tok-2",
            "Bearer tok-1",
            "Bearer tok-2"
        ]
    );
    assert_eq!(seen.sets, [request("svc"), request("svc")]);
    assert_eq!(seen.converges.len(), 2);
    assert_eq!(seen.converges[0].service, "svc");
}

/// A set of ports whose token and Releases calls are real, and whose pause and lines are the
/// fake's.
struct Live {
    fake: Fake,
    client: Tonic,
}

impl Ports for Live {
    fn token(&mut self, audience: &str) -> Result<String, TokenError> {
        self.fake.token(audience)
    }

    fn releases(&self) -> &dyn Releases {
        &self.client
    }

    fn pause(&mut self, secs: u64) {
        self.fake.pause(secs);
    }

    fn say(&mut self, line: &str) {
        self.fake.say(line);
    }
}

/// Proves: the whole flow over the real client ends `converged` with one token per call as the
/// bearer of that call, and a refusal status of the server reaches the flow as its refusal token.
///
/// Catches: a status that is turned into another code or message on the way, a flow that sends
/// one token twice, and a refusal read as a lost call.
#[test]
fn the_flow_runs_over_the_real_client_and_a_refusal_reaches_it_as_its_token() {
    let pki = pki();
    let server = Loopback::start(&pki);
    let dir = scratch("grpc-flow");
    let f = flags(dir.path());
    let target = target(dir.path(), &pki, "127.0.0.1", &server);
    let mut live = Live {
        fake: Fake::new(stored("unused"), vec![]),
        client: Tonic::new(),
    };
    let mut settings = settings(&f);
    settings.endpoint = target.endpoint.clone();
    settings.ca_file = target.ca_file.clone();
    let outcome = execute(&settings, &mut live);
    assert_eq!(outcome.reason, "converged");
    assert_eq!(outcome.desired_version, "1.2.3");
    assert_eq!(
        server.seen.lock().expect("lock").bearers,
        ["Bearer token-1", "Bearer token-2"]
    );
    settings.service = "refused".to_owned();
    let refused = execute(&settings, &mut live);
    assert_eq!(refused.reason, "held_bad");
    assert_eq!(refused.desired_version, "");
    assert_eq!(
        live.fake
            .log
            .lock()
            .expect("lock")
            .said
            .last()
            .map(String::as_str),
        Some(
            "::error::the coordinator refused the version 1.2.3 of refused: held_bad; it is not set"
        )
    );
}

/// The status of a failure, or a panic when the call was not sent or went through.
fn status_of<T: std::fmt::Debug>(result: Result<T, Failure>) -> Status {
    match result {
        Err(Failure::Status(status)) => status,
        other => panic!("expected a status, got {other:?}"),
    }
}

/// The reason of a failure that sent nothing, or a panic.
fn not_sent<T: std::fmt::Debug>(result: Result<T, Failure>) -> String {
    match result {
        Err(Failure::NotSent(why)) => why,
        other => panic!("expected a call not sent, got {other:?}"),
    }
}

/// Proves: a call that cannot be made says why and sends nothing: a CA file that is missing or
/// holds no certificate, an endpoint that is not `https://<host>[:<port>]`, and a token that
/// cannot be a header value.
///
/// Catches: a missing CA file reported as a lost call, an empty CA file trusted as no roots, an
/// `http://` endpoint, one with a path, a query or user information accepted, and a token with
/// a space, a control character, a non-ASCII character or none sent as it is.
#[test]
fn a_call_that_cannot_be_made_sends_nothing() {
    let dir = scratch("grpc-not-sent");
    let ca_file = dir.path().join("ca.pem");
    let at = |endpoint: &str| Target {
        endpoint: endpoint.to_owned(),
        ca_file: ca_file.clone(),
    };
    let client = Tonic::new();
    let converge =
        |target: &Target, token: &str| client.converge(target, token, ConvergeRequest::default());

    let missing = not_sent(converge(&at("https://127.0.0.1:1"), "tok"));
    let io = std::fs::read(&ca_file).expect_err("no CA file");
    assert_eq!(
        missing,
        format!("cannot read the CA from {}: {io}", ca_file.display())
    );

    std::fs::write(&ca_file, "not a certificate\n").expect("write");
    assert_eq!(
        not_sent(converge(&at("https://127.0.0.1:1"), "tok")),
        format!("the CA file {} holds no certificate", ca_file.display())
    );
    std::fs::write(&ca_file, "").expect("write");
    assert_eq!(
        not_sent(converge(&at("https://127.0.0.1:1"), "tok")),
        format!("the CA file {} holds no certificate", ca_file.display())
    );

    for endpoint in [
        "http://127.0.0.1:1",
        "127.0.0.1:1",
        "https://127.0.0.1:1/path",
        "https://127.0.0.1:1/",
        "https://127.0.0.1:1?q=1",
        "https://user@127.0.0.1:1",
        "https://",
    ] {
        assert_eq!(
            not_sent(converge(&at(endpoint), "tok")),
            format!("{endpoint:?} is not https://<host>[:<port>]"),
            "{endpoint}"
        );
    }
    for token in ["", "a b", "a\nb", "caf\u{e9}"] {
        assert_eq!(
            not_sent(converge(&at("https://127.0.0.1:1"), token)),
            "the OIDC token is not a bearer token of visible ASCII",
            "{token:?}"
        );
    }
}

/// Proves: a server whose certificate was signed by another CA than the CA file ends the call
/// with UNAVAILABLE, and the status text does not hold the token.
///
/// Catches: a client that trusts the system roots, or any certificate, instead of the CA file.
#[test]
fn a_server_signed_by_another_ca_is_unavailable() {
    let server_pki = pki();
    let other = pki();
    let server = Loopback::start(&server_pki);
    let dir = scratch("grpc-other-ca");
    let target = target(dir.path(), &other, "127.0.0.1", &server);
    let status = status_of(Tonic::new().set_desired(&target, "secret-tok", request("svc")));
    assert_eq!(status.code(), Code::Unavailable);
    assert!(
        status
            .message()
            .starts_with(&format!("cannot connect to {}", target.endpoint)),
        "{}",
        status.message()
    );
    assert!(!status.message().contains("secret-tok"));
    assert!(server.seen.lock().expect("lock").bearers.is_empty());
}

/// Proves: a call not finished within its bound ends with DEADLINE_EXCEEDED naming the endpoint
/// and the bound. The bound here is zero and the peer is a socket that never answers, so the
/// call cannot finish: no time is measured.
///
/// Catches: a call without a bound (it would wait for a peer that never answers), and a
/// timeout reported as another code.
#[test]
fn a_call_that_cannot_finish_within_its_bound_is_deadline_exceeded() {
    let silent = TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
    let port = silent.local_addr().expect("its address").port();
    let pki = pki();
    let dir = scratch("grpc-bound");
    let ca_file = dir.path().join("ca.pem");
    std::fs::write(&ca_file, &pki.ca_pem).expect("write the CA");
    let target = Target {
        endpoint: format!("https://127.0.0.1:{port}"),
        ca_file,
    };
    let status = status_of(Tonic::bounded(Duration::ZERO).converge(
        &target,
        "tok",
        ConvergeRequest::default(),
    ));
    assert_eq!(status.code(), Code::DeadlineExceeded);
    assert_eq!(
        status.message(),
        format!("the call to https://127.0.0.1:{port} did not finish within 0 seconds")
    );
}
