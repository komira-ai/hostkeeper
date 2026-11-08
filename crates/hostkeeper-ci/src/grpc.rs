//! The Releases calls: what the flow asks of the coordinator, and the gRPC client that asks it.
//!
//! One call is one connection: the client reads the CA file, connects over TLS trusting only the
//! certificates in it for the endpoint's host, sends the request with `authorization: Bearer
//! <token>` (marked sensitive, so that no log of the transport prints it), and gives back the
//! reply or the status the call ended with. The whole of it, the connection included, must be
//! done within [`CALL_BOUND`]; a call not done by then ends with DEADLINE_EXCEEDED. Each call
//! runs on a current-thread runtime of a thread of its own, so that no ambient runtime is
//! needed.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hostkeeper_proto::hostkeeper::operator::v1::releases_client::ReleasesClient;
use hostkeeper_proto::hostkeeper::operator::v1::{
    ConvergeRequest, ConvergeResponse, SetDesiredRequest, SetDesiredResponse,
};
use rustls_pki_types::CertificateDer;
use rustls_pki_types::pem::PemObject as _;
use tonic::metadata::{Ascii, MetadataValue};
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Uri};
use tonic::{Request, Response, Status};

/// How long one call may take, the connection and the TLS handshake included.
pub(crate) const CALL_BOUND: Duration = Duration::from_secs(30);

/// Where the calls go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Target {
    /// `https://<host>[:<port>]`.
    pub endpoint: String,
    /// The PEM file of the CA the coordinator's certificate must chain to.
    pub ca_file: PathBuf,
}

/// Why a call has no reply.
#[derive(Debug, Clone)]
pub(crate) enum Failure {
    /// The call ended with this status: the coordinator's, or the transport's.
    Status(Status),
    /// Nothing was sent: the text says why.
    NotSent(String),
}

/// The calls of the Releases service the flow makes. `bearer` is the raw token.
pub(crate) trait Releases {
    /// `SetDesired`.
    fn set_desired(
        &self,
        target: &Target,
        bearer: &str,
        request: SetDesiredRequest,
    ) -> Result<SetDesiredResponse, Failure>;
    /// `Converge`.
    fn converge(
        &self,
        target: &Target,
        bearer: &str,
        request: ConvergeRequest,
    ) -> Result<ConvergeResponse, Failure>;
}

/// The real client: gRPC over TLS.
pub(crate) struct Tonic {
    bound: Duration,
}

impl Tonic {
    /// A client whose calls are bound by [`CALL_BOUND`].
    pub(crate) fn new() -> Tonic {
        Tonic::bounded(CALL_BOUND)
    }

    /// A client whose calls are bound by `bound`.
    pub(crate) fn bounded(bound: Duration) -> Tonic {
        Tonic { bound }
    }
}

impl Releases for Tonic {
    fn set_desired(
        &self,
        target: &Target,
        bearer: &str,
        request: SetDesiredRequest,
    ) -> Result<SetDesiredResponse, Failure> {
        self.unary(target, bearer, request, |channel, request| async move {
            ReleasesClient::new(channel).set_desired(request).await
        })
    }

    fn converge(
        &self,
        target: &Target,
        bearer: &str,
        request: ConvergeRequest,
    ) -> Result<ConvergeResponse, Failure> {
        self.unary(target, bearer, request, |channel, request| async move {
            ReleasesClient::new(channel).converge(request).await
        })
    }
}

impl Tonic {
    /// One unary call: `call` is given the connected channel and the request, with the bearer in
    /// its metadata.
    fn unary<Q, A, F, Fut>(
        &self,
        target: &Target,
        token: &str,
        message: Q,
        call: F,
    ) -> Result<A, Failure>
    where
        Q: Send,
        A: Send,
        F: FnOnce(Channel, Request<Q>) -> Fut + Send,
        Fut: Future<Output = Result<Response<A>, Status>> + Send,
    {
        let (uri, host) = https(&target.endpoint).ok_or_else(|| {
            Failure::NotSent(format!(
                "{:?} is not https://<host>[:<port>]",
                target.endpoint
            ))
        })?;
        let bearer = bearer(token).ok_or_else(|| {
            Failure::NotSent("the OIDC token is not a bearer token of visible ASCII".to_owned())
        })?;
        let ca = read_ca(&target.ca_file)?;
        let tls = ClientTlsConfig::new()
            .ca_certificate(Certificate::from_pem(ca))
            .domain_name(host);
        let endpoint = Endpoint::from(uri.clone())
            .tls_config(tls)
            .map_err(|e| Failure::NotSent(format!("the TLS configuration is not usable: {e}")))?;
        let mut request = Request::new(message);
        request.metadata_mut().insert("authorization", bearer);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| Failure::NotSent(format!("cannot start a runtime: {e}")))?;
        let bound = self.bound;
        let work = async move {
            let channel = endpoint.connect().await.map_err(|e| {
                Status::unavailable(format!("cannot connect to {uri}: {}", source_chain(&e)))
            })?;
            call(channel, request).await.map(Response::into_inner)
        };
        let deadline = Status::deadline_exceeded(format!(
            "the call to {} did not finish within {} seconds",
            target.endpoint,
            bound.as_secs()
        ));
        // A thread of its own: a runtime cannot be entered from inside another. A panic of the
        // call goes on as a panic of this one.
        let mut result = Err(deadline.clone());
        std::thread::scope(|scope| {
            scope.spawn(|| {
                result = runtime
                    .block_on(async { tokio::time::timeout(bound, work).await })
                    .unwrap_or(Err(deadline));
            });
        });
        result.map_err(Failure::Status)
    }
}

/// The URI and the host of `endpoint` when it is `https://<host>[:<port>]` with nothing more.
fn https(endpoint: &str) -> Option<(Uri, String)> {
    let uri: Uri = endpoint.parse().ok()?;
    let authority = uri.authority()?;
    let plain = uri.scheme_str() == Some("https")
        && !authority.as_str().contains('@')
        && uri.path() == "/"
        && uri.query().is_none()
        && !endpoint.ends_with('/');
    // An IPv6 host is written in brackets in a URI, and without them as a TLS name.
    let host = authority
        .host()
        .trim_start_matches('[')
        .trim_end_matches(']');
    plain.then(|| (uri.clone(), host.to_owned()))
}

/// `Bearer <token>`, marked sensitive, when `token` is one or more bytes of visible ASCII.
fn bearer(token: &str) -> Option<MetadataValue<Ascii>> {
    let visible = !token.is_empty() && token.bytes().all(|b| b.is_ascii_graphic());
    visible
        .then(|| MetadataValue::try_from(format!("Bearer {token}")).ok())
        .flatten()
        .map(|mut value| {
            value.set_sensitive(true);
            value
        })
}

/// The PEM of the CA file, which holds at least one certificate.
fn read_ca(path: &Path) -> Result<Vec<u8>, Failure> {
    let pem = std::fs::read(path).map_err(|e| {
        Failure::NotSent(format!("cannot read the CA from {}: {e}", path.display()))
    })?;
    match CertificateDer::pem_slice_iter(&pem).next() {
        Some(Ok(_)) => Ok(pem),
        _ => Err(Failure::NotSent(format!(
            "the CA file {} holds no certificate",
            path.display()
        ))),
    }
}

/// An error with each of its sources, `: `-separated.
fn source_chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}
