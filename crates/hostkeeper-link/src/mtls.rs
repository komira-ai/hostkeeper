//! [`JsonMtlsLink`]: the loop's [`CoordinatorLink`] over the frozen JSON heartbeat route of
//! `hostkeeper-wire` (`POST /v1/heartbeat`) and rustls mutual TLS, because the coordinator serves
//! nothing else until the gRPC link lands (docs/design/hostkeeper-design.md, 12.2, the deviation
//! note). It speaks the bytes kbf's agent speaks (kbf `crates/kbf-updater/src/agent/coordinator.rs`
//! at `v0.0.64`): one POST per connection, HTTP/1.1, `Connection: close`.
//!
//! - **Who it talks to.** The coordinator's address is the daemon config's `coordinator`. Its
//!   certificate must chain to the CA whose SHA-256 is the config's `ca_sha256`: the roots are
//!   the certificates of the CA bundle in the pki directory's current generation
//!   (`<pki>/current/ca.pem`, which enrollment writes) whose SHA-256 is that one, read again at
//!   every heartbeat. A bundle that holds none is refused before anything is dialed. The
//!   certificate must be for the coordinator's name and be a server's (`serverAuth`); a host's
//!   certificate, which the CA makes for `clientAuth` only, is refused.
//! - **Who it is.** The client certificate and key of the heartbeat's [`ClientIdentity`], which
//!   the certificate source read for this heartbeat.
//! - **What it sends.** The loop's heartbeat as the wire's (the private module `wire_map`),
//!   refused by name when the wire has no word for one of its facts. The wire's grammars bound
//!   every fact, so the body is always under the wire's
//!   [`MAX_HEARTBEAT_BYTES`](hostkeeper_wire::wire::MAX_HEARTBEAT_BYTES), which the coordinator
//!   reads no more of.
//! - **What it takes back.** A 200 whose body is exactly an `Answer` of the wire: read strictly
//!   (a key other than `reply`, `service`, `version`, `features` and `halts` is
//!   [`ReplyFault::UnknownKey`]) and bounded (a body over [`REPLY_MAX`] or a head over
//!   [`HEAD_MAX`] is [`ReplyFault::OverBound`]). An answer of another status whose body is the
//!   coordinator's `{"error": <code>}` is [`LinkError::Refused`] by that code. An update in the
//!   answer is not given to the loop, because this route carries no package digest, and the link
//!   logs it once, and again when it changes.
//!
//! The link reaches the coordinator through a [`Transport`]: [`crate::TcpTransport`] in the
//! daemon, an in-process one in the tests.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_config::daemon::{DaemonConfig, HttpsUrl, Sha256Hex};
use hostkeeper_wire::wire::{Answer, HEARTBEAT_PATH};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::beat::Heartbeat;
use crate::names::Reason;
use crate::order::Reply;
use crate::pki::{CA, CURRENT};
use crate::ports::{ClientIdentity, CoordinatorLink, LinkError, Log, ReplyFault, TrustFault};
use crate::wire_map::{self, Update};

/// The most bytes of a reply's body that are read: kbf's `REPLY_MAX`.
pub const REPLY_MAX: usize = 16 * 1024;

/// The most bytes of a reply's status line and headers that are read.
pub const HEAD_MAX: usize = 8 * 1024;

/// The keys of an answer of the JSON route.
const ANSWER_KEYS: [&str; 5] = ["reply", "service", "version", "features", "halts"];

/// A byte stream to the coordinator.
pub trait Conn: Read + Write + Send {}

impl<T: Read + Write + Send + ?Sized> Conn for T {}

/// How the link reaches the coordinator's address: one connection per heartbeat.
pub trait Transport: Send + Sync {
    /// A connection to `host` (a DNS name or an address, without brackets) on `port`.
    ///
    /// # Errors
    /// The address cannot be resolved or reached.
    fn connect(&self, host: &str, port: u16) -> io::Result<Box<dyn Conn>>;
}

/// Why the daemon config's coordinator address is not one the link dials.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EndpointError {
    #[error(
        "the coordinator address {0:?} has a path: the heartbeat goes to {HEARTBEAT_PATH} at its root"
    )]
    Path(String),
    #[error("the coordinator address {0:?} has no host a certificate can be checked against")]
    Host(String),
}

/// Where the coordinator is: the host and port to dial, and the name its certificate must be
/// for.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Endpoint {
    /// `host:port` as the URL spells it, for the `Host` header.
    authority: String,
    host: String,
    port: u16,
    name: ServerName<'static>,
}

