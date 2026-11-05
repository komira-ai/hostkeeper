//! The coordinator the tests of `GrpcEnroll` talk to: a tonic server with TLS on 127.0.0.1 (port
//! 0), whose `Coordinator` service records every call and the client certificates it carried, and
//! answers from a script or, when the script has nothing left, as a well-behaved coordinator
//! would. The test PKI is made at run time, so that no key is committed.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_proto::hostkeeper::agent::v1::coordinator_server::{Coordinator, CoordinatorServer};
use tonic::transport::{Certificate, Identity, Server, ServerTlsConfig};
use tonic::{Request, Response, Status};
use x509_cert::der::{Decode, Encode};
use x509_cert::request::CertReq;

use crate::wire::RequestId;

/// A test CA and the certificates it signed, as PEM and DER.
pub(super) struct Pki {
    pub ca_pem: String,
    pub ca_der: Vec<u8>,
    /// The coordinator's certificate for 127.0.0.1 followed by the CA: the chain it presents.
    pub chain_pem: String,
    /// The coordinator's certificate alone.
    pub leaf_pem: String,
    pub leaf_der: Vec<u8>,
    pub server_key_pem: String,
    /// The same key as PKCS#8 DER.
    pub server_key_der: Vec<u8>,
    /// A host certificate (`host:test-client`) and its key as PKCS#8 DER, the form the store keeps.
    pub client_cert_pem: String,
    pub client_cert_der: Vec<u8>,
    pub client_key_der: Vec<u8>,
}

/// The parameters and key of a certificate named `name` for `san`, a CA when `ca` is set.
fn certificate(name: &str, san: &[&str], ca: bool) -> (rcgen::CertificateParams, rcgen::KeyPair) {
    let mut params =
        rcgen::CertificateParams::new(san.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>())
            .expect("params");
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, name);
    if ca {
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    }
    (params, rcgen::KeyPair::generate().expect("a key"))
}

/// A new PKI; two calls make two unrelated CAs.
pub(super) fn pki() -> Pki {
    let (ca_params, ca_key) = certificate("hostkeeper test ca", &[], true);
    let ca = rcgen::CertifiedIssuer::self_signed(ca_params, ca_key).expect("the CA");
    let (server, server_key) = certificate("hostkeeper test server", &["127.0.0.1"], false);
    let server = server
        .signed_by(&server_key, &*ca)
        .expect("the server certificate");
    let (client, client_key) = certificate("host:test-client", &[], false);
    let client = client
        .signed_by(&client_key, &*ca)
        .expect("the client certificate");
    Pki {
        ca_pem: ca.pem(),
        ca_der: ca.der().to_vec(),
        chain_pem: format!("{}{}", server.pem(), ca.pem()),
        leaf_pem: server.pem(),
        leaf_der: server.der().to_vec(),
        server_key_pem: server_key.serialize_pem(),
        server_key_der: server_key.serialize_der(),
        client_cert_pem: client.pem(),
        client_cert_der: client.der().to_vec(),
        client_key_der: client_key.serialize_der(),
    }
}

/// A request the service received.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Seen {
    Enroll(pb::EnrollRequest),
    Poll(pb::PollRequest),
    Renew(pb::RenewRequest),
}

/// A call the service received, and the certificates the client presented in the handshake.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Call {
    pub seen: Seen,
    pub client_certs: Vec<Vec<u8>>,
}

#[derive(Default)]
struct Shared {
    calls: Mutex<Vec<Call>>,
    enroll: Mutex<VecDeque<Result<pb::EnrollResponse, Status>>>,
    poll: Mutex<VecDeque<Result<pb::PollResponse, Status>>>,
    renew: Mutex<VecDeque<Result<pb::RenewResponse, Status>>>,
}

/// The nonce a well-behaved coordinator gives.
pub(super) const NONCE: [u8; 32] = [9; 32];

/// The recording `Coordinator` service. Clones share their record and script.
#[derive(Clone, Default)]
pub(super) struct Double(Arc<Shared>);

impl Double {
    /// The calls received so far, in order.
    pub fn calls(&self) -> Vec<Call> {
        self.0.calls.lock().expect("calls").clone()
    }

    /// The next `Enroll` is answered with `answer`.
    pub fn script_enroll(&self, answer: Result<pb::EnrollResponse, Status>) {
        self.0.enroll.lock().expect("script").push_back(answer);
    }

