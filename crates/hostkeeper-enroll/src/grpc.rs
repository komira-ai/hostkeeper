//! [`GrpcEnroll`]: the [`CoordinatorEnroll`] port over gRPC with TLS, to the coordinator's
//! `Enroll`, `Poll` and `Renew` (`hostkeeper.agent.v1.Coordinator`).
//!
//! Which coordinator it talks to is decided in two steps:
//!
//! 1. [`CoordinatorEnroll::presented_ca_der`] connects, runs a TLS handshake that accepts any
//!    chain and sends no application data (`probe.rs`), and returns the last certificate of the
//!    chain the coordinator presented: the CA it ends in. The enroller compares its SHA-256 with
//!    the pinned `ca_sha256`.
//! 2. Every call after it opens a new channel whose TLS trusts that CA alone, so a coordinator
//!    whose chain does not end in it fails the handshake and receives nothing. A call made before
//!    the first successful probe, or after a failed one, sends nothing.
//!
//! `Enroll` and `Poll` present no client certificate: the host has none yet. `Renew` presents the
//! host's current certificate and key, read from the files of [`CurrentPaths`] when it is called
//! (the certificate as PEM, the key as the PKCS#8 DER the store keeps).
//!
//! The port is synchronous, so a [`GrpcEnroll`] owns a Tokio runtime on the current thread and
//! blocks on it for each call. It must not be called, or dropped, from inside a Tokio runtime.
//!
//! `convert.rs` turns the messages into the port's types and back.

mod convert;
mod probe;
#[cfg(test)]
mod server;
#[cfg(test)]
mod tests;

use std::path::Path;
use std::time::Duration;

use hostkeeper_proto::hostkeeper::agent::v1::coordinator_client::CoordinatorClient;
use rustls::RootCertStore;
use rustls::pki_types::{CertificateDer, ServerName, TrustAnchor};
use tokio::runtime::Runtime;
use tonic::codegen::http::Uri;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint, Identity};

use crate::store::CurrentPaths;
use crate::wire::{
    CoordinatorEnroll, EnrollAnswer, EnrollRequest, Poll, PollAnswer, RenewAnswer, RenewRequest,
    TransportError,
};

/// Why a [`GrpcEnroll`] could not be made.
#[derive(Debug, thiserror::Error)]
pub enum GrpcError {
    #[error(
        "the coordinator endpoint {0:?} is not host:port, or an https:// URL of a host with an optional port, with a DNS name or an IP address as the host"
    )]
    Endpoint(String),
    #[error("cannot start the runtime of the coordinator client: {0}")]
    Runtime(std::io::Error),
}

/// The type label of a PEM PKCS#8 private key.
const PRIVATE_KEY: &str = "PRIVATE KEY";

/// The port of an `https://` URL that names none.
const HTTPS_PORT: u16 = 443;

/// The coordinator's address.
#[derive(Debug)]
struct Target {
    /// The address of the channel: the host and the port under `https://`.
    uri: Uri,
    /// The host without brackets: what the TLS handshake names, and what is connected to.
    host: String,
    port: u16,
    name: ServerName<'static>,
    /// `host:port`, for messages.
    shown: String,
}

impl Target {
    fn parse(text: &str) -> Result<Target, GrpcError> {
        let bad = || GrpcError::Endpoint(text.to_owned());
        let url = text.strip_prefix("https://");
        let uri = format!("https://{}", url.unwrap_or(text))
            .parse::<Uri>()
            .map_err(|_| bad())?;
        // Nothing but the host and the port: no user, path or query. An `https://` URL that names
        // no port names 443, as the daemon config's `coordinator` may; a bare `host:port` must
        // name its port.
        let written = uri.port().map(|port| format!(":{port}"));
        let (shown_host, port) = uri
            .host()
            .zip(uri.port_u16().or(url.map(|_| HTTPS_PORT)))
            .filter(|(host, _)| {
                uri.to_string() == format!("https://{host}{}/", written.as_deref().unwrap_or(""))
            })
            .ok_or_else(bad)?;
        let shown = format!("{shown_host}:{port}");
        let host = shown_host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned();
        let name = ServerName::try_from(host.clone()).map_err(|_| bad())?;
        Ok(Target {
            uri,
            host,
            port,
            name,
            shown,
        })
    }
}

/// The coordinator's enrollment API over gRPC with TLS; see the module documentation.
#[derive(Debug)]
pub struct GrpcEnroll {
    runtime: Runtime,
    target: Target,
    current: CurrentPaths,
    /// Bounds the connection, the handshake and each call.
    timeout: Duration,
    /// The CA the last probe returned, which every call's TLS trusts alone.
    trusted: Option<Vec<TrustAnchor<'static>>>,
}