impl Endpoint {
    /// The endpoint of an `https://` URL of the daemon config: its host and its port (443
    /// when it names none); its path must be empty or `/`.
    ///
    /// # Errors
    /// [`EndpointError`]: a path, or a host that is no DNS name or address.
    fn of(url: &HttpsUrl) -> Result<Endpoint, EndpointError> {
        let text = url.as_str();
        let rest = text.strip_prefix("https://").unwrap_or(text);
        let (authority, path) = rest.find('/').map_or((rest, ""), |at| rest.split_at(at));
        if !matches!(path, "" | "/") {
            return Err(EndpointError::Path(text.to_owned()));
        }
        let host_error = || EndpointError::Host(text.to_owned());
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) if !port.contains(']') => {
                (host, port.parse::<u16>().map_err(|_| host_error())?)
            }
            _ => (authority, 443),
        };
        let bare = host
            .strip_prefix('[')
            .and_then(|inner| inner.strip_suffix(']'))
            .unwrap_or(host);
        let name = ServerName::try_from(bare.to_owned()).map_err(|_| host_error())?;
        Ok(Endpoint {
            authority: authority.to_owned(),
            host: bare.to_owned(),
            port,
            name,
        })
    }
}

/// The [`CoordinatorLink`] over the JSON heartbeat route and mutual TLS.
pub struct JsonMtlsLink {
    endpoint: Endpoint,
    pin: Sha256Hex,
    ca_bundle: PathBuf,
    transport: Arc<dyn Transport>,
    log: Arc<dyn Log>,
    /// The update last logged as not run, so that it is logged once.
    dropped: Mutex<Option<Update>>,
}

impl JsonMtlsLink {
    /// The link to the coordinator of `config`, trusting the CA of the pki directory `pki`
    /// whose SHA-256 `config` pins, reached through `transport`, logging to `log`.
    ///
    /// # Errors
    /// [`EndpointError`]: the config's coordinator address is not one the link dials.
    pub fn new(
        config: &DaemonConfig,
        pki: &Path,
        transport: Arc<dyn Transport>,
        log: Arc<dyn Log>,
    ) -> Result<JsonMtlsLink, EndpointError> {
        Ok(JsonMtlsLink {
            endpoint: Endpoint::of(&config.coordinator)?,
            pin: config.ca_sha256.clone(),
            ca_bundle: pki.join(CURRENT).join(CA),
            transport,
            log,
            dropped: Mutex::new(None),
        })
    }

    /// The TLS configuration of one heartbeat: the pinned CA as the only root, and `identity`.
    fn client_config(&self, identity: &ClientIdentity) -> Result<Arc<ClientConfig>, LinkError> {
        let bundle =
            fs::read(&self.ca_bundle).map_err(|_| LinkError::Untrusted(TrustFault::NoPinnedCa))?;
        let pinned: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(&bundle)
            .filter_map(Result::ok)
            .filter(|ca| sha256_hex(ca.as_ref()) == self.pin.as_str())
            .collect();
        let mut roots = RootCertStore::empty();
        roots.add_parsable_certificates(pinned);
        if roots.is_empty() {
            return Err(LinkError::Untrusted(TrustFault::NoPinnedCa));
        }
        let not_identity = |_| LinkError::Untrusted(TrustFault::ClientIdentity);
        let chain: Vec<CertificateDer<'static>> =
            CertificateDer::pem_slice_iter(identity.certificate())
                .collect::<Result<_, _>>()
                .map_err(not_identity)?;
        if chain.is_empty() {
            return Err(LinkError::Untrusted(TrustFault::ClientIdentity));
        }
        let key = PrivateKeyDer::from_pem_slice(identity.key()).map_err(not_identity)?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .and_then(|builder| {
                builder
                    .with_root_certificates(roots)
                    .with_client_auth_cert(chain, key)
            })
            .map_err(|_| LinkError::Untrusted(TrustFault::ClientIdentity))?;
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Arc::new(config))
    }

    /// Posts `body` over a new connection and returns every byte of the answer, up to the
    /// bound.
    fn exchange(&self, config: Arc<ClientConfig>, body: &[u8]) -> Result<Vec<u8>, LinkError> {
        let connection = ClientConnection::new(config, self.endpoint.name.clone())
            .map_err(|_| LinkError::Transport(io::ErrorKind::Other))?;
        let sock = self
            .transport
            .connect(&self.endpoint.host, self.endpoint.port)
            .map_err(|e| LinkError::Transport(e.kind()))?;
        let mut tls = StreamOwned::new(connection, sock);
        // The handshake first: what answered must be the coordinator before anything is said.
        while tls.conn.is_handshaking() {
            tls.conn
                .complete_io(&mut tls.sock)
                .map_err(|e| failed(&e))?;
        }
        let head = format!(
            "POST {HEARTBEAT_PATH} HTTP/1.1\r\nHost: {}\r\nAccept: application/json\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.endpoint.authority,
            body.len()
        );
        tls.write_all(head.as_bytes())
            .and_then(|()| tls.write_all(body))
            .and_then(|()| tls.flush())
            .map_err(|e| failed(&e))?;
        let limit = HEAD_MAX + REPLY_MAX;
        let mut raw = Vec::new();
        // A coordinator that closes without the TLS close is read as ended: the length of the
        // body decides whether the answer is whole.
        match Read::by_ref(&mut tls)
            .take(limit as u64 + 1)
            .read_to_end(&mut raw)
        {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {}
            Err(e) => return Err(failed(&e)),
        }
        if raw.len() > limit {
            return Err(LinkError::BadReply(ReplyFault::OverBound));
        }
        Ok(raw)
    }

    /// Logs an update of the answer that is not run, once, and again when it changes.
    fn note(&self, update: Option<Update>) {
        let mut dropped = self.dropped.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(update) = &update
            && dropped.as_ref() != Some(update)
        {
            let service = update
                .service
                .as_deref()
                .unwrap_or("the coordinator's legacy service");
            self.log.line(&format!(
                "error: the coordinator's update of {service} to {} is not run: the JSON \
                 heartbeat route carries no package digest",
                update.version.as_str()
            ));
        }
        *dropped = update;
    }
}

