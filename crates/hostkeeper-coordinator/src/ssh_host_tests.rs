//! What the tests of the UI's terminal share: a real SSH server standing for a host's sshd (the
//! `russh` server, on a loopback socket of port 0 that the tunnel dials), a coordinator that holds
//! a pinned host key for it, a signed-in session, and a browser that speaks WebSocket over an
//! in-memory pipe.
//!
//! The server takes a certificate only when the test's CA signed it, it is a user certificate, it
//! is valid at the ledger's time and its principal is `hk-host-<host id>`; it answers a pty and a
//! shell, and then echoes what it is typed, tells a window change (`winch <cols>x<rows>`), ends the
//! shell on `exit`, writes to standard error on `err`, and writes 40,000 bytes on `big`. A host can
//! instead flood output before it answers the shell request, or hold its input.

use std::sync::Arc;
use std::time::Duration;

use russh::keys::ssh_key::private::Ed25519Keypair;
use russh::keys::ssh_key::{Fingerprint, HashAlg, PublicKey};
use russh::keys::{Certificate, PrivateKey};
use russh::server::{self, Auth, Msg, Session};
use russh::{Channel, ChannelId};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};

use crate::session::{SessionKey, Sessions};
use crate::ssh_cert::fingerprint;
use crate::ssh_sign::tests::Tune;
use crate::ssh_terminal::{Start, serve};
use crate::ssh_tunnel::tests::Rig;
use crate::websocket::tests::client_frame;

/// The host the terminals of the tests reach.
pub(crate) const HOST: &str = "node-07";

/// How the host behaves.
#[derive(Clone, Debug)]
pub(crate) struct Behavior {
    /// Whether the host trusts the CA that signed the terminal's certificate.
    pub(crate) trusts_ca: bool,
    /// Whether the pty request is refused.
    pub(crate) refuse_pty: bool,
    /// Whether the shell request is refused.
    pub(crate) refuse_shell: bool,
    /// Whether the channel is closed in answer to the shell request.
    pub(crate) close_on_shell: bool,
    /// Whether the shell prints `welcome` before it answers the request.
    pub(crate) greets: bool,
    /// Whether the shell prints `later` after it answered the request, unasked.
    pub(crate) speaks_after: bool,
    /// Whether the channel is refused when it is opened.
    pub(crate) refuse_channel: bool,
    /// Whether the shell sends 128 KiB of output before it answers the request, and never
    /// answers.
    pub(crate) floods: bool,
    /// A host that reads no input until the watched value is `true`, and whose window for it is
    /// 4 KiB.
    pub(crate) holds_input: Option<watch::Receiver<bool>>,
}

impl Behavior {
    /// A host that does everything the terminal asks.
    pub(crate) fn good() -> Self {
        Self {
            trusts_ca: true,
            refuse_pty: false,
            refuse_shell: false,
            close_on_shell: false,
            greets: true,
            speaks_after: false,
            refuse_channel: false,
            floods: false,
            holds_input: None,
        }
    }
}

/// What the host's sshd saw.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Seen {
    /// A certificate login: user, serial, key id and whether it was accepted.
    Login(String, u64, String, bool),
    /// The pty request: the terminal type and size.
    Pty(String, u32, u32),
}

/// What the dialled socket is.
pub(crate) enum Kind {
    /// An SSH server of this behavior.
    Ssh(Behavior),
    /// A socket that is accepted and read from by nobody.
    Stall,
    /// A socket that is accepted and closed at once.
    HangUp,
}

struct Sshd {
    /// The time of the ledger that signed the certificate, which the server judges it by.
    now: u64,
    host_id: String,
    ca: Option<Fingerprint>,
    behavior: Behavior,
    seen: mpsc::UnboundedSender<Seen>,
}

impl server::Handler for Sshd {
    type Error = russh::Error;

