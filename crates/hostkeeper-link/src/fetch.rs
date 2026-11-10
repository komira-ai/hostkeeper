//! [`CoordinatorGet`]: one `GET` of a path of the coordinator's agent listener, over the same
//! mutual TLS as the heartbeat, with the body streamed to the caller's sink instead of held.
//! The daemon's mirror of a private repository reads its files this way, and so will the Mac's
//! `coordinator` store.
//!
//! - **Who it talks to, who it is.** As [`crate::JsonMtlsLink`]: the coordinator of the daemon
//!   config, trusted by the certificate of the pki directory's CA bundle that the config pins,
//!   read again at every call; the client certificate is the [`CertSource`]'s, read again at
//!   every call. A host that is not enrolled makes no call.
//! - **What it sends.** `GET <path> HTTP/1.1` with `Connection: close`, one request per
//!   connection. The path is ASCII graphic bytes from a `/`.
//! - **What it takes back.** A status line and headers of at most [`HEAD_MAX`] bytes, with no
//!   `Transfer-Encoding`. A 404 is [`Got::NotFound`]; another status other than 200 is
//!   [`GetError::Status`]. A 200 must carry exactly one `Content-Length`, which must not pass
//!   the caller's [`Limit::max_bytes`]; its body goes to the sink in chunks as it arrives, and
//!   a body shorter or longer than its length is refused.
//! - **How long it may take.** [`Limit::deadline_secs`] on the injected clock, from the start of
//!   the call, checked before every read; the connection is made with the same bound
//!   ([`Transport::connect_within`]), so a read or write that blocks ends at it too.

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;

use hostkeeper_config::daemon::{DaemonConfig, Sha256Hex};
use hostkeeper_steps::Clock;
use rustls::{ClientConnection, StreamOwned};

use crate::mtls::{Endpoint, EndpointError, HEAD_MAX, Transport, client_config, failed};
use crate::pki::{CA, CURRENT};
use crate::ports::{CertSource, CertState, LinkError, ReplyFault};

/// The size of one read of a body: the chunk the sink is given, at most.
const CHUNK_BYTES: usize = 64 * 1024;

/// The most bytes of a path the call sends.
const PATH_MAX: usize = 2048;

/// What one call may use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limit {
    /// The most bytes of body the call takes; a `Content-Length` over it is refused before
    /// the body is read.
    pub max_bytes: u64,
    /// Seconds from the start of the call, on the clock the call was made with, after which it
    /// is ended.
    pub deadline_secs: u64,
}

/// What the coordinator answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Got {
    /// A 200; its body, this many bytes, went to the sink.
    Body(u64),
    /// A 404: the coordinator has no such file for this host.
    NotFound,
}

/// Why a call got no body.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GetError {
    #[error("the path {0:?} is not a path the call sends")]
    Path(String),
    #[error("the host has no certificate to present")]
    NotEnrolled,
    /// The coordinator could not be reached, was not the coordinator, or answered with a head
    /// that is no HTTP/1.1 answer this call reads.
    #[error("{0}")]
    Link(LinkError),
    #[error("the coordinator answered {0}")]
    Status(u16),
    #[error("the body is {length} bytes, more than the {max} the call takes")]
    TooLong { length: u64, max: u64 },
    #[error("the connection ended before the whole body came")]
    Truncated,
    #[error("the call took longer than its bound")]
    Deadline,
    /// The caller's sink refused a chunk.
    #[error("the sink refused the body: {0}")]
    Sink(io::ErrorKind),
}

/// The client of the coordinator's `GET` paths.
pub struct CoordinatorGet {
    endpoint: Endpoint,
    pin: Sha256Hex,
    ca_bundle: PathBuf,
    certs: Arc<dyn CertSource>,
    transport: Arc<dyn Transport>,
    clock: Arc<dyn Clock>,
}

impl CoordinatorGet {
    /// The client of the coordinator of `config`, trusting the CA of the pki directory `pki`
    /// that `config` pins, presenting the certificate of `certs`, reaching the coordinator
    /// through `transport` and keeping time by `clock`.
    ///
    /// # Errors
    /// [`EndpointError`]: the config's coordinator address is not one the client dials.
    pub fn new(
        config: &DaemonConfig,
        pki: &std::path::Path,
        certs: Arc<dyn CertSource>,
        transport: Arc<dyn Transport>,
        clock: Arc<dyn Clock>,
    ) -> Result<CoordinatorGet, EndpointError> {
        Ok(CoordinatorGet {
            endpoint: Endpoint::of(&config.coordinator)?,
            pin: config.ca_sha256.clone(),
            ca_bundle: pki.join(CURRENT).join(CA),
            certs,
            transport,
            clock,
        })
    }

