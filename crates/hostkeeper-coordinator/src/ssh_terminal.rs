//! The UI's Remote SSH after the upgrade (docs/design/ssh-through-coordinator.md, section 13):
//! the terminal task of one WebSocket, from the first message to the last.
//!
//! Messages (JSON in text messages, bytes in binary ones):
//!
//! | direction | type | content |
//! |---|---|---|
//! | browser to coordinator | text, first | `{"csrf": "<token>", "cols": 1..500, "rows": 1..200}`, within the listener's patience |
//! | browser to coordinator | binary | keystrokes, at most 16 KiB each |
//! | browser to coordinator | text | `{"type": "resize", "cols": n, "rows": n}` |
//! | coordinator to browser | text | `{"type": "status", "step": "signing" \| "connecting" \| "opened", "login_user": "<name>"}` |
//! | coordinator to browser | binary | the host's output, at most 16 KiB each |
//! | coordinator to browser | text, last | `{"type": "closed", "cause": "<token>"}` |
//!
//! The steps, in the order of `hkctl ssh`: the first message must carry the session's CSRF token
//! and a size in range (else `bad_request`); the operators file must permit the host and the host
//! must be enrolled (the refusal's token: `not_permitted`, `unknown_host`, ...) and have a pinned
//! host key set (`no_pin`); then a key of the terminal's own is made from the coordinator's random
//! source, its certificate signed and audited as by `Ssh.SignUserCert` (reason `coordinator UI
//! terminal`), the tunnel opened as by `Ssh.Tunnel` under the credential of the login session
//! ([`Credential::Session`]), and the SSH client ([`crate::ssh_client`]) logs in as the login user
//! over the tunnel and asks for a pty and a shell. Whatever fails ends the socket with the cause
//! and nothing after it: a refusal's token, `host_key_mismatch` (the host's key is not in the
//! pinned set, so the certificate was never shown), `auth_refused`, `shell_refused`, or the
//! tunnel's own cause (`idle`, `max_session`, `revoked`, `not_permitted`, `token_revoked` when the
//! session ended, `coordinator_stopping`, `host_closed`, ...). A shell that ends is `exit`. The
//! browser closing the socket ends the SSH session and the tunnel, and has no message.
//!
//! The key lives in this task alone: it is made when the terminal opens, passed to the SSH client
//! and dropped when the task ends. It is in no log, file or message.

use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::SshRefusal as Refusal;
use hostkeeper_proto::hostkeeper::operator::v1::tunnel_frame::Frame;
use hyper::upgrade::OnUpgrade;
use hyper_util::rt::TokioIo;
use russh::ChannelWriteHalf;
use russh::client::Msg;
use russh::keys::Certificate;
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use tokio::task::AbortHandle;

use crate::listen::Hold;
use crate::operator::Caller;
use crate::service::Coordinator;
use crate::session::{SessionKey, SessionView};
use crate::ssh_client::{self, OpenError, Output, bridge, key_from_seed};
use crate::ssh_sign::refusal_token;
use crate::ssh_tunnel::{Credential, MAX_FRAME, QUEUE};
use crate::tokens::{Operator, TokenName};
use crate::websocket::{CLOSE_NORMAL, Message, Outbox, ReadError, Reader, Writer};

/// The reason the terminal's certificate is signed for.
pub(crate) const REASON: &str = "coordinator UI terminal";

/// The most columns a terminal has.
const MAX_COLUMNS: u32 = 500;

/// The most rows a terminal has.
const MAX_ROWS: u32 = 200;

/// The most messages of the browser that wait while a keystroke is being written to a host that
/// does not take it: past them the browser is not read until the write is done.
const MAX_WAITING: usize = 64;

/// The most bytes of a text message read: a first message or a resize is far smaller.
const MAX_TEXT: usize = 1024;

