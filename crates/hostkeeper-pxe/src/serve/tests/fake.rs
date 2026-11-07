//! The fake coordinator of the tests: the operator API's `Enrollment` and `Hosts` services, over
//! TLS on a loopback port the kernel chooses, answering from lists the test sets and changes,
//! a page at a time when the test asks for small pages, and refusing every call with a status
//! the test sets. Every other call is unimplemented.

use std::sync::{Arc, Mutex};

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::enrollment_server::{Enrollment, EnrollmentServer};
use hostkeeper_proto::hostkeeper::operator::v1::hosts_server::{Hosts, HostsServer};
use tonic::codegen::{BoxFuture, Service, http};
use tonic::transport::{Identity, Server, ServerTlsConfig};
use tonic::{Code, Request, Response, Status};

/// A CA and a server certificate for `127.0.0.1` it signed, as PEM.
pub struct Pki {
    pub ca: String,
    pub cert: String,
    pub key: String,
}

/// A new PKI; two calls make two unrelated CAs.
pub fn pki() -> Pki {
    let params = |san: &[&str], ca: bool| {
        let mut params =
            rcgen::CertificateParams::new(san.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>())
                .expect("params");
        if ca {
            params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        }
        (params, rcgen::KeyPair::generate().expect("a key"))
    };
    let (ca_params, ca_key) = params(&[], true);
    let ca = rcgen::CertifiedIssuer::self_signed(ca_params, ca_key).expect("the CA");
    let (server, key) = params(&["127.0.0.1"], false);
    let server = server.signed_by(&key, &*ca).expect("the certificate");
    Pki {
        ca: ca.pem(),
        cert: server.pem(),
        key: key.serialize_pem(),
    }
}

/// What the fake answers, and what it saw.
#[derive(Default)]
pub struct State {
    pub records: Vec<proto::InventoryRecord>,
    pub hosts: Vec<proto::Host>,
    pub pending: Vec<proto::PendingEnrollment>,
    /// Every call is refused with this, when set.
    pub refuse: Option<(Code, String)>,
    /// The entries of one page; every entry on one page when 0.
    pub page: usize,
    /// The page token of every call, in order.
    pub tokens: Vec<String>,
    /// The `authorization` of every call, in order.
    pub bearers: Vec<String>,
}

/// The fake's services, sharing one [`State`].
#[derive(Clone, Default)]
pub struct Fake(pub Arc<Mutex<State>>);

impl Fake {
    /// Changes the state.
    pub fn set(&self, change: impl FnOnce(&mut State)) {
        change(&mut self.0.lock().expect("the fake's state"));
    }

    /// Reads the state.
    pub fn get<T>(&self, read: impl FnOnce(&State) -> T) -> T {
        read(&self.0.lock().expect("the fake's state"))
    }

    /// The page after `token` of the list `pick` gives, noting the call.
    fn page<T: Clone, R>(
        &self,
        request: &Request<R>,
        token: &str,
        pick: impl FnOnce(&State) -> Vec<T>,
    ) -> Result<(Vec<T>, String), Status> {
        let mut state = self.0.lock().expect("the fake's state");
        let bearer = request
            .metadata()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("none")
            .to_owned();
        state.bearers.push(bearer);
        state.tokens.push(token.to_owned());
        if let Some((code, message)) = &state.refuse {
            return Err(Status::new(*code, message.clone()));
        }
        let all = pick(&state);
        let from: usize = token.parse().unwrap_or(0);
        let size = match state.page {
            0 => all.len().max(1),
            size => size,
        };
        let to = (from + size).min(all.len());
        let next = match to < all.len() {
            true => to.to_string(),
            false => String::new(),
        };
        Ok((all[from.min(to)..to].to_vec(), next))
    }
}

fn unimplemented<T>() -> Result<Response<T>, Status> {
    Err(Status::unimplemented("not in the fake"))
}

#[tonic::async_trait]
impl Enrollment for Fake {
    async fn list(
        &self,
        request: Request<proto::ListPendingRequest>,
    ) -> Result<Response<proto::ListPendingResponse>, Status> {
        let token = request.get_ref().page_token.clone();
        let (requests, next_page_token) = self.page(&request, &token, |s| s.pending.clone())?;
        Ok(Response::new(proto::ListPendingResponse {
            requests,
            next_page_token,
        }))
    }

    async fn approve(
        &self,
        _: Request<proto::ApproveRequest>,
    ) -> Result<Response<proto::ApproveResponse>, Status> {
        unimplemented()
    }

    async fn reject(
        &self,
        _: Request<proto::RejectRequest>,
    ) -> Result<Response<proto::RejectResponse>, Status> {
        unimplemented()
    }

    async fn add_record(
        &self,
        _: Request<proto::AddRecordRequest>,
    ) -> Result<Response<proto::AddRecordResponse>, Status> {
        unimplemented()
    }

