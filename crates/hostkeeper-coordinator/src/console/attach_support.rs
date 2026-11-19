//! What the tests of the console's stream share: the console fixture with named operator tokens,
//! its coordinator shared as a listener shares it, a client that speaks the route's HTTP/1.1 and
//! then WebSocket over an in-memory connection served by hyper, and waits on the stream's end as
//! progress events (no clock and no sleep).

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hyper::Request;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream};

use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use super::support::{Fixture, caller};
use crate::http::{PeerAddr, Surface, handle};
use crate::service::Coordinator;
use crate::ssh_terminal_route::Shared;
use crate::tokens::OperatorTokens;
use crate::websocket::tests::client_frame;

/// The tokens of `alice` and `bob`, whom the console operators file names, and of `carol`, whom
/// it does not.
pub(crate) const ALICE: &str = "alice-token-0123456789abcdef0123456789";
pub(crate) const BOB: &str = "bob-token-0123456789abcdef0123456789";
pub(crate) const CAROL: &str = "carol-token-0123456789abcdef0123456789";

/// The address every test connection comes from.
pub(crate) const PEER: &str = "198.51.100.7:50123";

/// The key of the RFC's sample handshake.
pub(crate) const KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";

const PATIENCE: Duration = Duration::from_secs(10);

/// `token` as the route reads it from its subprotocol.
pub(crate) fn encoded(token: &str) -> String {
    URL_SAFE_NO_PAD.encode(token.as_bytes())
}

/// The tokens file of `names` with their tokens.
fn tokens_text(tokens: &[(&str, &str)]) -> String {
    tokens
        .iter()
        .map(|(name, token)| {
            let sha = Sha256::digest(token.as_bytes());
            let hex: String = sha.iter().map(|byte| format!("{byte:02x}")).collect();
            format!("[[token]]\nname = \"{name}\"\nsha256 = \"{hex}\"\n\n")
        })
        .collect()
}

/// A console fixture whose coordinator is shared, with the three named tokens.
pub(crate) struct Live {
    pub(crate) fixture: Fixture,
    pub(crate) coordinator: Arc<Coordinator>,
    tokens_path: std::path::PathBuf,
}

impl Live {
    /// The fixture `name` with at most `max` sessions, each attached within `window`.
    pub(crate) fn with(name: &str, max: usize, window: Duration) -> Self {
        let mut fixture = Fixture::with(name, max, window);
        let tokens_path = fixture.rig.dir.path().join("tokens.toml");
        let live = [("alice", ALICE), ("bob", BOB), ("carol", CAROL)];
        write_secret(&tokens_path, &tokens_text(&live));
        let mut coordinator = std::mem::replace(
            &mut fixture.rig.coordinator,
            crate::marks_support::start(&fixture.rig.dir, &fixture.rig.clock, ""),
        );
        coordinator.tokens = OperatorTokens::from_file(&tokens_path).expect("tokens");
        Self {
            fixture,
            coordinator: Arc::new(coordinator),
            tokens_path,
        }
    }

    /// The fixture `name` with the default bounds.
    pub(crate) fn new(name: &str) -> Self {
        Self::with(name, 8, Duration::from_secs(30))
    }

    /// The tokens file names only `tokens` from now on, read again.
    pub(crate) fn keep_tokens(&self, tokens: &[(&str, &str)]) {
        write_secret(&self.tokens_path, &tokens_text(tokens));
        self.coordinator.tokens.reload();
    }

    /// Opens a session of `host` as `by`: its id.
    pub(crate) fn open(&self, by: &str, host: &str) -> String {
        self.coordinator
            .console_open(
                &caller(by),
                &proto::OpenConsoleRequest {
                    host_id: host.to_owned(),
                    reason: "watch it boot".to_owned(),
                },
            )
            .expect("opened")
            .session
    }