/// What a terminal is started with.
pub(crate) struct Start {
    /// The coordinator.
    pub(crate) coordinator: Arc<Coordinator>,
    /// The operator the session's user is.
    pub(crate) operator: TokenName,
    /// The session the terminal runs under: its credential.
    pub(crate) session: SessionKey,
    /// What the session said about itself when the socket was upgraded: its CSRF token.
    pub(crate) view: SessionView,
    /// The host the browser asked for, as it wrote it.
    pub(crate) host: String,
    /// How long the first message may take.
    pub(crate) patience: Duration,
    /// The listener's slot of the connection that was upgraded, held until the terminal ends
    /// (none for a terminal that no listener served).
    pub(crate) slot: Option<Hold>,
}

/// What the browser sent, as the terminal task reads it.
enum Input {
    Text(String),
    Binary(Vec<u8>),
    /// The browser closed, or the connection ended.
    Gone,
    /// The browser broke the protocol or sent more than a message may hold.
    Bad,
}

/// The first message.
#[derive(Deserialize)]
struct Hello {
    csrf: String,
    cols: u32,
    rows: u32,
}

/// A text message after the first.
#[derive(Deserialize)]
struct Resize {
    #[serde(rename = "type")]
    kind: String,
    cols: u32,
    rows: u32,
}

/// Whether `columns` and `rows` are a size a terminal may have.
fn size_ok(columns: u32, rows: u32) -> bool {
    (1..=MAX_COLUMNS).contains(&columns) && (1..=MAX_ROWS).contains(&rows)
}

/// The size a resize message asks for, if it is one and in range.
fn resize_of(text: &str) -> Option<(u32, u32)> {
    let resize: Resize = serde_json::from_str(text).ok()?;
    (resize.kind == "resize" && size_ok(resize.cols, resize.rows))
        .then_some((resize.cols, resize.rows))
}

/// The terminal of an upgrade: waits for the connection, then serves it.
pub(crate) async fn run(start: Start, upgrade: OnUpgrade) {
    match upgrade.await {
        Ok(upgraded) => serve(start, TokioIo::new(upgraded)).await,
        Err(why) => tracing::debug!(%why, "a terminal's connection was not handed over"),
    }
}

/// The terminal over `io`, a connection that speaks WebSocket from its first byte.
pub(crate) async fn serve<S>(mut start: Start, io: S)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    // The connection counts against its listener for as long as the terminal serves it.
    let _slot = start.slot.take();
    let (read, write) = tokio::io::split(io);
    let outbox = Outbox::new(Writer::new(write));
    let (mut input, reader) = listen(read, outbox.clone());
    if let Some(cause) = terminal(start, &outbox, &mut input).await {
        // A browser that is gone has nothing to be told.
        let _ = outbox
            .text(&json!({"type": "closed", "cause": cause}).to_string())
            .await;
    }
    let _ = outbox.close(CLOSE_NORMAL).await;
    // A browser that does not answer the close frame is not waited for: the connection ends here.
    reader.abort();
}

/// Reads the browser's messages in a task of its own, answering pings, and hands the rest on; the
/// handle ends the task. The task ends with the connection, or after the first thing that is no
/// message.
fn listen<R, W>(read: R, outbox: Outbox<W>) -> (mpsc::Receiver<Input>, AbortHandle)
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (messages, input) = mpsc::channel(8);
    let reader = tokio::spawn(async move {
        let mut reader = Reader::new(read, MAX_FRAME);
        loop {
            let sent = match reader.next().await {
                Ok(Message::Text(text)) if text.len() <= MAX_TEXT => Input::Text(text),
                Ok(Message::Text(_)) | Err(ReadError::Protocol(_) | ReadError::TooBig) => {
                    Input::Bad
                }
                Ok(Message::Binary(bytes)) => Input::Binary(bytes),
                Ok(Message::Ping(payload)) => {
                    // A browser that is gone has nothing to be answered.
                    let _ = outbox.pong(&payload).await;
                    continue;
                }
                Ok(Message::Close) | Err(ReadError::Ended) => Input::Gone,
            };
            let last = matches!(sent, Input::Gone | Input::Bad);
            // A terminal that has ended has no one to hand it to; the task is ended with it.
            let _ = messages.send(sent).await;
            if last {
                return;
            }
        }
    });
    (input, reader.abort_handle())
}

