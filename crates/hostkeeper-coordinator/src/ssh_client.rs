//! The SSH client of the UI's terminal ([`crate::ssh_terminal`]), over the in-process end of a
//! tunnel (docs/design/ssh-through-coordinator.md, section 13).
//!
//! Three parts. [`bridge`] turns the frames of a tunnel ([`crate::ssh_tunnel`]) into a byte
//! stream: whatever the SSH client writes goes out as `data` frames of at most
//! [`crate::ssh_tunnel::MAX_FRAME`] bytes, whatever the host sends comes back in as bytes, and
//! the cause of the tunnel's `close` frame, when one arrives, is kept for the terminal to tell.
//! [`open`] speaks SSH over that stream with the `russh` crate: the host's key must be one the
//! operator pinned (its `SHA256:` fingerprint is in the set, the form `ssh-keygen -l` prints and
//! [`crate::ssh_cert::fingerprint`] makes), checked before any credential is sent; then the
//! operator's certificate logs in as the login user, and a pty and a shell are requested.
//! [`key_from_seed`] makes the terminal's key: an Ed25519 key from bytes the caller drew, held
//! in memory and in no file, no log and no message.
//!
//! The client does not trust what the host sends past the host key: a refusal of the login or of
//! the shell is an error with its own name ([`OpenError`]), and the terminal tells it as such.

use std::collections::BTreeSet;
use std::sync::Arc;

use futures::StreamExt;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::tunnel_frame::Frame;
use russh::client::{self, Handle, Msg};
use russh::keys::ssh_key::private::Ed25519Keypair;
use russh::keys::{Certificate, PrivateKey, PublicKeyOrCertificate};
use russh::{ChannelMsg, ChannelReadHalf, ChannelWriteHalf, Disconnect};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tonic::Status;

use crate::ssh_cert::fingerprint;
use crate::ssh_tunnel::{MAX_FRAME, Outgoing};

/// The terminal type the pty is requested with.
const TERM: &str = "xterm-256color";

/// The bytes the stream between the SSH client and the tunnel holds each way.
const BUFFER: usize = 64 * 1024;

/// The identification the client sends: the name of the product and no version, so a host's log
/// does not say which build it was.
const CLIENT_ID: &str = "SSH-2.0-hostkeeper-coordinator";

/// What a frame sent into the tunnel looks like to its stream.
pub(crate) type Sender = mpsc::Sender<Result<proto::TunnelFrame, Status>>;

/// The tunnel as the terminal holds it: what it said when it ended, and the task that carries
/// what the client writes into it.
pub(crate) struct Tunnel {
    cause: watch::Receiver<Option<String>>,
    up: JoinHandle<()>,
}

impl Tunnel {
    /// The cause of the tunnel's `close` frame, once it arrived.
    pub(crate) fn cause(&self) -> Option<String> {
        self.cause.borrow().clone()
    }
}

impl Drop for Tunnel {
    /// Letting go of the tunnel ends it: what the client writes stops, so the tunnel sees its
    /// client's stream end and closes (`eof`). The task that carries the answer ends with the
    /// answer, which is its `close`.
    fn drop(&mut self) {
        self.up.abort();
    }
}

/// A `data` frame of `bytes`, which are at most [`MAX_FRAME`].
fn data(bytes: &[u8]) -> proto::TunnelFrame {
    proto::TunnelFrame {
        frame: Some(Frame::Data(bytes.to_vec())),
    }
}

/// The stream of the SSH client over a tunnel: `into_tunnel` takes the frames the client sends
/// (`open` was sent before), `from_tunnel` gives the frames of the tunnel's answer. Two tasks
/// carry the bytes; when the client drops its end the tunnel is sent `close{eof}`, and when the
/// tunnel ends the client reads end of file. The stream is the client's end; the [`Tunnel`] is
/// the terminal's.
pub(crate) fn bridge(into_tunnel: Sender, from_tunnel: Outgoing) -> (DuplexStream, Tunnel) {
    let (ours, theirs) = tokio::io::duplex(BUFFER);
    let (from_client, to_client) = tokio::io::split(theirs);
    let (told, cause) = watch::channel(None);
    let up = tokio::spawn(carry_up(from_client, into_tunnel));
    tokio::spawn(carry_down(from_tunnel, to_client, told));
    (ours, Tunnel { cause, up })
}