    /// A connection to the coordinator's private surface, served by hyper with upgrades, as a
    /// listener serves one, from [`PEER`].
    pub(crate) fn connection(&self) -> DuplexStream {
        let (client, server) = tokio::io::duplex(1 << 20);
        let served = Arc::clone(&self.coordinator);
        tokio::spawn(async move {
            let service = service_fn(move |mut asked: Request<hyper::body::Incoming>| {
                let coordinator = Arc::clone(&served);
                asked
                    .extensions_mut()
                    .insert(Shared(Arc::clone(&coordinator)));
                let peer: SocketAddr = PEER.parse().expect("an address");
                asked.extensions_mut().insert(PeerAddr(peer));
                async move {
                    Ok::<_, Infallible>(
                        handle(&coordinator, Surface::Control, &None, asked, PATIENCE).await,
                    )
                }
            });
            let _ = http1::Builder::new()
                .serve_connection(TokioIo::new(server), service)
                .with_upgrades()
                .await;
        });
        client
    }

    /// Attaches to `session` with `token`: the client of the stream, once its `101` came.
    pub(crate) async fn attach(&self, session: &str, token: &str) -> Client<DuplexStream> {
        let mut stream = self.connection();
        let head = send(&mut stream, &upgrade(session, &good_headers(token))).await;
        assert!(head.starts_with("HTTP/1.1 101 "), "{head}");
        Client { stream }
    }

    /// Opens a session of `host` as `alice` and attaches it with no connection: the stream that
    /// would start, which nothing runs.
    pub(crate) fn attach_unserved(&self, host: &str) -> super::stream::Attached {
        let id = self.open("alice", host);
        let id = hostkeeper_console::session::SessionId::parse(&id).expect("an id");
        super::route::attach(&self.coordinator, &id, "alice", None).expect("attached")
    }

    /// Waits until the session of `host` has left the table and its close line is written.
    pub(crate) async fn gone(&self, host: &str) {
        loop {
            let ended = self.coordinator.console.ended.notified();
            if self
                .coordinator
                .console
                .lock_table()
                .sessions
                .get(host)
                .is_none()
            {
                return;
            }
            ended.await;
        }
    }

    /// The console's audit lines as `(by, action, node, reason)`, oldest first.
    pub(crate) fn lines(&self) -> Vec<(String, String, String, String)> {
        let all = self.coordinator.ops().audit_log().expect("the log");
        all.into_iter()
            .filter(|entry| crate::audit_actions::AuditOnly::is_audit_only(&entry.action))
            .map(|entry| {
                (
                    entry.by,
                    entry.action,
                    entry.node_id.unwrap_or_default(),
                    entry.reason.unwrap_or_default(),
                )
            })
            .collect()
    }

    /// The reason of the last console line.
    pub(crate) fn last_reason(&self) -> String {
        self.lines().pop().expect("a line").3
    }
}

/// Writes `text` to `path` with the mode of a secret file.
fn write_secret(path: &std::path::Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, text).expect("written");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).expect("its mode");
}

/// The headers of a good attach with `token`, from `https://hk.test`.
pub(crate) fn good_headers(token: &str) -> Vec<(&'static str, String)> {
    vec![
        ("Host", "hk.test".to_owned()),
        ("Origin", "https://hk.test".to_owned()),
        ("Upgrade", "websocket".to_owned()),
        ("Connection", "Upgrade".to_owned()),
        ("Sec-WebSocket-Version", "13".to_owned()),
        ("Sec-WebSocket-Key", KEY.to_owned()),
        (
            "Sec-WebSocket-Protocol",
            format!("hostkeeper.console.v1, bearer.{}", encoded(token)),
        ),
    ]
}