    async fn list_records(
        &self,
        request: Request<proto::ListRecordsRequest>,
    ) -> Result<Response<proto::ListRecordsResponse>, Status> {
        let token = request.get_ref().page_token.clone();
        let (records, next_page_token) = self.page(&request, &token, |s| s.records.clone())?;
        Ok(Response::new(proto::ListRecordsResponse {
            records,
            next_page_token,
        }))
    }

    async fn remove_record(
        &self,
        _: Request<proto::RemoveRecordRequest>,
    ) -> Result<Response<proto::RemoveRecordResponse>, Status> {
        unimplemented()
    }
}

#[tonic::async_trait]
impl Hosts for Fake {
    async fn list(
        &self,
        request: Request<proto::ListHostsRequest>,
    ) -> Result<Response<proto::ListHostsResponse>, Status> {
        let token = request.get_ref().page_token.clone();
        let (hosts, next_page_token) = self.page(&request, &token, |s| s.hosts.clone())?;
        Ok(Response::new(proto::ListHostsResponse {
            hosts,
            next_page_token,
        }))
    }

    async fn get(
        &self,
        _: Request<proto::GetHostRequest>,
    ) -> Result<Response<proto::GetHostResponse>, Status> {
        unimplemented()
    }

    async fn exclude(
        &self,
        _: Request<proto::ExcludeRequest>,
    ) -> Result<Response<proto::ExcludeResponse>, Status> {
        unimplemented()
    }

    async fn include(
        &self,
        _: Request<proto::IncludeRequest>,
    ) -> Result<Response<proto::IncludeResponse>, Status> {
        unimplemented()
    }

    async fn set_labels(
        &self,
        _: Request<proto::SetLabelsRequest>,
    ) -> Result<Response<proto::SetLabelsResponse>, Status> {
        unimplemented()
    }

    async fn abort(
        &self,
        _: Request<proto::AbortRequest>,
    ) -> Result<Response<proto::AbortResponse>, Status> {
        unimplemented()
    }

    async fn retire(
        &self,
        _: Request<proto::RetireRequest>,
    ) -> Result<Response<proto::RetireResponse>, Status> {
        unimplemented()
    }
}

/// Both services behind one: a call goes to `Hosts` or to `Enrollment` by its path.
#[derive(Clone)]
struct Both {
    enrollment: EnrollmentServer<Fake>,
    hosts: HostsServer<Fake>,
}

impl Service<http::Request<tonic::body::Body>> for Both {
    type Response = http::Response<tonic::body::Body>;
    type Error = std::convert::Infallible;
    type Future = BoxFuture<Self::Response, Self::Error>;

    fn poll_ready(
        &mut self,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: http::Request<tonic::body::Body>) -> Self::Future {
        match request
            .uri()
            .path()
            .starts_with("/hostkeeper.operator.v1.Hosts/")
        {
            true => self.hosts.call(request),
            false => self.enrollment.call(request),
        }
    }
}

/// The fake on a loopback port with TLS; it stops when dropped.
pub struct Coordinator {
    pub fake: Fake,
    pub port: u16,
    runtime: Option<tokio::runtime::Runtime>,
}

impl Coordinator {
    /// The fake serving `fake` with the server certificate of `pki`.
    pub fn start(pki: &Pki, fake: Fake) -> Coordinator {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("a runtime");
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .expect("a loopback listener");
        let port = listener.local_addr().expect("its address").port();
        let tls = ServerTlsConfig::new().identity(Identity::from_pem(&pki.cert, &pki.key));
        let server = Server::builder()
            .tls_config(tls)
            .expect("TLS")
            .serve_with_incoming(
                Both {
                    enrollment: EnrollmentServer::new(fake.clone()),
                    hosts: HostsServer::new(fake.clone()),
                },
                tokio_stream::wrappers::TcpListenerStream::new(listener),
            );
        runtime.spawn(server);
        Coordinator {
            fake,
            port,
            runtime: Some(runtime),
        }
    }

    /// Its endpoint, as `--coordinator` names it.
    pub fn endpoint(&self) -> String {
        format!("https://127.0.0.1:{}", self.port)
    }
}

impl Drop for Coordinator {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

/// A record of the inventory.
pub fn record(host_id: &str, macs: &[&str], enrolled: bool) -> proto::InventoryRecord {
    proto::InventoryRecord {
        host_id: host_id.to_owned(),
        data_macs: macs.iter().map(|m| (*m).to_owned()).collect(),
        enrolled,
        ..Default::default()
    }
}

/// A host the coordinator knows.
pub fn host(node_id: &str) -> proto::Host {
    proto::Host {
        node_id: node_id.to_owned(),
        ..Default::default()
    }
}

/// A pending request matched to the record of `host_id`.
pub fn pending(request_id: &str, host_id: &str) -> proto::PendingEnrollment {
    proto::PendingEnrollment {
        request_id: request_id.to_owned(),
        record_host_id: host_id.to_owned(),
        inventory_match: proto::InventoryMatch::Matched.into(),
        ..Default::default()
    }
}