    async fn auth_openssh_certificate(
        &mut self,
        user: &str,
        certificate: &Certificate,
    ) -> Result<Auth, Self::Error> {
        let signed_by_ca = self
            .ca
            .as_ref()
            .is_some_and(|ca| certificate.validate_at(self.now, [ca]).is_ok());
        let ok = signed_by_ca
            && certificate.cert_type().is_user()
            && certificate
                .valid_principals()
                .iter()
                .any(|principal| principal == &format!("hk-host-{}", self.host_id));
        let _ = self.seen.send(Seen::Login(
            user.to_owned(),
            certificate.serial(),
            certificate.key_id().to_owned(),
            ok,
        ));
        Ok(if ok { Auth::Accept } else { Auth::reject() })
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if !self.behavior.refuse_channel {
            reply.accept().await;
        }
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        term: &str,
        columns: u32,
        rows: u32,
        _pixel_width: u32,
        _pixel_height: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let _ = self.seen.send(Seen::Pty(term.to_owned(), columns, rows));
        if self.behavior.refuse_pty {
            session.channel_failure(channel)
        } else {
            session.channel_success(channel)
        }
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.behavior.refuse_shell {
            return session.channel_failure(channel);
        }
        if self.behavior.close_on_shell {
            return session.close(channel);
        }
        if self.behavior.floods {
            for _ in 0..8 {
                session.data(channel, vec![b'x'; 16 * 1024])?;
            }
            return Ok(());
        }
        if self.behavior.greets {
            // Output before the reply, as a shell that prints its banner at once does.
            session.data(channel, &b"welcome\r\n"[..])?;
            // And a word the client has no use for before the reply, which it passes over.
            session.exit_status_request(channel, 0)?;
        }
        session.channel_success(channel)?;
        if self.behavior.speaks_after {
            session.data(channel, &b"later\r\n"[..])?;
        }
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if let Some(open) = &self.behavior.holds_input {
            // The host's session is stuck here: nothing more is read from the connection.
            let mut open = open.clone();
            let _ = open.wait_for(|open| *open).await;
        }
        match data {
            b"exit\r" => {
                session.exit_status_request(channel, 0)?;
                session.eof(channel)?;
                session.close(channel)
            }
            b"err\r" => session.extended_data(channel, 1, &b"stderr\r\n"[..]),
            b"big\r" => session.data(channel, vec![b'x'; 40_000]),
            other => session.data(channel, other.to_vec()),
        }
    }

    async fn window_change_request(
        &mut self,
        channel: ChannelId,
        columns: u32,
        rows: u32,
        _pixel_width: u32,
        _pixel_height: u32,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.data(channel, format!("winch {columns}x{rows}\r\n").into_bytes())
    }
}

/// The key of the host's sshd: always the same, so a test can pin it.
fn host_key() -> PrivateKey {
    PrivateKey::from(Ed25519Keypair::from_seed(&[9_u8; 32]))
}

/// The `SHA256:` fingerprint of the host's key, as the pin holds it.
pub(crate) fn host_fingerprint() -> String {
    fingerprint(&host_key().public_key().to_bytes().expect("a key blob"))
}

/// A coordinator with a host to reach, a signed-in session of `operator`, and what the host saw.
pub(crate) struct Site {
    pub(crate) rig: Rig,
    pub(crate) seen: mpsc::UnboundedReceiver<Seen>,
    /// The session's id (the cookie value) and the handle the terminal runs under.
    pub(crate) session: (String, SessionKey),
    pub(crate) csrf: String,
    pub(crate) operator: String,
}

impl Site {
    /// A site whose host behaves as `kind` says, with `pinned` host keys pinned (none: the host
    /// is enrolled and heard, and has no pin).
    pub(crate) async fn new(name: &str, kind: Kind, pinned: Option<Vec<String>>) -> Site {
        Site::with(name, kind, (HOST, "operator"), pinned).await
    }

    /// [`Site::new`] for the host `host` and a session of `operator`.
    pub(crate) async fn with(
        name: &str,
        kind: Kind,
        who: (&str, &str),
        pinned: Option<Vec<String>>,
    ) -> Site {
        Site::build(name, kind, who, pinned, Tune::default()).await
    }

    /// [`Site::with`] over a signing changed as `signing` says.
    pub(crate) async fn build(
        name: &str,
        kind: Kind,
        who: (&str, &str),
        pinned: Option<Vec<String>>,
        signing: Tune,
    ) -> Site {
        // A real SSH server judges a certificate's validity by the system's clock, so the ledger
        // that signs it reads that clock too.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since_epoch| since_epoch.as_secs());
        let signing = Tune {
            ledger_now: Some(now),
            ..signing
        };
        let (host_id, operator) = who;
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a listener on a port of loopback");
        let port = listener.local_addr().expect("an address").port();
        let rig = Rig::tuned(name, signing, |limits| limits.port = port).await;
        let (told, seen) = mpsc::unbounded_channel();
        let ca = rig
            .coordinator
            .ssh
            .as_ref()
            .map(|ssh| ssh.fleet[ssh.signing].public_line())
            .and_then(|line| PublicKey::from_openssh(&line).ok())
            .map(|key| key.fingerprint(HashAlg::Sha256));
        tokio::spawn(host(listener, kind, (host_id.to_owned(), ca), now, told));
        if let Some(pinned) = pinned {
            let book = &rig.book.inner;
            use crate::ssh_book::Book as _;
            book.heard(host_id, "127.0.0.1", &pinned).expect("heard");
            book.pin("operator", host_id, &pinned).expect("pinned");
        }
        let (id, view) = rig
            .coordinator
            .login()
            .expect("a sign-in")
            .sessions
            .create(operator, false, None)
            .expect("a session");
        let session = (id.as_str().to_owned(), Sessions::key_of(id.as_str()));
        Site {
            rig,
            seen,
            session,
            csrf: view.csrf().to_owned(),
            operator: operator.to_owned(),
        }
    }