/// The request of an attach to `session` with `headers`.
pub(crate) fn upgrade(session: &str, headers: &[(&'static str, String)]) -> String {
    let mut text = format!("GET /console/v1/stream/{session} HTTP/1.1\r\n");
    for (name, value) in headers {
        text.push_str(&format!("{name}: {value}\r\n"));
    }
    text.push_str("\r\n");
    text
}

/// Sends `request` on `stream` and reads the head of its answer, up to the blank line; for an
/// answer other than `101`, its body too, after the head.
pub(crate) async fn send<S>(stream: &mut S, request: &str) -> String
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    stream.write_all(request.as_bytes()).await.expect("sent");
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut byte = [0u8; 1];
        stream.read_exact(&mut byte).await.expect("the head");
        head.push(byte[0]);
    }
    let mut head = String::from_utf8(head).expect("UTF-8");
    let length = head.lines().find_map(|line| {
        let (name, value) = line.split_once(": ")?;
        name.eq_ignore_ascii_case("content-length")
            .then(|| value.parse::<usize>().ok())?
    });
    if let Some(length) = length {
        let mut body = vec![0u8; length];
        stream.read_exact(&mut body).await.expect("the body");
        head.push_str(&String::from_utf8(body).expect("UTF-8"));
    }
    head
}

/// A frame the coordinator sent.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Frame {
    Binary(Vec<u8>),
    Pong(Vec<u8>),
    /// A close frame: its code and reason.
    Close(u16, String),
    /// The connection ended.
    End,
}

/// The client's end of a stream.
pub(crate) struct Client<S> {
    pub(crate) stream: S,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Client<S> {
    /// Sends a binary message.
    pub(crate) async fn binary(&mut self, bytes: &[u8]) {
        self.raw(&client_frame(2, bytes)).await;
    }

    /// Sends a text message.
    pub(crate) async fn text(&mut self, text: &str) {
        self.raw(&client_frame(1, text.as_bytes())).await;
    }

    /// Sends the bytes of a frame as they are.
    pub(crate) async fn raw(&mut self, bytes: &[u8]) {
        self.stream.write_all(bytes).await.expect("sent");
    }

    /// The next frame, or [`Frame::End`].
    pub(crate) async fn next(&mut self) -> Frame {
        let mut head = [0u8; 2];
        if self.stream.read_exact(&mut head).await.is_err() {
            return Frame::End;
        }
        assert_eq!(head[1] & 0x80, 0, "a server frame is not masked");
        let length = match head[1] & 0x7f {
            126 => {
                let mut long = [0u8; 2];
                self.stream.read_exact(&mut long).await.expect("a length");
                usize::from(u16::from_be_bytes(long))
            }
            127 => {
                let mut long = [0u8; 8];
                self.stream.read_exact(&mut long).await.expect("a length");
                usize::try_from(u64::from_be_bytes(long)).expect("a length")
            }
            short => usize::from(short),
        };
        let mut payload = vec![0u8; length];
        self.stream
            .read_exact(&mut payload)
            .await
            .expect("a payload");
        match head[0] {
            0x82 => Frame::Binary(payload),
            0x8a => Frame::Pong(payload),
            0x88 => Frame::Close(
                u16::from_be_bytes([payload[0], payload[1]]),
                String::from_utf8(payload[2..].to_vec()).expect("UTF-8"),
            ),
            other => panic!("a frame that is none of binary, pong and close: {other:#x}"),
        }
    }

    /// Reads binary messages until `count` bytes came: the bytes.
    pub(crate) async fn output(&mut self, count: usize) -> Vec<u8> {
        let mut all = Vec::new();
        while all.len() < count {
            match self.next().await {
                Frame::Binary(bytes) => {
                    assert!(bytes.len() <= 16 * 1024, "a message of {}", bytes.len());
                    all.extend(bytes);
                }
                other => panic!("output was expected, not {other:?}"),
            }
        }
        all
    }

    /// Reads to the close frame, which must have `code` and `reason`, and then the end of the
    /// connection: the output before it.
    pub(crate) async fn closed(&mut self, code: u16, reason: &str) -> Vec<u8> {
        let mut before = Vec::new();
        loop {
            match self.next().await {
                Frame::Binary(bytes) => before.extend(bytes),
                Frame::Close(got, why) => {
                    assert_eq!((got, why.as_str()), (code, reason));
                    break;
                }
                other => panic!("a close was expected, not {other:?}"),
            }
        }
        assert_eq!(self.next().await, Frame::End);
        before
    }
}