/// What the client writes, in frames; its end of the stream closing is `close{eof}`.
async fn carry_up(mut from: impl AsyncReadExt + Unpin, into_tunnel: Sender) {
    let mut buffer = vec![0_u8; MAX_FRAME];
    while let Ok(read @ 1..) = from.read(&mut buffer).await {
        if into_tunnel.send(Ok(data(&buffer[..read]))).await.is_err() {
            return;
        }
    }
    let close = proto::Close {
        cause: "eof".to_owned(),
    };
    // A tunnel that is gone has nothing to be told.
    let _ = into_tunnel
        .send(Ok(proto::TunnelFrame {
            frame: Some(Frame::Close(close)),
        }))
        .await;
}

/// What the tunnel answers: `data` is written to the client, `close` is kept as the cause, and the
/// client's stream is shut when the answer ends, whatever way.
async fn carry_down(
    mut from_tunnel: Outgoing,
    mut to_client: impl AsyncWriteExt + Unpin,
    told: watch::Sender<Option<String>>,
) {
    while let Some(Ok(frame)) = from_tunnel.next().await {
        match frame.frame {
            Some(Frame::Data(bytes)) => {
                let written = to_client.write_all(&bytes).await;
                if written.is_err() {
                    break;
                }
            }
            Some(Frame::Close(close)) => {
                told.send_replace(Some(close.cause));
                break;
            }
            _ => {}
        }
    }
    // A client that is gone has nothing to be shut.
    let _ = to_client.shutdown().await;
}

/// The terminal's key: the Ed25519 key `seed` makes.
pub(crate) fn key_from_seed(seed: &[u8; 32]) -> PrivateKey {
    PrivateKey::from(Ed25519Keypair::from_seed(seed))
}

/// Why the terminal could not be opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum OpenError {
    /// The host's key is not one the operator pinned: nothing was sent past the key exchange.
    #[error("the host's key is not in the pinned set")]
    HostKey,
    /// The host refused the certificate.
    #[error("the host refused the certificate")]
    Auth,
    /// The host refused the channel, the pty or the shell.
    #[error("the host refused the shell")]
    Shell,
    /// The connection broke before the host answered.
    #[error("the connection to the host broke")]
    Connection,
}

impl From<russh::Error> for OpenError {
    /// A host that refused to open the channel refused the shell; any other failure of the
    /// connection is the connection's.
    fn from(why: russh::Error) -> Self {
        match why {
            russh::Error::ChannelOpenFailure(_) => OpenError::Shell,
            _ => OpenError::Connection,
        }
    }
}

/// The handler of the client: it trusts the pinned keys and nothing else.
struct Pinned {
    fingerprints: BTreeSet<String>,
}

/// What the handler and the connection can fail with.
#[derive(Debug, thiserror::Error)]
enum ClientError {
    #[error("the host's key is not in the pinned set")]
    Unpinned,
    #[error("ssh: {0}")]
    Ssh(#[from] russh::Error),
}

impl client::Handler for Pinned {
    type Error = ClientError;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        // A host certificate is not asked about: the key it holds is the host key, which the
        // operator pinned or did not.
        let pinned = server_public_key
            .public_key()
            .to_bytes()
            .is_ok_and(|blob| self.fingerprints.contains(&fingerprint(&blob)));
        if pinned {
            Ok(true)
        } else {
            Err(ClientError::Unpinned)
        }
    }
}

/// An open shell on the host: the halves of its channel and the connection that carries them.
pub(crate) struct Shell {
    /// The host's output.
    pub(crate) output: ChannelReadHalf,
    /// The operator's input and the window size.
    pub(crate) input: ChannelWriteHalf<Msg>,
    /// What the host sent before it answered the shell request.
    pub(crate) early: Vec<Vec<u8>>,
    handle: Handle<Pinned>,
}