impl GrpcEnroll {
    /// A client of the coordinator at `endpoint`: `host:port`, or an `https://` URL of a host
    /// with an optional port (443 when it names none), the host a DNS name or an IP address. `current` names the files of the host's
    /// current certificate and key, which `Renew` presents. `timeout` bounds the connection and
    /// handshake of every connection and every call. Nothing is connected until
    /// [`CoordinatorEnroll::presented_ca_der`].
    pub fn new(
        endpoint: &str,
        current: CurrentPaths,
        timeout: Duration,
    ) -> Result<GrpcEnroll, GrpcError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        GrpcEnroll::with_runtime(runtime, endpoint, current, timeout)
    }

    /// [`GrpcEnroll::new`] with the result of making the runtime given.
    fn with_runtime(
        runtime: std::io::Result<Runtime>,
        endpoint: &str,
        current: CurrentPaths,
        timeout: Duration,
    ) -> Result<GrpcEnroll, GrpcError> {
        let target = Target::parse(endpoint)?;
        Ok(GrpcEnroll {
            runtime: runtime.map_err(GrpcError::Runtime)?,
            target,
            current,
            timeout,
            trusted: None,
        })
    }

    /// A client whose TLS trusts the probed CA alone, and presents the host's certificate and
    /// key when `identity` names their files.
    fn client(
        &self,
        identity: Option<&CurrentPaths>,
    ) -> Result<CoordinatorClient<Channel>, TransportError> {
        let anchors = self.trusted.as_ref().ok_or_else(|| {
            TransportError(
                "the coordinator's CA has not been read: presented_ca_der comes first".to_owned(),
            )
        })?;
        let mut tls = ClientTlsConfig::new()
            .trust_anchors(anchors.iter().cloned())
            .domain_name(self.target.host.clone())
            .timeout(self.timeout);
        if let Some(paths) = identity {
            tls = tls.identity(read_identity(paths)?);
        }
        let shown = &self.target.shown;
        let endpoint = Endpoint::from(self.target.uri.clone())
            .connect_timeout(self.timeout)
            .timeout(self.timeout)
            .tls_config(tls)
            .map_err(|e| {
                TransportError(format!("cannot set up TLS for {shown}: {}", describe(&e)))
            })?;
        let channel = self
            .runtime
            .block_on(endpoint.connect())
            .map_err(|e| TransportError(format!("cannot connect to {shown}: {}", describe(&e))))?;
        Ok(CoordinatorClient::new(channel))
    }
}

/// `error` and its causes, from the outermost, separated by `: `.
fn describe(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut cause = error.source();
    while let Some(next) = cause {
        text.push_str(": ");
        text.push_str(&next.to_string());
        cause = next.source();
    }
    text
}

/// The client certificate and key in the files `paths` names. The store keeps the certificate as
/// PEM and the key as PKCS#8 DER; TLS takes the key as PEM.
fn read_identity(paths: &CurrentPaths) -> Result<Identity, TransportError> {
    let read = |what: &str, path: &Path| {
        std::fs::read(path)
            .map_err(|e| TransportError(format!("cannot read {what} {}: {e}", path.display())))
    };
    let cert = read("the host certificate", &paths.cert)?;
    let key = read("the host key", &paths.key)?;
    Ok(Identity::from_pem(cert, convert::pem(PRIVATE_KEY, &key)?))
}

/// The last certificate of `chain` (the CA it ends in) as DER, and the trust anchors of that
/// certificate alone.
fn trust_only_last(
    chain: Vec<CertificateDer<'static>>,
) -> Result<(Vec<u8>, Vec<TrustAnchor<'static>>), TransportError> {
    let ca = chain
        .last()
        .ok_or_else(|| TransportError("the coordinator presented no certificate".to_owned()))?;
    let mut roots = RootCertStore::empty();
    roots.add(ca.clone()).map_err(|e| {
        TransportError(format!(
            "the last certificate the coordinator presented is not a CA certificate this daemon can use: {e}"
        ))
    })?;
    Ok((ca.to_vec(), roots.roots))
}

impl CoordinatorEnroll for GrpcEnroll {
    fn presented_ca_der(&mut self) -> Result<Vec<u8>, TransportError> {
        self.trusted = None;
        let target = &self.target;
        let chain =
            probe::presented_chain(&target.host, target.port, target.name.clone(), self.timeout)
                .map_err(|e| {
                    TransportError(format!(
                        "cannot read the CA of {}: {}",
                        target.shown,
                        describe(&e)
                    ))
                });
        chain.and_then(trust_only_last).map(|(der, anchors)| {
            self.trusted = Some(anchors);
            der
        })
    }

    fn enroll(&mut self, request: &EnrollRequest) -> Result<EnrollAnswer, TransportError> {
        let mut client = self.client(None)?;
        let reply = self
            .runtime
            .block_on(client.enroll(convert::enroll_request(request)))
            .map_err(|status| convert::status_error("Enroll", &status))?;
        convert::enroll_answer(reply.into_inner())
    }

    fn poll(&mut self, poll: &Poll) -> Result<PollAnswer, TransportError> {
        let mut client = self.client(None)?;
        let reply = self
            .runtime
            .block_on(client.poll(convert::poll_request(poll)))
            .map_err(|status| convert::status_error("Poll", &status))?;
        convert::poll_answer(reply.into_inner())
    }

    fn renew(&mut self, request: &RenewRequest) -> Result<RenewAnswer, TransportError> {
        let mut client = self.client(Some(&self.current))?;
        let reply = self
            .runtime
            .block_on(client.renew(convert::renew_request(request)));
        convert::renew_answer(reply, &request.host_id)
    }
}
