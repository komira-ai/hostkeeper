//! The server side of RFC 6455, as much as the UI's terminal needs ([`crate::ssh_terminal`]):
//! the opening handshake's checks and its accept key, the frames a browser sends, and the frames
//! the coordinator answers with.
//!
//! What is taken: a request whose `Upgrade` is `websocket`, whose `Connection` has the token
//! `upgrade`, whose `Sec-WebSocket-Version` is `13` and whose `Sec-WebSocket-Key` is 16 bytes in
//! base64; masked, unfragmented text and binary messages of at most the limit the reader is
//! given; ping (answered by the caller with the same payload), pong (dropped) and close. What is
//! refused, as a [`ReadError::Protocol`]: an unmasked frame, a reserved bit, an extension opcode,
//! a continuation or a message of several frames, a control frame that is fragmented or longer
//! than 125 bytes, and a text message that is not UTF-8. No extension is negotiated, so a browser
//! sends no compressed frame. A message longer than the limit is [`ReadError::TooBig`], and
//! nothing of it is read past its header.

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hyper::HeaderMap;
use hyper::header::{CONNECTION, UPGRADE};
use sha1::{Digest as _, Sha1};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::Mutex;

/// The GUID RFC 6455 appends to the client's key to make the accept key.
const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// The most payload bytes a control frame has.
const MAX_CONTROL: usize = 125;

/// The close status of a normal closure.
pub(crate) const CLOSE_NORMAL: u16 = 1000;

/// A message the browser sent.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Message {
    /// A text message.
    Text(String),
    /// A binary message.
    Binary(Vec<u8>),
    /// A ping, to be answered with a pong of the same payload.
    Ping(Vec<u8>),
    /// A close frame: the browser is done.
    Close,
}

/// Why no message could be read.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ReadError {
    /// The transport ended or failed.
    #[error("the connection ended")]
    Ended,
    /// The browser broke the protocol.
    #[error("websocket protocol error: {0}")]
    Protocol(&'static str),
    /// A message longer than the limit.
    #[error("a message longer than the limit")]
    TooBig,
}

/// Why a request is no WebSocket upgrade.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum BadUpgrade {
    /// `Upgrade` is missing or is not `websocket`.
    #[error("no Upgrade: websocket")]
    Upgrade,
    /// `Connection` lacks the `upgrade` token.
    #[error("no Connection: upgrade")]
    Connection,
    /// `Sec-WebSocket-Version` is missing or not 13.
    #[error("no Sec-WebSocket-Version: 13")]
    Version,
    /// `Sec-WebSocket-Key` is missing or not 16 bytes in base64.
    #[error("no valid Sec-WebSocket-Key")]
    Key,
}

/// The `Sec-WebSocket-Accept` value for the client's `key`.
pub(crate) fn accept_key(key: &str) -> String {
    let mut hash = Sha1::new();
    hash.update(key.as_bytes());
    hash.update(GUID.as_bytes());
    STANDARD.encode(hash.finalize())
}

/// Whether the header `name` of `headers` has `token` among its comma-separated values, in any
/// case.
fn has_token(headers: &HeaderMap, name: hyper::header::HeaderName, token: &str) -> bool {
    headers
        .get_all(name)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|part| part.trim().eq_ignore_ascii_case(token))
}

/// The accept key for a request whose headers `headers` are a WebSocket opening handshake.
///
/// # Errors
/// [`BadUpgrade`]: the first check that failed.
pub(crate) fn handshake(headers: &HeaderMap) -> Result<String, BadUpgrade> {
    if !has_token(headers, UPGRADE, "websocket") {
        return Err(BadUpgrade::Upgrade);
    }
    if !has_token(headers, CONNECTION, "upgrade") {
        return Err(BadUpgrade::Connection);
    }
    if headers
        .get("sec-websocket-version")
        .is_none_or(|version| version.as_bytes() != b"13")
    {
        return Err(BadUpgrade::Version);
    }
    let key = headers
        .get("sec-websocket-key")
        .and_then(|key| key.to_str().ok())
        .filter(|key| {
            STANDARD
                .decode(key.as_bytes())
                .is_ok_and(|decoded| decoded.len() == 16)
        })
        .ok_or(BadUpgrade::Key)?;
    Ok(accept_key(key))
}

/// The frames of a browser, read one message at a time.
pub(crate) struct Reader<R> {
    inner: R,
    limit: usize,
}

impl<R: AsyncRead + Unpin> Reader<R> {
    /// A reader of `inner` that refuses a data message of more than `limit` bytes.
    pub(crate) fn new(inner: R, limit: usize) -> Self {
        Self { inner, limit }
    }

    async fn bytes<const N: usize>(&mut self) -> Result<[u8; N], ReadError> {
        let mut buffer = [0_u8; N];
        self.inner
            .read_exact(&mut buffer)
            .await
            .map_err(|_| ReadError::Ended)?;
        Ok(buffer)
    }

