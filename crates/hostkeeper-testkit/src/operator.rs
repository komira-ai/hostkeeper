//! The operator's side of the coordinator's API in a test: the generated tonic clients of
//! `hostkeeper-proto` over gRPC with TLS, with the operator's bearer token on every call, from a
//! test that is not async.
//!
//! [`Operator::connect`] sets up the channel once: `https://<host>[:<port>]`, trusting only the CA
//! the test names, with `authorization: Bearer <token>` added to each request. [`Operator::call`]
//! hands the test a [`Channel`] to build a generated client on, runs the call on the operator's
//! own runtime and gives back the reply's message. [`canonical_json`] turns a reply into the
//! proto3 canonical JSON the operator API's JSON gateway answers with, for tests that read a
//! reply's fields by their JSON names.
//!
//! The crate names no workspace crate (`crates/xtask/tests/layering`), so the clients and the
//! descriptor set come from the test's own `hostkeeper-proto`.

use std::future::Future;
use std::time::Duration;

use prost_reflect::{DescriptorPool, DynamicMessage};
use tonic::metadata::{Ascii, MetadataValue};
use tonic::service::Interceptor;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::{Certificate, ClientTlsConfig, Endpoint, Uri};
use tonic::{Request, Response, Status};

/// How long the connection, and then each call, may take. A coordinator that stops answering
/// fails the test that waits on it instead of hanging it; no test measures this time.
pub const PATIENCE: Duration = Duration::from_secs(30);

/// The channel a generated client is built on: TLS to the coordinator, with the operator's
/// bearer token on every request.
pub type Channel = InterceptedService<tonic::transport::Channel, Bearer>;

/// Adds `authorization: Bearer <token>` to every request.
#[derive(Clone)]
pub struct Bearer(MetadataValue<Ascii>);

impl Interceptor for Bearer {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, Status> {
        request
            .metadata_mut()
            .insert("authorization", self.0.clone());
        Ok(request)
    }
}

/// Why [`Operator::connect`] made no channel.
#[derive(Debug, thiserror::Error)]
pub enum OperatorError {
    /// The endpoint is not `https://<host>[:<port>]`.
    #[error("{0:?} is not https://<host>[:<port>]")]
    Endpoint(String),
    /// The token cannot be a header value: empty, or holding a byte that is not visible ASCII.
    #[error("the token is not a bearer token of visible ASCII")]
    Token,
    /// The CA is not usable as the channel's only trust root.
    #[error("the CA is not usable: {0}")]
    Ca(String),
    /// The runtime the calls run on could not be started.
    #[error("cannot start a runtime: {0}")]
    Runtime(std::io::Error),
}

/// The operator's gRPC client: the endpoint, the token, and the runtime its calls run on.
pub struct Operator {
    runtime: tokio::runtime::Runtime,
    endpoint: Endpoint,
    bearer: Bearer,
}

impl Operator {
    /// The channel to `endpoint` (`https://<host>[:<port>]`), trusting only the certificates of
    /// `ca_pem` for the name `<host>` (a DNS name or an IP address), each call carrying
    /// `token`. Nothing is sent until the first call, and each call connects anew: a test that
    /// restarts the coordinator on the same address calls the new one, never a connection to the
    /// old one.
    pub fn connect(endpoint: &str, ca_pem: &[u8], token: &str) -> Result<Operator, OperatorError> {
        let (uri, host) =
            https(endpoint).ok_or_else(|| OperatorError::Endpoint(endpoint.into()))?;
        let bearer = bearer(token).ok_or(OperatorError::Token)?;
        let tls = ClientTlsConfig::new()
            .ca_certificate(Certificate::from_pem(ca_pem))
            .domain_name(host);
        let endpoint = Endpoint::from(uri)
            .connect_timeout(PATIENCE)
            .timeout(PATIENCE)
            .tls_config(tls)
            .map_err(|e| OperatorError::Ca(source_chain(&e)))?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(OperatorError::Runtime)?;
        Ok(Operator {
            runtime,
            endpoint,
            bearer: Bearer(bearer),
        })
    }

    /// Runs one call to its reply: `call` builds a generated client on the channel it is given
    /// and calls one method, as in
    /// `operator.call(|channel| async move { HostsClient::new(channel).list(request).await })`.
    /// The status a call ends with is the error, as the coordinator (or the channel) gave it.
    ///
    /// # Panics
    /// When the calling thread is inside a Tokio runtime already, as a runtime's `block_on`
    /// does: the operator's calls are made from a test's own thread.
    pub fn call<T, F, Fut>(&self, call: F) -> Result<T, Status>
    where
        F: FnOnce(Channel) -> Fut,
        Fut: Future<Output = Result<Response<T>, Status>>,
    {
        // A lazy channel spawns its connection's task, so it is made inside the runtime; it is
        // dropped with its connection when the call ends.
        let channel = {
            let _inside = self.runtime.enter();
            self.endpoint.connect_lazy()
        };
        self.runtime
            .block_on(call(InterceptedService::new(channel, self.bearer.clone())))
            .map(Response::into_inner)
    }
}

/// The URI and the host of `endpoint` when it is `https://<host>[:<port>]` with nothing more:
/// no user, no path and no query.
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
    if token.is_empty() || !token.bytes().all(|b| b.is_ascii_graphic()) {
        return None;
    }
    let mut value = MetadataValue::try_from(format!("Bearer {token}")).ok()?;
    value.set_sensitive(true);
    Some(value)
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

/// Why [`canonical_json`] gave no JSON.
#[derive(Debug, thiserror::Error)]
pub enum JsonError {
    /// The descriptor set does not decode.
    #[error("the descriptor set does not decode: {0}")]
    Descriptors(#[from] prost_reflect::DescriptorError),
    /// The descriptor set has no message of that name.
    #[error("the descriptor set has no message {0}")]
    Unknown(String),
    /// The message's bytes are not a message of that name.
    #[error("the bytes are not a {name}: {why}")]
    Decode {
        name: String,
        why: prost::DecodeError,
    },
}

/// `message`, the message named `name` (`package.Message`) in the encoded descriptor set
/// `descriptors`, as proto3 canonical JSON: fields by their JSON names (`nodeId`), enums by their
/// value names, 64-bit integers as strings, and a field at its default left out.
pub fn canonical_json(
    descriptors: &[u8],
    name: &str,
    message: &impl prost::Message,
) -> Result<serde_json::Value, JsonError> {
    let pool = DescriptorPool::decode(descriptors)?;
    let descriptor = pool
        .get_message_by_name(name)
        .ok_or_else(|| JsonError::Unknown(name.to_owned()))?;
    let dynamic =
        DynamicMessage::decode(descriptor, message.encode_to_vec().as_slice()).map_err(|why| {
            JsonError::Decode {
                name: name.to_owned(),
                why,
            }
        })?;
    // A dynamic message always serializes: every value it holds has a JSON form.
    Ok(serde_json::to_value(&dynamic).unwrap_or_default())
}

#[cfg(test)]
mod tests;