    /// What a terminal of this site for `host` is started with.
    pub(crate) fn start(&self, host: &str) -> Start {
        let login = self.rig.coordinator.login().expect("a sign-in");
        Start {
            coordinator: Arc::clone(&self.rig.coordinator),
            operator: self.operator.parse().expect("a token name"),
            session: self.session.1,
            view: login
                .sessions
                .lookup(&self.session.0)
                .expect("a live session"),
            host: host.to_owned(),
            patience: Duration::from_secs(10),
            slot: None,
        }
    }

    /// A browser's end of a pipe to a terminal started as `start` says whose writes to the
    /// browser fail.
    pub(crate) fn connect_broken(&self, start: Start) -> Browser {
        let (browser, server) = tokio::io::duplex(1 << 20);
        let serving = tokio::spawn(serve(start, BrokenWrites(server)));
        Browser::over(browser, serving)
    }

    /// Waits until a tunnel of this site has been opened, and gives its id.
    pub(crate) async fn opened(&mut self) -> u64 {
        match self
            .rig
            .event(|event| matches!(event, crate::ssh_tunnel::Event::Opened(_)))
            .await
        {
            crate::ssh_tunnel::Event::Opened(id) => id,
            other => panic!("{other:?}"),
        }
    }

    /// A browser connected to a terminal for `host`, which has not sent its first message.
    pub(crate) fn connect(&self, host: &str) -> Browser {
        self.connect_with(self.start(host))
    }

    /// [`Self::connect`] for a terminal started as `start` says.
    pub(crate) fn connect_with(&self, start: Start) -> Browser {
        let (browser, server) = tokio::io::duplex(1 << 20);
        let serving = tokio::spawn(serve(start, server));
        Browser::over(browser, serving)
    }

    /// The first message of a browser for the session of this site: its CSRF token and a size.
    pub(crate) fn hello(&self, columns: u32, rows: u32) -> String {
        serde_json::json!({"csrf": self.csrf, "cols": columns, "rows": rows}).to_string()
    }
}

/// The dialled side of a site: accepts connections and serves them as `kind` says, as the host
/// `host_id` whose certificates are signed by `ca`.
async fn host(
    listener: TcpListener,
    kind: Kind,
    who: (String, Option<Fingerprint>),
    now: u64,
    seen: mpsc::UnboundedSender<Seen>,
) {
    let (host_id, ca) = who;
    let mut held = Vec::new();
    let config = Arc::new(server::Config {
        keys: vec![host_key()],
        auth_rejection_time: Duration::ZERO,
        auth_rejection_time_initial: Some(Duration::ZERO),
        inactivity_timeout: None,
        window_size: match &kind {
            Kind::Ssh(Behavior {
                holds_input: Some(_),
                ..
            }) => 4096,
            _ => server::Config::default().window_size,
        },
        ..server::Config::default()
    });
    while let Ok((socket, _)) = listener.accept().await {
        match &kind {
            Kind::Ssh(behavior) => {
                let sshd = Sshd {
                    now,
                    host_id: host_id.clone(),
                    ca: behavior.trusts_ca.then_some(ca).flatten(),
                    behavior: behavior.clone(),
                    seen: seen.clone(),
                };
                let config = Arc::clone(&config);
                tokio::spawn(async move {
                    if let Ok(running) = server::run_stream(config, socket, sshd).await {
                        let _ = running.await;
                    }
                });
            }
            Kind::Stall => held.push(socket),
            Kind::HangUp => drop(socket),
        }
    }
}

/// One frame the coordinator sent.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Sent {
    Text(Value),
    Binary(Vec<u8>),
    /// A pong with its payload.
    Pong(Vec<u8>),
    /// A close frame with its status.
    Close(u16),
    /// The pipe ended.
    End,
}

/// The browser's end of a terminal's WebSocket.
pub(crate) struct Browser {
    read: Box<dyn AsyncRead + Unpin + Send>,
    write: Box<dyn AsyncWrite + Unpin + Send>,
    pub(crate) serving: tokio::task::JoinHandle<()>,
}