/// Pushes out the idle deadline of the session a terminal runs under; whether it lives.
fn touch(coordinator: &Coordinator, session: &SessionKey) -> bool {
    coordinator
        .login()
        .is_some_and(|login| login.sessions.touch(session))
}

/// A status message.
fn status(step: &str, login_user: Option<&str>) -> String {
    match login_user {
        Some(login_user) => {
            json!({"type": "status", "step": step, "login_user": login_user}).to_string()
        }
        None => json!({"type": "status", "step": step}).to_string(),
    }
}

/// The certificate of the terminal's key, the key, and the account the certificate logs in as.
struct Signed {
    key: russh::keys::PrivateKey,
    certificate: Certificate,
    login_user: String,
}

impl Coordinator {
    /// The terminal's key: 32 bytes of the coordinator's random source make an Ed25519 key.
    fn ssh_terminal_key(&self) -> Result<russh::keys::PrivateKey, &'static str> {
        let ssh = self.ssh.as_ref().ok_or("not_configured")?;
        let mut seed = [0_u8; 32];
        if (ssh.random)(&mut seed) {
            Ok(key_from_seed(&seed))
        } else {
            Err("error")
        }
    }
}

/// The host keys the operator pinned for `host`: `no_pin` when none are, a refusal's token when
/// `caller` may not read them.
async fn pinned_keys(
    coordinator: &Arc<Coordinator>,
    caller: &Caller,
    host: &str,
) -> Result<BTreeSet<String>, String> {
    let (coordinator, caller) = (Arc::clone(coordinator), caller.clone());
    let request = proto::HostKeysRequest {
        host_id: host.to_owned(),
    };
    // The book blocks: off the runtime's threads.
    let answer =
        tokio::task::spawn_blocking(move || coordinator.ssh_host_keys(&caller, &request)).await;
    // A task that did not finish is a store that did not answer.
    match answer.unwrap_or(Err(Refusal::StoreFailed)) {
        Ok(keys) if keys.state == "no_pin" => Err("no_pin".to_owned()),
        Ok(keys) => Ok(keys.fingerprints.into_iter().collect()),
        Err(refusal) => Err(refusal_token(refusal)),
    }
}

/// Makes the terminal's key and has its certificate signed for `host`: the certificate, with the
/// ledger's serial, and the login user.
async fn sign(
    coordinator: &Arc<Coordinator>,
    caller: &Caller,
    host: &str,
) -> Result<Signed, String> {
    let key = coordinator.ssh_terminal_key().map_err(str::to_owned)?;
    // A key that cannot be written is no key: the signing refuses it (`bad_key`).
    let public_key = key.public_key().to_openssh().unwrap_or_default();
    let (coordinator, caller) = (Arc::clone(coordinator), caller.clone());
    let request = proto::SignUserCertRequest {
        host_id: host.to_owned(),
        public_key,
        reason: REASON.to_owned(),
    };
    // The ledger blocks: off the runtime's threads.
    let signed =
        tokio::task::spawn_blocking(move || coordinator.ssh_sign_user_cert(&caller, &request))
            .await
            .unwrap_or(Err(Refusal::StoreFailed))
            .map_err(refusal_token)?;
    let certificate =
        Certificate::from_openssh(&signed.certificate).map_err(|_| "error".to_owned())?;
    Ok(Signed {
        key,
        certificate,
        login_user: signed.login_user,
    })
}

