//! [`GrpcMtlsLink`]: the loop's [`CoordinatorLink`] over gRPC (`Coordinator.Heartbeat` of
//! `hostkeeper.agent.v1`), HTTP/2 and rustls mutual TLS, to a coordinator that advertised
//! `grpc-v1`.
//!
//! - **Who it talks to, and who it is.** As [`crate::JsonMtlsLink`]: the coordinator's address is
//!   the daemon config's `coordinator`, its certificate must chain to the CA of the pki
//!   directory's current generation whose SHA-256 the config pins (one implementation of that
//!   trust, [`crate::mtls`]'s, with ALPN `h2` here), and the client certificate and key are the
//!   heartbeat's [`ClientIdentity`]. The CA bundle is read again at every heartbeat.
//! - **How it reaches it.** One connection per heartbeat, opened through a [`GrpcDial`] (the
//!   production one is [`crate::TcpDial`], the only code of the crate that opens a socket). The
//!   TLS handshake is run before anything is said, so a server whose certificate is refused
//!   receives nothing; the tonic client then speaks HTTP/2 over the established stream.
//! - **What it sends and takes back.** The heartbeat as the protobuf request (the private
//!   module `proto_map`), refused when it is over [`MAX_HEARTBEAT_BYTES`] encoded. A reply is
//!   read at most [`REPLY_MAX`] bytes long. A refusal is a gRPC status whose message is one
//!   token ([`LinkError::Refused`]); an order the daemon does not run is logged once with why,
//!   and again when it changes.
//! - **The bound.** The whole call (dial, handshake, request and reply) takes at most
//!   [`CALL_SECS`] seconds.
//!
//! The link is synchronous, as the port is: it owns a Tokio runtime on the current thread and
//! blocks on it for each heartbeat. It must not be called, or dropped, from inside a Tokio
//! runtime.

use std::error::Error;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use hostkeeper_config::daemon::{DaemonConfig, Sha256Hex};
use hostkeeper_proto::hostkeeper::agent::v1::coordinator_client::CoordinatorClient;
use hostkeeper_proto::hostkeeper::agent::v1::{HeartbeatRequest, HeartbeatResponse};
use hyper_util::rt::TokioIo;
use prost::Message;
use rustls::ClientConfig;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::runtime::Runtime;
use tokio_rustls::TlsConnector;
use tonic::Code;
use tonic::codegen::http::Uri;
use tonic::transport::Endpoint as ChannelEndpoint;

use crate::beat::{Heartbeat, MAX_HEARTBEAT_BYTES};
use crate::dial::CALL_SECS;
use crate::mtls::{Endpoint, EndpointError, REPLY_MAX, client_config, failed};
use crate::names::Reason;
use crate::order::Reply;
use crate::pki::{CA, CURRENT};
use crate::ports::{ClientIdentity, CoordinatorLink, LinkError, Log, ReplyFault, Unsendable};
use crate::proto_map::{self, Dropped};

/// The protocol the link offers in ALPN: HTTP/2.
const H2: &[u8] = b"h2";

/// A byte stream to the coordinator, for the async side.
pub trait AsyncConn: AsyncRead + AsyncWrite + Send + Unpin {}

impl<T: AsyncRead + AsyncWrite + Send + Unpin + ?Sized> AsyncConn for T {}

/// The connection a [`GrpcDial`] is making.
pub type Dialing<'a> = Pin<Box<dyn Future<Output = io::Result<Box<dyn AsyncConn>>> + Send + 'a>>;

/// How the link reaches the coordinator's address: one connection per heartbeat.
pub trait GrpcDial: Send + Sync {
    /// A connection to `host` (a DNS name or an address, without brackets) on `port`. The
    /// future owns what it needs of `host`.
    ///
    /// # Errors
    /// The address cannot be resolved or reached.
    fn dial(&self, host: &str, port: u16) -> Dialing<'_>;
}