    /// The next message: a pong is skipped.
    ///
    /// Not safe to cancel between its first byte and its last: a caller reads in a task of its
    /// own.
    ///
    /// # Errors
    /// [`ReadError`]: the stream ended, the browser broke the protocol, or a message was too long.
    pub(crate) async fn next(&mut self) -> Result<Message, ReadError> {
        loop {
            let [first, second] = self.bytes::<2>().await?;
            if first & 0x70 != 0 {
                return Err(ReadError::Protocol("a reserved bit is set"));
            }
            if second & 0x80 == 0 {
                return Err(ReadError::Protocol("a frame is not masked"));
            }
            let opcode = first & 0x0f;
            let control = opcode & 0x08 != 0;
            if first & 0x80 == 0 {
                return Err(ReadError::Protocol("a message is fragmented"));
            }
            let length = match second & 0x7f {
                126 => usize::from(u16::from_be_bytes(self.bytes::<2>().await?)),
                127 => usize::try_from(u64::from_be_bytes(self.bytes::<8>().await?))
                    .unwrap_or(usize::MAX),
                short => usize::from(short),
            };
            if control && length > MAX_CONTROL {
                return Err(ReadError::Protocol("a control frame is too long"));
            }
            if !control && length > self.limit {
                return Err(ReadError::TooBig);
            }
            let mask = self.bytes::<4>().await?;
            let mut payload = vec![0_u8; length];
            self.inner
                .read_exact(&mut payload)
                .await
                .map_err(|_| ReadError::Ended)?;
            for (at, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[at % 4];
            }
            match opcode {
                0x1 => {
                    return String::from_utf8(payload)
                        .map(Message::Text)
                        .map_err(|_| ReadError::Protocol("a text message is not UTF-8"));
                }
                0x2 => return Ok(Message::Binary(payload)),
                0x8 => return Ok(Message::Close),
                0x9 => return Ok(Message::Ping(payload)),
                0xa => {}
                _ => return Err(ReadError::Protocol("an opcode that is not taken")),
            }
        }
    }
}

/// The frames the coordinator sends, unmasked.
pub(crate) struct Writer<W> {
    inner: W,
}

impl<W: AsyncWrite + Unpin> Writer<W> {
    /// A writer of frames to `inner`.
    pub(crate) fn new(inner: W) -> Self {
        Self { inner }
    }

    /// One frame of `opcode` with `payload`, flushed.
    async fn frame(&mut self, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
        let mut head = vec![0x80 | opcode];
        match payload.len() {
            short @ 0..=125 => head.push(u8::try_from(short).unwrap_or(0)),
            medium @ 126..=65535 => {
                head.push(126);
                head.extend_from_slice(&u16::try_from(medium).unwrap_or(0).to_be_bytes());
            }
            long => {
                head.push(127);
                head.extend_from_slice(&u64::try_from(long).unwrap_or(0).to_be_bytes());
            }
        }
        self.inner.write_all(&head).await?;
        self.inner.write_all(payload).await?;
        self.inner.flush().await
    }
}

/// The writer of a connection, shared by whoever sends on it (the terminal's output, and the
/// reader's answers to a ping): one message at a time.
pub(crate) struct Outbox<W>(Arc<Mutex<Writer<W>>>);

impl<W> Clone for Outbox<W> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<W: AsyncWrite + Unpin> Outbox<W> {
    /// The shared writer of `writer`.
    pub(crate) fn new(writer: Writer<W>) -> Self {
        Self(Arc::new(Mutex::new(writer)))
    }

    /// Sends a text message.
    ///
    /// # Errors
    /// The transport failed.
    pub(crate) async fn text(&self, text: &str) -> std::io::Result<()> {
        self.0.lock().await.frame(0x1, text.as_bytes()).await
    }

    /// Sends a binary message.
    ///
    /// # Errors
    /// The transport failed.
    pub(crate) async fn binary(&self, bytes: &[u8]) -> std::io::Result<()> {
        self.0.lock().await.frame(0x2, bytes).await
    }

    /// Answers a ping with its payload.
    ///
    /// # Errors
    /// The transport failed.
    pub(crate) async fn pong(&self, payload: &[u8]) -> std::io::Result<()> {
        self.0.lock().await.frame(0xa, payload).await
    }

    /// Sends a close frame with `status`, and ends the write side of the transport.
    ///
    /// # Errors
    /// The transport failed.
    pub(crate) async fn close(&self, status: u16) -> std::io::Result<()> {
        let mut writer = self.0.lock().await;
        writer.frame(0x8, &status.to_be_bytes()).await?;
        writer.inner.shutdown().await
    }
}

#[cfg(test)]
#[path = "websocket_tests.rs"]
pub(crate) mod tests;