/// Waits until the browser is gone, dropping what it types meanwhile: nothing is listening for
/// it before the shell is open. The cause to tell, if the browser broke the protocol.
async fn browser_left(input: &mut mpsc::Receiver<Input>) -> Option<String> {
    loop {
        match input.recv().await {
            Some(Input::Text(_) | Input::Binary(_)) => {}
            Some(Input::Bad) => return Some("bad_request".to_owned()),
            Some(Input::Gone) | None => return None,
        }
    }
}

/// How a terminal's relay ended.
enum End {
    /// The shell ended.
    Exit,
    /// The connection to the host went with no word from the shell.
    Gone,
    /// The browser closed the socket.
    Browser,
    /// The browser sent what it may not.
    Bad,
}

/// The whole of one terminal: the cause to tell the browser, or `None` when the browser is gone
/// and there is nobody to tell.
async fn terminal<W>(
    start: Start,
    outbox: &Outbox<W>,
    input: &mut mpsc::Receiver<Input>,
) -> Option<String>
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    let Start {
        coordinator,
        operator,
        session,
        view,
        host,
        patience,
        ..
    } = start;
    let hello = match tokio::time::timeout(patience, input.recv()).await {
        Ok(Some(Input::Text(text))) => serde_json::from_str::<Hello>(&text).ok(),
        _ => None,
    };
    let Some(Hello {
        cols: columns,
        rows,
        ..
    }) = hello.filter(|hello| view.csrf_matches(&hello.csrf) && size_ok(hello.cols, hello.rows))
    else {
        return Some("bad_request".to_owned());
    };
    // A session that ended while the socket opened gets no terminal; one that lives is in use.
    if !touch(&coordinator, &session) {
        return Some("token_revoked".to_owned());
    }
    let caller = Caller::Operator(Operator::named(operator));
    let pinned = match pinned_keys(&coordinator, &caller, &host).await {
        Ok(pinned) => pinned,
        Err(cause) => return Some(cause),
    };
    // A browser that is gone is noticed when the shell is waited for.
    let _ = outbox.text(&status("signing", None)).await;
    let signed = match sign(&coordinator, &caller, &host).await {
        Ok(signed) => signed,
        Err(cause) => return Some(cause),
    };
    let _ = outbox.text(&status("connecting", None)).await;
    let Signed {
        key,
        certificate,
        login_user,
    } = signed;
    let (into_tunnel, frames) = mpsc::channel(QUEUE);
    let open = proto::TunnelFrame {
        frame: Some(Frame::Open(proto::Open {
            host_id: host.clone(),
            cert_serial: certificate.serial(),
        })),
    };
    // The channel is empty and its receiver is held below: the first frame is not refused.
    let _ = into_tunnel.send(Ok(open)).await;
    let frames = futures::stream::unfold(frames, |mut frames| async move {
        frames.recv().await.map(|frame| (frame, frames))
    });
    let outgoing = match coordinator
        .ssh_tunnel_open_as(&caller, Credential::Session(session), frames, None)
        .await
    {
        Ok(outgoing) => outgoing,
        Err(refusal) => return Some(refusal_token(refusal)),
    };
    let (stream, tunnel) = bridge(into_tunnel, outgoing);
    let login = (login_user.as_str(), Arc::new(key), certificate);
    let opened = tokio::select! {
        opened = ssh_client::open(stream, pinned, login, (columns, rows)) => opened,
        left = browser_left(input) => return left,
    };
    let mut shell = match opened {
        Ok(shell) => shell,
        Err(OpenError::HostKey) => return Some("host_key_mismatch".to_owned()),
        Err(OpenError::Auth) => return Some("auth_refused".to_owned()),
        Err(OpenError::Shell) => return Some("shell_refused".to_owned()),
        Err(OpenError::Connection) => {
            return Some(tunnel.cause().unwrap_or_else(|| "error".to_owned()));
        }
    };
    let _ = outbox.text(&status("opened", Some(&login_user))).await;
    let early = std::mem::take(&mut shell.early);
    let end = relay(&mut shell, &early, outbox, input, (&coordinator, &session)).await;
    shell.close().await;
    match end {
        End::Exit => Some("exit".to_owned()),
        End::Gone => Some(tunnel.cause().unwrap_or_else(|| "error".to_owned())),
        End::Browser => None,
        End::Bad => Some("bad_request".to_owned()),
    }
}