impl CoordinatorLink for JsonMtlsLink {
    fn heartbeat(&self, identity: &ClientIdentity, beat: &Heartbeat) -> Result<Reply, LinkError> {
        let wire = wire_map::heartbeat(beat).map_err(LinkError::Unsendable)?;
        // A heartbeat is strings, numbers and enums: it serializes. It is under the wire's bound
        // whatever its facts, because the wire's grammars bound every one of them
        // (`the_largest_heartbeat_the_json_route_carries_is_under_its_bound`).
        let body = serde_json::to_vec(&wire).unwrap_or_default();
        let config = self.client_config(identity)?;
        let raw = self.exchange(config, &body)?;
        let mapped = wire_map::reply(answer_of(&raw)?);
        self.note(mapped.update);
        Ok(mapped.reply)
    }
}

/// `bytes`' SHA-256 as lower-case hex.
fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A failure of the TLS stream: the server's certificate refused is the server not being the
/// coordinator; anything else is the connection failing.
fn failed(error: &io::Error) -> LinkError {
    let refused = error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<rustls::Error>())
        .is_some_and(|inner| matches!(inner, rustls::Error::InvalidCertificate(_)));
    if refused {
        LinkError::Untrusted(TrustFault::ServerCertificate)
    } else {
        LinkError::Transport(error.kind())
    }
}

/// The coordinator's refusal: `{"error": <code>}`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Refusal {
    error: String,
}

/// The answer in the HTTP response `raw`: a status line, headers with one `Content-Length` and
/// no `Transfer-Encoding`, and a body of exactly that length.
fn answer_of(raw: &[u8]) -> Result<Answer, LinkError> {
    let malformed = LinkError::BadReply(ReplyFault::Malformed);
    if raw.is_empty() {
        return Err(LinkError::Transport(io::ErrorKind::UnexpectedEof));
    }
    let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") else {
        return Err(malformed);
    };
    if end > HEAD_MAX {
        return Err(LinkError::BadReply(ReplyFault::OverBound));
    }
    let head = std::str::from_utf8(&raw[..end]).map_err(|_| malformed.clone())?;
    let body = &raw[end + 4..];
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.strip_prefix("HTTP/1.1 "))
        .and_then(|rest| rest.get(..3))
        .and_then(|code| code.parse::<u16>().ok());
    let Some(status) = status else {
        return Err(malformed);
    };
    let mut lengths = Vec::new();
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return Err(malformed);
        };
        match name.to_ascii_lowercase().as_str() {
            "transfer-encoding" => return Err(malformed),
            "content-length" => lengths.push(value.trim()),
            _ => {}
        }
    }
    let [length] = lengths[..] else {
        return Err(malformed);
    };
    let length: usize = length.parse().map_err(|_| malformed.clone())?;
    if length > REPLY_MAX {
        return Err(LinkError::BadReply(ReplyFault::OverBound));
    }
    if body.len() != length {
        return Err(malformed);
    }
    if status != 200 {
        return Err(serde_json::from_slice::<Refusal>(body)
            .ok()
            .and_then(|refusal| Reason::new(&refusal.error).ok())
            .map_or(malformed, LinkError::Refused));
    }
    let value: serde_json::Value = serde_json::from_slice(body).map_err(|_| malformed.clone())?;
    let Some(object) = value.as_object() else {
        return Err(malformed);
    };
    if object
        .keys()
        .any(|key| !ANSWER_KEYS.contains(&key.as_str()))
    {
        return Err(LinkError::BadReply(ReplyFault::UnknownKey));
    }
    // From the bytes again, so that a key given twice is refused as the wire's reader does.
    serde_json::from_slice::<Answer>(body).map_err(|_| malformed)
}