    /// The next `Poll` that has no answer scripted before it is answered with `answer`.
    pub fn script_poll(&self, answer: Result<pb::PollResponse, Status>) {
        self.0.poll.lock().expect("script").push_back(answer);
    }

    /// The next `Renew` is answered with `answer`.
    pub fn script_renew(&self, answer: Result<pb::RenewResponse, Status>) {
        self.0.renew.lock().expect("script").push_back(answer);
    }

    fn record<T>(&self, request: &Request<T>, seen: Seen) {
        let client_certs = request
            .peer_certs()
            .map(|chain| chain.iter().map(|der| der.to_vec()).collect::<Vec<_>>())
            .unwrap_or_default();
        self.0
            .calls
            .lock()
            .expect("calls")
            .push(Call { seen, client_certs });
    }
}

/// The request id of the key a CSR is for.
fn request_id_of(csr_der: &[u8]) -> String {
    let csr = CertReq::from_der(csr_der).expect("a CSR");
    RequestId::of_spki(&csr.info.public_key.to_der().expect("DER")).to_string()
}

#[tonic::async_trait]
impl Coordinator for Double {
    async fn heartbeat(
        &self,
        _request: Request<pb::HeartbeatRequest>,
    ) -> Result<Response<pb::HeartbeatResponse>, Status> {
        Err(Status::unimplemented(
            "this coordinator serves no heartbeat",
        ))
    }

    async fn enroll(
        &self,
        request: Request<pb::EnrollRequest>,
    ) -> Result<Response<pb::EnrollResponse>, Status> {
        self.record(&request, Seen::Enroll(request.get_ref().clone()));
        let scripted = self.0.enroll.lock().expect("script").pop_front();
        scripted
            .unwrap_or_else(|| {
                Ok(pb::EnrollResponse {
                    request_id: request_id_of(&request.get_ref().csr_der),
                    state: pb::EnrollState::Pending.into(),
                })
            })
            .map(Response::new)
    }

    async fn poll(
        &self,
        request: Request<pb::PollRequest>,
    ) -> Result<Response<pb::PollResponse>, Status> {
        self.record(&request, Seen::Poll(request.get_ref().clone()));
        let scripted = self.0.poll.lock().expect("script").pop_front();
        let signed = !request.get_ref().signature.is_empty();
        scripted
            .unwrap_or_else(|| {
                Ok(if signed {
                    pb::PollResponse {
                        state: pb::PollState::Pending.into(),
                        ..pb::PollResponse::default()
                    }
                } else {
                    pb::PollResponse {
                        nonce: NONCE.to_vec(),
                        ..pb::PollResponse::default()
                    }
                })
            })
            .map(Response::new)
    }

    async fn renew(
        &self,
        request: Request<pb::RenewRequest>,
    ) -> Result<Response<pb::RenewResponse>, Status> {
        self.record(&request, Seen::Renew(request.get_ref().clone()));
        let scripted = self.0.renew.lock().expect("script").pop_front();
        scripted
            .unwrap_or_else(|| {
                Ok(pb::RenewResponse {
                    certificate_der: b"a renewed certificate".to_vec(),
                    ca_bundle_pem: "a bundle".to_owned(),
                })
            })
            .map(Response::new)
    }
}

/// A loopback gRPC server with TLS serving a [`Double`]; it stops when dropped, with its runtime.
pub(super) struct Loopback {
    _runtime: tokio::runtime::Runtime,
    pub port: u16,
}

impl Loopback {
    /// A server presenting the chain `chain_pem` with `key_pem`. With `client_ca`, it asks for a
    /// client certificate that chains to it and serves a client that has none.
    pub fn start(
        chain_pem: &str,
        key_pem: &str,
        client_ca: Option<&str>,
        double: Double,
    ) -> Loopback {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("a runtime");
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .expect("a loopback listener");
        let port = listener.local_addr().expect("its address").port();
        let mut tls = ServerTlsConfig::new().identity(Identity::from_pem(chain_pem, key_pem));
        if let Some(ca) = client_ca {
            tls = tls
                .client_ca_root(Certificate::from_pem(ca))
                .client_auth_optional(true);
        }
        let server = Server::builder()
            .tls_config(tls)
            .expect("TLS")
            .serve_with_incoming(
                CoordinatorServer::new(double),
                tokio_stream::wrappers::TcpListenerStream::new(listener),
            );
        runtime.spawn(server);
        Loopback {
            _runtime: runtime,
            port,
        }
    }
}