impl Shell {
    /// Tells the host the session is over and lets go of it.
    pub(crate) async fn close(self) {
        // A connection that is gone has nothing to be told.
        let _ = self
            .handle
            .disconnect(Disconnect::ByApplication, "", "")
            .await;
    }
}

/// The most bytes of output the host may send before it answers a request that wants a reply: a
/// shell's banner is a few lines, and a host that sends more without answering is not held in
/// memory for it.
pub(crate) const MAX_EARLY: usize = 64 * 1024;

/// What the host sends to a request that wants a reply: the output it sent before the reply, and
/// whether the reply was a success. More than [`MAX_EARLY`] bytes before the reply is a broken
/// connection.
async fn reply(channel: &mut russh::Channel<Msg>) -> Result<Vec<Vec<u8>>, OpenError> {
    let mut early: Vec<Vec<u8>> = Vec::new();
    let mut held = 0_usize;
    loop {
        match channel.wait().await {
            Some(ChannelMsg::Success) => return Ok(early),
            Some(ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. }) => {
                held = held.saturating_add(data.len());
                if held > MAX_EARLY {
                    return Err(OpenError::Connection);
                }
                early.push(data.to_vec());
            }
            Some(ChannelMsg::Failure) => return Err(OpenError::Shell),
            None | Some(ChannelMsg::Eof | ChannelMsg::Close) => return Err(OpenError::Connection),
            Some(_) => {}
        }
    }
}

/// Speaks SSH over `stream`: the host's key must be among `pinned` (fingerprints), then `user` logs
/// in with `certificate` (made for `key`), and a pty of `size` (columns, rows) and a shell are
/// requested.
///
/// # Errors
/// [`OpenError`]: the step that failed.
pub(crate) async fn open(
    stream: DuplexStream,
    pinned: BTreeSet<String>,
    login: (&str, Arc<PrivateKey>, Certificate),
    size: (u32, u32),
) -> Result<Shell, OpenError> {
    let (user, key, certificate) = login;
    let config = client::Config {
        client_id: russh::SshId::Standard(CLIENT_ID.into()),
        ..client::Config::default()
    };
    let mut handle = client::connect_stream(
        Arc::new(config),
        stream,
        Pinned {
            fingerprints: pinned,
        },
    )
    .await
    .map_err(|why| match why {
        ClientError::Unpinned => OpenError::HostKey,
        ClientError::Ssh(_) => OpenError::Connection,
    })?;
    let login = handle
        .authenticate_openssh_cert(user, key, certificate)
        .await?;
    if !login.success() {
        return Err(OpenError::Auth);
    }
    let mut channel = handle.channel_open_session().await?;
    let (columns, rows) = size;
    channel
        .request_pty(true, TERM, columns, rows, 0, 0, &[])
        .await?;
    reply(&mut channel).await?;
    channel.request_shell(true).await?;
    let early = reply(&mut channel).await?;
    let (output, input) = channel.split();
    Ok(Shell {
        output,
        input,
        early,
        handle,
    })
}

/// What the shell's channel says next.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Output {
    /// Bytes for the screen: standard output or standard error.
    Bytes(Vec<u8>),
    /// The shell is done: the host closed the channel or sent end of file.
    Exit,
    /// The channel is gone with no word: the connection broke.
    Gone,
}

/// The next thing the shell's channel says that the terminal shows or acts on; the rest (exit
/// status, window adjustments) is passed over.
pub(crate) async fn next_output(output: &mut ChannelReadHalf) -> Output {
    loop {
        match output.wait().await {
            Some(ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. }) => {
                return Output::Bytes(data.to_vec());
            }
            Some(ChannelMsg::Eof | ChannelMsg::Close) => return Output::Exit,
            None => return Output::Gone,
            Some(_) => {}
        }
    }
}

#[cfg(test)]
#[path = "ssh_client_tests.rs"]
mod tests;