    /// Reads `path` from the coordinator, giving the body to `sink` in chunks.
    ///
    /// # Errors
    /// [`GetError`]: see its variants. A sink that fails ends the call with
    /// [`GetError::Sink`]; what it was already given stays with the caller.
    pub fn get(
        &self,
        path: &str,
        limit: Limit,
        sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> Result<Got, GetError> {
        if !path_ok(path) {
            return Err(GetError::Path(path.chars().take(64).collect()));
        }
        let CertState::Enrolled(identity) = self.certs.current() else {
            return Err(GetError::NotEnrolled);
        };
        let deadline = self.clock.mono_secs().saturating_add(limit.deadline_secs);
        let config = client_config(&self.ca_bundle, &self.pin, &identity, b"http/1.1")
            .map_err(GetError::Link)?;
        let connection = ClientConnection::new(config, self.endpoint.name.clone())
            .map_err(|_| GetError::Link(LinkError::Transport(io::ErrorKind::Other)))?;
        let sock = self
            .transport
            .connect_within(&self.endpoint.host, self.endpoint.port, limit.deadline_secs)
            .map_err(|e| GetError::Link(LinkError::Transport(e.kind())))?;
        let mut tls = StreamOwned::new(connection, sock);
        // The handshake first: what answered must be the coordinator before anything is said.
        while tls.conn.is_handshaking() {
            tls.conn
                .complete_io(&mut tls.sock)
                .map_err(|e| GetError::Link(failed(&e)))?;
        }
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {}\r\nAccept: */*\r\nConnection: close\r\n\r\n",
            self.endpoint.authority
        );
        tls.write_all(request.as_bytes())
            .and_then(|()| tls.flush())
            .map_err(|e| GetError::Link(failed(&e)))?;
        let (head, rest) = self.read_head(&mut tls, deadline)?;
        let length = match head.status {
            404 => return Ok(Got::NotFound),
            200 => head
                .length
                .ok_or(GetError::Link(LinkError::BadReply(ReplyFault::Malformed)))?,
            other => return Err(GetError::Status(other)),
        };
        if length > limit.max_bytes {
            return Err(GetError::TooLong {
                length,
                max: limit.max_bytes,
            });
        }
        self.read_body(&mut tls, rest, length, deadline, sink)?;
        Ok(Got::Body(length))
    }

    /// Fails when the call's deadline has passed.
    fn within(&self, deadline: u64) -> Result<(), GetError> {
        if self.clock.mono_secs() >= deadline {
            Err(GetError::Deadline)
        } else {
            Ok(())
        }
    }

    /// The head of the answer and the body bytes that came with it.
    fn read_head(&self, tls: &mut dyn Read, deadline: u64) -> Result<(Head, Vec<u8>), GetError> {
        let mut raw: Vec<u8> = Vec::new();
        let mut chunk = [0_u8; 4096];
        let end = loop {
            if let Some(at) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                break at;
            }
            if raw.len() > HEAD_MAX {
                return Err(GetError::Link(LinkError::BadReply(ReplyFault::OverBound)));
            }
            self.within(deadline)?;
            match tls.read(&mut chunk) {
                Ok(0) => {
                    return Err(GetError::Link(LinkError::Transport(
                        io::ErrorKind::UnexpectedEof,
                    )));
                }
                Ok(n) => raw.extend_from_slice(&chunk[..n]),
                Err(e) => return Err(GetError::Link(failed(&e))),
            }
        };
        if end > HEAD_MAX {
            return Err(GetError::Link(LinkError::BadReply(ReplyFault::OverBound)));
        }
        let head = Head::parse(&raw[..end])
            .ok_or(GetError::Link(LinkError::BadReply(ReplyFault::Malformed)))?;
        Ok((head, raw[end + 4..].to_vec()))
    }

    /// Gives the sink `length` bytes of body: `first`, already read, then the stream.
    fn read_body(
        &self,
        tls: &mut dyn Read,
        first: Vec<u8>,
        length: u64,
        deadline: u64,
        sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> Result<(), GetError> {
        let mut given = 0_u64;
        give(&mut given, length, &first, sink)?;
        let mut chunk = vec![0_u8; CHUNK_BYTES];
        while given < length {
            self.within(deadline)?;
            let want = usize::try_from(length - given).map_or(CHUNK_BYTES, |n| n.min(CHUNK_BYTES));
            match tls.read(&mut chunk[..want]) {
                Ok(0) => return Err(GetError::Truncated),
                Ok(n) => give(&mut given, length, &chunk[..n], sink)?,
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                    return Err(GetError::Truncated);
                }
                Err(e) => return Err(GetError::Link(failed(&e))),
            }
        }
        Ok(())
    }
}

/// Gives `bytes` to `sink` and counts them in `given`; more than `length` in all is a body
/// longer than its head said.
fn give(
    given: &mut u64,
    length: u64,
    bytes: &[u8],
    sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
) -> Result<(), GetError> {
    *given += bytes.len() as u64;
    if *given > length {
        return Err(GetError::Link(LinkError::BadReply(ReplyFault::Malformed)));
    }
    if bytes.is_empty() {
        return Ok(());
    }
    sink(bytes).map_err(|e| GetError::Sink(e.kind()))
}

/// A path the call sends: a `/`, then ASCII graphic bytes, at most [`PATH_MAX`].
fn path_ok(path: &str) -> bool {
    path.starts_with('/') && path.len() <= PATH_MAX && path.bytes().all(|b| b.is_ascii_graphic())
}

/// The status and the framing of an answer's head.
struct Head {
    status: u16,
    /// The one `Content-Length`; none when the head gave none.
    length: Option<u64>,
}

impl Head {
    /// The head `raw` (without the blank line): `HTTP/1.1 <code> ...`, headers of `name:
    /// value`, no `Transfer-Encoding`, at most one `Content-Length`. None when it is not that.
    fn parse(raw: &[u8]) -> Option<Head> {
        let text = std::str::from_utf8(raw).ok()?;
        let mut lines = text.split("\r\n");
        let status = lines
            .next()?
            .strip_prefix("HTTP/1.1 ")?
            .get(..3)?
            .parse::<u16>()
            .ok()?;
        let mut length = None;
        for line in lines {
            let (name, value) = line.split_once(':')?;
            match name.to_ascii_lowercase().as_str() {
                "transfer-encoding" => return None,
                "content-length" => {
                    if length.is_some() {
                        return None;
                    }
                    length = Some(value.trim().parse::<u64>().ok()?);
                }
                _ => {}
            }
        }
        Some(Head { status, length })
    }
}