impl Browser {
    /// The browser's end of a connection whose upgrade was answered: `serving` is the task that
    /// serves its other end.
    pub(crate) fn over<S>(stream: S, serving: tokio::task::JoinHandle<()>) -> Browser
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (read, write) = tokio::io::split(stream);
        Browser {
            read: Box::new(read),
            write: Box::new(write),
            serving,
        }
    }

    /// Sends a text message.
    pub(crate) async fn text(&mut self, text: &str) {
        self.raw(&client_frame(1, text.as_bytes())).await;
    }

    /// Sends a binary message.
    pub(crate) async fn binary(&mut self, bytes: &[u8]) {
        self.raw(&client_frame(2, bytes)).await;
    }

    /// Sends the bytes of a frame as they are.
    pub(crate) async fn raw(&mut self, bytes: &[u8]) {
        self.write.write_all(bytes).await.expect("sent");
    }

    /// The next frame the coordinator sent, or [`Sent::End`] when the pipe ended.
    pub(crate) async fn next(&mut self) -> Sent {
        let mut head = [0_u8; 2];
        if self.read.read_exact(&mut head).await.is_err() {
            return Sent::End;
        }
        assert_eq!(head[1] & 0x80, 0, "a server frame is not masked");
        let length = match head[1] & 0x7f {
            126 => {
                let mut long = [0_u8; 2];
                self.read.read_exact(&mut long).await.expect("a length");
                usize::from(u16::from_be_bytes(long))
            }
            127 => {
                let mut long = [0_u8; 8];
                self.read.read_exact(&mut long).await.expect("a length");
                usize::try_from(u64::from_be_bytes(long)).expect("a length")
            }
            short => usize::from(short),
        };
        let mut payload = vec![0_u8; length];
        self.read.read_exact(&mut payload).await.expect("a payload");
        match head[0] {
            0x81 => Sent::Text(serde_json::from_slice(&payload).expect("JSON")),
            0x82 => Sent::Binary(payload),
            0x8a => Sent::Pong(payload),
            0x88 => Sent::Close(u16::from_be_bytes([payload[0], payload[1]])),
            other => panic!("a frame that is none of text, binary, pong and close: {other:#x}"),
        }
    }

    /// The next text message, which must be a status message of `step`; its `login_user`, if any.
    pub(crate) async fn status(&mut self, step: &str) -> Option<String> {
        let Sent::Text(message) = self.next().await else {
            panic!("a status of {step} was expected");
        };
        assert_eq!(message["type"], "status", "{message}");
        assert_eq!(message["step"], step, "{message}");
        message["login_user"].as_str().map(str::to_owned)
    }

    /// Reads binary messages until the bytes read hold `needle`; all the bytes read.
    pub(crate) async fn output_until(&mut self, needle: &[u8]) -> Vec<u8> {
        let mut all = Vec::new();
        while !all.windows(needle.len()).any(|window| window == needle) {
            match self.next().await {
                Sent::Binary(bytes) => {
                    assert!(
                        bytes.len() <= 16 * 1024,
                        "a message of {} bytes",
                        bytes.len()
                    );
                    all.extend(bytes);
                }
                other => panic!("output was expected, not {other:?}"),
            }
        }
        all
    }

    /// Reads binary messages until at least `count` bytes were read, each message of at most
    /// 16 KiB; how many messages it took.
    pub(crate) async fn output_count(&mut self, count: usize) -> usize {
        let (mut total, mut messages) = (0, 0);
        while total < count {
            match self.next().await {
                Sent::Binary(bytes) => {
                    assert!(
                        bytes.len() <= 16 * 1024,
                        "a message of {} bytes",
                        bytes.len()
                    );
                    total += bytes.len();
                    messages += 1;
                }
                other => panic!("output was expected, not {other:?}"),
            }
        }
        messages
    }

    /// Reads until the last message, which must be `closed` with `cause`, and then the close
    /// frame and the end of the pipe, and nothing else; the messages before it.
    pub(crate) async fn closed(&mut self, cause: &str) -> Vec<Sent> {
        let mut before = Vec::new();
        loop {
            match self.next().await {
                Sent::Text(message) if message["type"] == "closed" => {
                    assert_eq!(
                        message,
                        serde_json::json!({"type": "closed", "cause": cause})
                    );
                    break;
                }
                other => before.push(other),
            }
        }
        assert_eq!(self.next().await, Sent::Close(1000));
        assert_eq!(self.next().await, Sent::End);
        before
    }
}

/// A connection whose writes all fail and whose reads are the pipe's: a browser that went away
/// while still sending.
pub(crate) struct BrokenWrites(pub(crate) DuplexStream);

impl tokio::io::AsyncRead for BrokenWrites {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl tokio::io::AsyncWrite for BrokenWrites {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        _data: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::task::Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

/// The messages of a refusal at a step: the statuses the terminal sent before it, as
/// `["signing", ...]`.
pub(crate) fn steps(sent: &[Sent]) -> Vec<String> {
    sent.iter()
        .filter_map(|sent| match sent {
            Sent::Text(message) if message["type"] == "status" => {
                message["step"].as_str().map(str::to_owned)
            }
            _ => None,
        })
        .collect()
}