/// Why a [`GrpcMtlsLink`] could not be made.
#[derive(Debug, thiserror::Error)]
pub enum GrpcLinkError {
    #[error(transparent)]
    Endpoint(#[from] EndpointError),
    #[error("cannot start the runtime of the gRPC link: {0}")]
    Runtime(#[from] io::Error),
}

/// The [`CoordinatorLink`] over gRPC and mutual TLS.
pub struct GrpcMtlsLink {
    runtime: Runtime,
    endpoint: Endpoint,
    /// The address the HTTP/2 requests name, `http://` because the TLS is already done.
    origin: Uri,
    pin: Sha256Hex,
    ca_bundle: PathBuf,
    dial: Arc<dyn GrpcDial>,
    log: Arc<dyn Log>,
    /// The most a call takes.
    bound: Duration,
    /// The order last logged as not run, so that it is logged once.
    dropped: Mutex<Option<Dropped>>,
}

impl GrpcMtlsLink {
    /// The link to the coordinator of `config`, trusting the CA of the pki directory `pki`
    /// whose SHA-256 `config` pins, reached through `dial`, logging to `log`. Each call is
    /// bounded by [`CALL_SECS`].
    ///
    /// # Errors
    /// [`GrpcLinkError::Endpoint`]: the config's coordinator address is not one the link dials.
    /// [`GrpcLinkError::Runtime`]: the runtime cannot be started.
    pub fn new(
        config: &DaemonConfig,
        pki: &Path,
        dial: Arc<dyn GrpcDial>,
        log: Arc<dyn Log>,
    ) -> Result<GrpcMtlsLink, GrpcLinkError> {
        let endpoint = Endpoint::of(&config.coordinator)?;
        let origin = format!("http://{}", endpoint.authority)
            .parse::<Uri>()
            .map_err(|_| EndpointError::Host(config.coordinator.as_str().to_owned()))?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        Ok(GrpcMtlsLink {
            runtime,
            endpoint,
            origin,
            pin: config.ca_sha256.clone(),
            ca_bundle: pki.join(CURRENT).join(CA),
            dial,
            log,
            bound: Duration::from_secs(CALL_SECS),
            dropped: Mutex::new(None),
        })
    }

    /// The same link with every call bounded by `bound` instead of [`CALL_SECS`].
    #[must_use]
    pub fn bounded_by(self, bound: Duration) -> GrpcMtlsLink {
        GrpcMtlsLink { bound, ..self }
    }

    /// One heartbeat over a new connection: dial, TLS handshake, then the gRPC call on the
    /// established stream.
    async fn call(
        &self,
        config: Arc<ClientConfig>,
        request: HeartbeatRequest,
    ) -> Result<HeartbeatResponse, LinkError> {
        let sock = self
            .dial
            .dial(&self.endpoint.host, self.endpoint.port)
            .await
            .map_err(|e| LinkError::Transport(e.kind()))?;
        // The handshake first: what answered must be the coordinator before anything is said.
        let tls = TlsConnector::from(config)
            .connect(self.endpoint.name.clone(), sock)
            .await
            .map_err(|e| failed(&e))?;
        let mut stream = Some(TokioIo::new(tls));
        let connector = tower::service_fn(move |_: Uri| {
            let stream = stream.take();
            async move { stream.ok_or_else(|| io::Error::other("the connection was used")) }
        });
        let channel = ChannelEndpoint::from(self.origin.clone())
            .connect_with_connector(connector)
            .await
            .map_err(|e| LinkError::Transport(io_kind(&e).unwrap_or(io::ErrorKind::Other)))?;
        let mut client = CoordinatorClient::new(channel).max_decoding_message_size(REPLY_MAX);
        client
            .heartbeat(request)
            .await
            .map(tonic::Response::into_inner)
            .map_err(|status| refusal(&status))
    }

    /// Logs an order of the response that is not run, once, and again when it changes.
    fn note(&self, order: Option<Dropped>) {
        let mut dropped = self.dropped.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(order) = &order
            && dropped.as_ref() != Some(order)
        {
            self.log.line(&format!(
                "error: the coordinator's {} is not run: {}",
                order.what, order.why
            ));
        }
        *dropped = order;
    }
}

impl CoordinatorLink for GrpcMtlsLink {
    fn heartbeat(&self, identity: &ClientIdentity, beat: &Heartbeat) -> Result<Reply, LinkError> {
        let request = proto_map::heartbeat(beat).map_err(LinkError::Unsendable)?;
        let len = request.encoded_len();
        if len > MAX_HEARTBEAT_BYTES {
            return Err(LinkError::Unsendable(Unsendable::Bytes { len }));
        }
        let config = client_config(&self.ca_bundle, &self.pin, identity, H2)?;
        let response = self.runtime.block_on(async {
            tokio::time::timeout(self.bound, self.call(config, request))
                .await
                .unwrap_or(Err(LinkError::Transport(io::ErrorKind::TimedOut)))
        })?;
        let mapped = proto_map::reply(response);
        self.note(mapped.dropped);
        Ok(mapped.reply)
    }
}

/// The kind of the first `io::Error` in the chain of `error` and its sources.
fn io_kind(error: &(dyn Error + 'static)) -> Option<io::ErrorKind> {
    let mut at = Some(error);
    while let Some(current) = at {
        if let Some(io) = current.downcast_ref::<io::Error>() {
            return Some(io.kind());
        }
        at = current.source();
    }
    None
}

/// The link's error for a status the call ended with. The coordinator's refusal is a status whose
/// message is one token; anything else is the call failing: a transport error (its `io::Error`
/// when it has one), a coordinator that does not serve gRPC, a time-out, a reply too large to
/// read, or one that is no reply.
fn refusal(status: &tonic::Status) -> LinkError {
    if let Ok(reason) = Reason::new(status.message()) {
        return LinkError::Refused(reason);
    }
    if let Some(kind) = io_kind(status) {
        return LinkError::Transport(kind);
    }
    match status.code() {
        Code::Internal => LinkError::BadReply(ReplyFault::Malformed),
        Code::OutOfRange => LinkError::BadReply(ReplyFault::OverBound),
        Code::Unimplemented => LinkError::Transport(io::ErrorKind::Unsupported),
        Code::DeadlineExceeded => LinkError::Transport(io::ErrorKind::TimedOut),
        _ => LinkError::Transport(io::ErrorKind::Other),
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use hostkeeper_config::daemon::DaemonConfig;
    use tonic::{Code, Status};

    use super::{Dialing, Dropped, GrpcDial, GrpcMtlsLink, refusal};
    use crate::names::Reason;
    use crate::ports::{LinkError, Log, ReplyFault};

    /// A log that keeps its lines.
    #[derive(Default)]
    struct Lines(Mutex<Vec<String>>);

    impl Log for Lines {
        fn line(&self, text: &str) {
            self.0.lock().expect("the lines").push(text.to_owned());
        }
    }

    /// A dial that is never used.
    struct NoDial;

    impl GrpcDial for NoDial {
        fn dial(&self, _host: &str, _port: u16) -> Dialing<'_> {
            Box::pin(std::future::ready(Err(io::Error::from(
                io::ErrorKind::Unsupported,
            ))))
        }
    }

    /// An error that has a cause, as the chain of a transport error does.
    #[derive(Debug)]
    struct Wrapped(io::Error);

    impl std::fmt::Display for Wrapped {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("A wrapper of the failure")
        }
    }

    impl std::error::Error for Wrapped {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }

    fn link(log: &Arc<Lines>) -> GrpcMtlsLink {
        let text = format!(
            "coordinator = \"https://coordinator.test:8443\"\nca_sha256 = \"{}\"\n\
             release_channel = \"https://releases.test\"\n\n[trusted_signers]\n\
             organisation = \"komira-ai\"\nworkflow = \".github/workflows/release.yml\"\n\
             refs = [\"refs/tags/*\"]\nhosted_runner = true\n",
            "0".repeat(64)
        );
        let config = DaemonConfig::parse(&text).expect("a daemon config");
        GrpcMtlsLink::new(&config, Path::new("pki"), Arc::new(NoDial), log.clone()).expect("a link")
    }

    fn dropped(what: &str, why: &'static str) -> Dropped {
        Dropped {
            what: what.to_owned(),
            why,
        }
    }

    /// Proves a status whose message is one token is the coordinator's refusal by that token,
    /// whatever its code, and that a message that is not a token is not one.
    #[test]
    fn a_status_whose_message_is_one_token_is_the_coordinators_refusal() {
        for code in [
            Code::PermissionDenied,
            Code::InvalidArgument,
            Code::Internal,
        ] {
            assert_eq!(
                refusal(&Status::new(code, "forbidden")),
                LinkError::Refused(Reason::new("forbidden").expect("a reason"))
            );
        }
        assert_eq!(
            refusal(&Status::new(Code::PermissionDenied, "Forbidden: no")),
            LinkError::Transport(io::ErrorKind::Other)
        );
    }

    /// Proves a status caused by an I/O error, at any depth of its causes, is a transport error
    /// of that error's kind and not a reading of its code.
    #[test]
    fn a_status_caused_by_an_io_error_is_a_transport_error_of_its_kind() {
        let direct = Status::from_error(Box::new(io::Error::new(
            io::ErrorKind::ConnectionReset,
            "Connection reset by peer",
        )));
        assert_eq!(direct.code(), Code::Unknown);
        assert_eq!(
            refusal(&direct),
            LinkError::Transport(io::ErrorKind::ConnectionReset)
        );
        let nested = Status::from_error(Box::new(Wrapped(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "Broken pipe",
        ))));
        assert_eq!(
            refusal(&nested),
            LinkError::Transport(io::ErrorKind::BrokenPipe)
        );
    }

    /// Proves a status with no token and no I/O cause is read by its code: a reply that is no
    /// reply, one too large to read, a coordinator that does not serve gRPC, a time-out, and
    /// anything else a transport failure.
    #[test]
    fn a_status_with_neither_token_nor_cause_is_read_by_its_code() {
        let cases = [
            (Code::Internal, LinkError::BadReply(ReplyFault::Malformed)),
            (Code::OutOfRange, LinkError::BadReply(ReplyFault::OverBound)),
            (
                Code::Unimplemented,
                LinkError::Transport(io::ErrorKind::Unsupported),
            ),
            (
                Code::DeadlineExceeded,
                LinkError::Transport(io::ErrorKind::TimedOut),
            ),
            (
                Code::Unavailable,
                LinkError::Transport(io::ErrorKind::Other),
            ),
            (Code::Unknown, LinkError::Transport(io::ErrorKind::Other)),
        ];
        for (code, error) in cases {
            assert_eq!(refusal(&Status::new(code, "")), error, "{code:?}");
        }
    }

    /// Proves an order the daemon does not run is logged once, again when it changes, and again
    /// after a response that has none (the same order coming back is then new), and that a
    /// response with none logs nothing.
    #[test]
    fn an_order_not_run_is_logged_once_and_again_when_it_changes() {
        let log = Arc::new(Lines::default());
        let link = link(&log);
        let line =
            |what: &str, why: &str| format!("error: the coordinator's {what} is not run: {why}");
        link.note(None);
        assert_eq!(*log.0.lock().expect("the lines"), Vec::<String>::new());
        link.note(Some(dropped("order \"o-1\"", "a reason")));
        link.note(Some(dropped("order \"o-1\"", "a reason")));
        link.note(Some(dropped("order \"o-2\"", "a reason")));
        link.note(None);
        link.note(Some(dropped("order \"o-2\"", "a reason")));
        assert_eq!(
            *log.0.lock().expect("the lines"),
            [
                line("order \"o-1\"", "a reason"),
                line("order \"o-2\"", "a reason"),
                line("order \"o-2\"", "a reason"),
            ]
        );
    }
}