/// Carries the keystrokes to the shell and its output to the browser until either end ends. The
/// two directions are separate futures of one select, so a shell that waits for the window to be
/// opened by the host does not stop the output being read.
async fn relay<W>(
    shell: &mut ssh_client::Shell,
    early: &[Vec<u8>],
    outbox: &Outbox<W>,
    input: &mut mpsc::Receiver<Input>,
    session: (&Arc<Coordinator>, &SessionKey),
) -> End
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    let ssh_client::Shell {
        output,
        input: keys,
        ..
    } = shell;
    let (coordinator, session) = session;
    let typing = async {
        // What the browser sent while a keystroke was being written, in order.
        let mut waiting = VecDeque::new();
        loop {
            let next = match waiting.pop_front() {
                Some(next) => next,
                None => match input.recv().await {
                    Some(next) => next,
                    None => return End::Browser,
                },
            };
            match next {
                Input::Binary(bytes) => {
                    // What is typed is a use of the session: it does not idle out under a
                    // terminal that is in use.
                    touch(coordinator, session);
                    if let Some(end) = type_into(keys, bytes, input, &mut waiting).await {
                        return end;
                    }
                }
                Input::Text(text) => match resize_of(&text) {
                    Some((columns, rows)) => {
                        // A connection that is gone is told by the shell's output, which ends.
                        let _ = keys.window_change(columns, rows, 0, 0).await;
                    }
                    None => return End::Bad,
                },
                Input::Bad => return End::Bad,
                Input::Gone => return End::Browser,
            }
        }
    };
    let showing = async {
        for bytes in early {
            if send_output(outbox, bytes).await.is_err() {
                return End::Browser;
            }
        }
        loop {
            match ssh_client::next_output(output).await {
                Output::Bytes(bytes) => {
                    if send_output(outbox, &bytes).await.is_err() {
                        return End::Browser;
                    }
                }
                Output::Exit => return End::Exit,
                Output::Gone => return End::Gone,
            }
        }
    };
    tokio::select! {
        end = typing => end,
        end = showing => end,
    }
}

/// Writes `bytes` to the shell, which waits while the host's window is full: the browser is read
/// meanwhile (up to [`MAX_WAITING`] messages, which are kept in `waiting` for after the write), so
/// a browser that leaves or breaks the protocol ends the relay at once instead of after a host
/// that does not read its input. The end of the relay, if the browser ended it.
async fn type_into(
    keys: &ChannelWriteHalf<Msg>,
    bytes: Vec<u8>,
    input: &mut mpsc::Receiver<Input>,
    waiting: &mut VecDeque<Input>,
) -> Option<End> {
    // A connection that is gone is told by the shell's output, which ends.
    let mut written = std::pin::pin!(keys.data_bytes(bytes));
    loop {
        let listening = waiting.len() < MAX_WAITING;
        tokio::select! {
            _ = &mut written => return None,
            next = input.recv(), if listening => match next {
                Some(next @ (Input::Text(_) | Input::Binary(_))) => waiting.push_back(next),
                Some(Input::Bad) => return Some(End::Bad),
                Some(Input::Gone) | None => return Some(End::Browser),
            },
        }
    }
}

/// Sends the host's output as binary messages of at most [`MAX_FRAME`] bytes.
async fn send_output<W>(outbox: &Outbox<W>, bytes: &[u8]) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    for chunk in bytes.chunks(MAX_FRAME) {
        outbox.binary(chunk).await?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "ssh_terminal_tests.rs"]
mod tests;
