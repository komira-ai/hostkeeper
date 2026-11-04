//! How `ctl` reaches the coordinator: the port [`CtlTransport`], one call of one RPC by its gRPC
//! path with the request and response in protobuf's wire format, and its implementations.
//!
//! - [`Tonic`]: gRPC over TLS (rustls), the server's certificate chaining to `--ca-file`, a
//!   client certificate when one is given (the heartbeat listener's mutual TLS).
//! - [`Gateway`]: the HTTP/JSON gateway of the private listener, `POST /<package>.<Service>/<Method>`
//!   with a proto3 canonical JSON body and the token as a bearer, over the same TLS.
//! - [`InMemory`]: tonic services called in-process, each request carrying an explicit
//!   [`PeerIdentity`] in its extensions, for tests; no socket.
//!
//! [`Connect`] makes the transport of a call from the global flags ([`Target`]); [`Network`]
//! makes the two real ones.

mod gateway;
mod grpc;
mod tls;

use std::path::PathBuf;
use std::time::Duration;

use bytes::Bytes;
use tonic::Status;
use tonic::codegen::http::Uri;
use tonic::metadata::MetadataMap;

pub use gateway::Gateway;
pub use grpc::{InMemory, Tonic};

/// One call of one unary RPC.
pub trait CtlTransport {
    /// Calls the RPC at `path` (`/hostkeeper.operator.v1.Releases/SetDesired`) with `request`,
    /// an encoded request message, and `metadata` (the bearer token, when there is one), and
    /// returns the encoded response or the status the call ended with.
    fn call(&self, path: &str, request: Bytes, metadata: &MetadataMap) -> Result<Bytes, Status>;
}

/// Makes the transport of a call.
pub trait Connect {
    /// The transport `target` names. Reading a file the flags name happens here, before any
    /// connection.
    fn connect(&self, target: &Target) -> Result<Box<dyn CtlTransport>, ConnectError>;
}

/// Who an in-memory call comes from, as a server's transport would have learned it: put in the
/// request's extensions, where a test's server reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerIdentity {
    /// A host, by the name its client certificate carries.
    Host(String),
    /// An operator, by the name of their token.
    Operator(String),
    /// CI, by the subject of its OIDC token.
    Ci(String),
    /// No credential.
    Anonymous,
}

/// Which transport `--transport` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    Grpc,
    Gateway,
}

/// The coordinator's listener, from `--endpoint`: `https://<host>[:<port>]`, nothing after.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    uri: Uri,
    host: String,
    port: u16,
}

/// Why `--endpoint` is not one.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("--endpoint {given:?} is not https://<host>[:<port>]: {why}")]
pub struct EndpointError {
    given: String,
    why: &'static str,
}

impl Endpoint {
    /// Parses `--endpoint`.
    pub fn parse(given: &str) -> Result<Endpoint, EndpointError> {
        let refuse = |why| EndpointError {
            given: given.to_owned(),
            why,
        };
        let uri: Uri = given.parse().map_err(|_| refuse("not a URL"))?;
        // A URL with a scheme always has an authority.
        let (Some("https"), Some(authority)) = (uri.scheme_str(), uri.authority()) else {
            return Err(refuse("the scheme must be https"));
        };
        if uri.path_and_query().is_some_and(|p| p.as_str() != "/") {
            return Err(refuse("it has a path or a query"));
        }
        if authority.as_str().contains('@') {
            return Err(refuse("it carries user information"));
        }
        let host = authority
            .host()
            .trim_start_matches('[')
            .trim_end_matches(']');
        // `Uri` reads a port it cannot parse as no port, so the text is read here.
        let port = match port_text(authority.as_str()) {
            None => 443,
            Some(text) => text
                .parse::<u16>()
                .ok()
                .filter(|port| *port != 0 && text.bytes().all(|b| b.is_ascii_digit()))
                .ok_or_else(|| refuse("the port must be a number from 1 to 65535"))?,
        };
        Ok(Endpoint {
            host: host.to_owned(),
            port,
            uri,
        })
    }

    /// The URL as given.
    pub fn uri(&self) -> &Uri {
        &self.uri
    }

    /// The host: a name, or an IP address without brackets.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The port; 443 when the URL names none.
    pub fn port(&self) -> u16 {
        self.port
    }
}

/// The text after the colon that follows the host of `authority`, when there is one (possibly
/// empty).
fn port_text(authority: &str) -> Option<&str> {
    let after_host = match authority.strip_prefix('[') {
        Some(rest) => rest.split_once(']').map_or("", |(_, tail)| tail),
        None => authority,
    };
    after_host.split_once(':').map(|(_, port)| port)
}

/// The global flags of a call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub endpoint: Endpoint,
    pub transport: TransportKind,
    /// The CA the server's certificate must chain to.
    pub ca_file: PathBuf,
    /// The client certificate and its key, for mutual TLS.
    pub client_identity: Option<(PathBuf, PathBuf)>,
    /// How long the whole call may take: connecting, TLS and the exchange.
    pub timeout: Duration,
}

/// Why the transport of a call cannot be made: always a file or a flag of the caller's.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("cannot read {what} from {}: {source}", path.display())]
    Read {
        what: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{what} in {} is not usable: {why}", path.display())]
    Unusable {
        what: &'static str,
        path: PathBuf,
        why: String,
    },
    #[error("the endpoint {endpoint} is not a TLS server name: {why}")]
    ServerName { endpoint: String, why: String },
    #[error("the descriptor set does not decode: {0}")]
    Descriptors(String),
}

/// Makes the real transports: [`Tonic`] or [`Gateway`], as `--transport` says.
#[derive(Debug, Clone, Copy, Default)]
pub struct Network;

impl Connect for Network {
    fn connect(&self, target: &Target) -> Result<Box<dyn CtlTransport>, ConnectError> {
        let material = tls::Material::read(target)?;
        let transport: Box<dyn CtlTransport> = match target.transport {
            TransportKind::Grpc => Box::new(Tonic::new(target, &material)?),
            TransportKind::Gateway => Box::new(Gateway::new(
                target,
                &material,
                hostkeeper_proto::FILE_DESCRIPTOR_SET,
            )?),
        };
        Ok(transport)
    }
}

#[cfg(test)]
mod tests;
