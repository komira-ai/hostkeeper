//! A console stream after its upgrade (`docs/design/console-broker.md`, decisions D4, D6 and D12,
//! sections 11.1, 11.2, 12 and 17.3): one task per attached session, from the device's contact to
//! the last audit line.
//!
//! Messages: every text or binary message of the client is bytes for the device (a text message
//! as its UTF-8, so a terminal may send either); every message of the coordinator is binary, the
//! device's bytes, at most 16 KiB each. A client's message is at most 64 KiB; one longer ends the
//! stream with 1009, and a broken frame with 1002. Pings are answered.
//!
//! The steps: the device is reached ([`super::device`]), then the bridge
//! ([`hostkeeper_console::bridge`]) runs until a side ends or the stream is stopped from outside
//! (an operator's `Console/Close`, the sweep's limits and revocations, the drain at stop). Then:
//! the device ends (the BMC tool stopped, the Mac's connection dropped); what is left of the
//! device's output is sent, and the close frame with its code and reason token; for a serial
//! console the payload is deactivated in a session of its own, unless another holder took it
//! (that holder keeps it) or it was never activated; and last the session leaves the table and
//! one `console_close` line is written, by the operator who closed it (the stream's own operator
//! when the browser left) or `coordinator`, with the cause, the attaching peer when known, the
//! seconds attached and the bytes each way.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures::TryStreamExt;
use hostkeeper_console::bridge::{Ended, Moved, Pipe, bridge};
use hostkeeper_console::close::Close;
use hostkeeper_console::session::{Kind, Session};
use hostkeeper_console::sol;
use hyper::upgrade::OnUpgrade;
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot, watch};

use super::Duplex;
use super::Stop;
use super::device::{End, Reached, deactivate, reach, stop_tool};
use crate::listen::Hold;
use crate::reconcile::COORDINATOR;
use crate::service::Coordinator;
use crate::websocket::{
    CLOSE_NORMAL, CLOSE_PROTOCOL, CLOSE_TOO_BIG, Message, Outbox, ReadError, Reader, Writer,
};

/// The most bytes of one message of the client.
pub(crate) const MAX_MESSAGE: usize = 64 * 1024;

/// How long what is left of the device's output and the close frame may take to be written to a
/// client that has stopped reading; the stream ends without them after that.
pub(crate) const CLOSE_GRACE: Duration = Duration::from_secs(5);

/// What a stream starts with: its session as attached, and how it is stopped and counted.
pub(crate) struct Attached {
    pub(crate) coordinator: Arc<Coordinator>,
    pub(crate) session: Session,
    pub(crate) peer: Option<SocketAddr>,
    pub(crate) stopped: watch::Receiver<Option<Stop>>,
    pub(crate) moved: Arc<Moved>,
    /// The listener's slot of the upgraded connection, held until the stream ends (none for a
    /// stream no listener served).
    pub(crate) slot: Option<Hold>,
}

/// How the client's side ended, as its reader saw it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Client {
    /// It closed, or the connection ended.
    Gone,
    /// It broke the protocol: the close code and the cause.
    Bad(u16, &'static str),
}

/// How a stream closes: the close frame's code, the cause it and the audit line give, who closed
/// it, and whether the payload is to be deactivated.
struct Closing {
    code: u16,
    cause: &'static str,
    by: String,
    deactivate: bool,
}

impl Closing {
    fn of(close: Close, by: &str) -> Self {
        Self {
            code: close.code(),
            cause: close.token(),
            by: by.to_owned(),
            deactivate: false,
        }
    }
}

/// The stream of an upgrade: waits for the connection, then serves it.
pub(crate) async fn run(attached: Attached, upgrade: OnUpgrade) {
    match upgrade.await {
        Ok(upgraded) => serve(attached, Box::new(TokioIo::new(upgraded))).await,
        Err(why) => {
            tracing::debug!(%why, "a console stream's connection was not handed over");
            let closing = Closing {
                code: CLOSE_NORMAL,
                cause: "not_upgraded",
                by: COORDINATOR.to_owned(),
                deactivate: false,
            };
            finish(&attached, &closing);
        }
    }
}

/// The stream over `io`, a connection that speaks WebSocket from its first byte. One type of
/// connection, the upgraded one in the coordinator and an in-memory one in the tests, so that
/// one copy of the stream is compiled and measured.
pub(crate) async fn serve(mut attached: Attached, io: Box<dyn Duplex>) {
    // The connection counts against its listener for as long as the stream serves it.
    let _slot = attached.slot.take();
    let (read, write) = tokio::io::split(io);
    let outbox = Outbox::new(Writer::new(write));
    let (typed, typed_rx) = mpsc::channel(1);
    let (told, verdict) = oneshot::channel();
    let reader = tokio::spawn(read_client(read, outbox.clone(), typed, told));
    let (shown, shown_rx) = mpsc::channel::<Vec<u8>>(1);
    let showing = outbox.clone();
    let mut writer = tokio::spawn(async move {
        let shown = futures::stream::unfold(shown_rx, |mut shown| async move {
            shown.recv().await.map(|bytes| (Ok(bytes), shown))
        });
        // A client that takes no more ends the writes.
        let _ = shown
            .try_for_each(|bytes| {
                let showing = showing.clone();
                async move { showing.binary(&bytes).await }
            })
            .await;
    });
    let client = Pipe {
        from: typed_rx,
        to: shown,
    };
    let closing = match reached(&mut attached).await {
        Err(closing) => {
            // Nothing is bridged: the writer has nothing more to send.
            drop(client);
            closing
        }
        Ok(reached) => relay(&mut attached, client, reached, verdict).await,
    };
    // What is left of the device's output goes before the close frame, within the grace; a
    // client that reads nothing loses it, and its connection, at the grace.
    if tokio::time::timeout(CLOSE_GRACE, &mut writer)
        .await
        .is_err()
    {
        writer.abort();
        tracing::debug!("a console client did not take the end of its output in time");
    }
    let close = outbox.close_with(closing.code, closing.cause);
    // A client that is gone, or reads nothing, is not waited for.
    let _ = tokio::time::timeout(CLOSE_GRACE, close).await;
    reader.abort();
    if closing.deactivate {
        deactivate(&attached.coordinator, &attached.session).await;
    }
    finish(&attached, &closing);
}

/// The device of the stream, reached; or how the stream closes without it. A stop cancels the dial
/// of a Mac; a serial console's activation is waited for, since one cut part-way would leave a
/// payload active that nobody deactivates, and the stop is then seen by the bridge at once.
async fn reached(attached: &mut Attached) -> Result<Reached, Closing> {
    let reaching = reach(&attached.coordinator, &attached.session);
    if attached.session.kind == Kind::Serial {
        return reaching
            .await
            .map_err(|close| Closing::of(close, COORDINATOR));
    }
    tokio::select! {
        reached = reaching => reached.map_err(|close| Closing::of(close, COORDINATOR)),
        Stop { close, by } = stopped(&mut attached.stopped) => Err(Closing::of(close, &by)),
    }
}

/// Bridges the client and the device until either ends or the stream is stopped, then ends the
/// device: how the stream closes. `verdict` is told by the client's reader before its messages
/// end, how they ended.
async fn relay(
    attached: &mut Attached,
    client: Pipe,
    reached: Reached,
    mut verdict: oneshot::Receiver<Client>,
) -> Closing {
    let Reached { pipe, end } = reached;
    let kind = attached.session.kind;
    let ended = tokio::select! {
        ended = bridge(kind, client, pipe, &attached.moved) => Ok(ended),
        stop = stopped(&mut attached.stopped) => Err(stop),
    };
    let text = match end {
        End::Serial(stop) => Some(stop_tool(stop).await),
        End::Screen(tasks) => {
            tasks.iter().for_each(tokio::task::AbortHandle::abort);
            None
        }
    };
    // A payload the BMC gave to another session is that session's: it is never deactivated.
    let ours = text.as_ref().is_some_and(|text| !sol::taken(text));
    match ended {
        Err(Stop { close, by }) => Closing {
            deactivate: ours,
            ..Closing::of(close, &by)
        },
        Ok(Ended::Device) => match text {
            Some(text) => {
                // Only a payload that was activated and then lost is left active.
                let close = Close::from(sol::ended(&text));
                Closing {
                    deactivate: close == Close::DeviceUnreachable,
                    ..Closing::of(close, COORDINATOR)
                }
            }
            None => Closing {
                code: CLOSE_NORMAL,
                cause: "device_closed",
                by: COORDINATOR.to_owned(),
                deactivate: false,
            },
        },
        Ok(Ended::Browser) => {
            // The reader told its verdict before its messages ended; with none, the client
            // stopped taking output and is gone.
            match verdict.try_recv().unwrap_or(Client::Gone) {
                Client::Gone => Closing {
                    code: CLOSE_NORMAL,
                    cause: "client_left",
                    by: attached.session.opened_by.clone(),
                    deactivate: ours,
                },
                Client::Bad(code, cause) => Closing {
                    code,
                    cause,
                    by: COORDINATOR.to_owned(),
                    deactivate: ours,
                },
            }
        }
    }
}

/// The first stop sent to the stream; never, when nothing can send one any more.
async fn stopped(stopped: &mut watch::Receiver<Option<Stop>>) -> Stop {
    let first = stopped
        .wait_for(Option::is_some)
        .await
        .ok()
        .and_then(|stop| stop.clone());
    match first {
        Some(stop) => stop,
        None => std::future::pending().await,
    }
}

/// Reads the client's messages, answering pings, and hands their bytes to `typed`, one message at
/// a time: a message is read only once the one before was taken. How the client ended is told to
/// `told` before `typed` is dropped, which ends its messages.
async fn read_client<R, W>(
    read: R,
    outbox: Outbox<W>,
    typed: mpsc::Sender<Vec<u8>>,
    told: oneshot::Sender<Client>,
) where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let client = read_messages(read, outbox, &typed).await;
    // A stream that ended first has no one to tell.
    let _ = told.send(client);
    drop(typed);
}

/// [`read_client`]'s reading: how the client ended.
async fn read_messages<R, W>(read: R, outbox: Outbox<W>, typed: &mpsc::Sender<Vec<u8>>) -> Client
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let mut reader = Reader::new(read, MAX_MESSAGE);
    loop {
        let bytes = match reader.next().await {
            Ok(Message::Text(text)) => text.into_bytes(),
            Ok(Message::Binary(bytes)) => bytes,
            Ok(Message::Ping(payload)) => {
                // A client that is gone has nothing to be answered.
                let _ = outbox.pong(&payload).await;
                continue;
            }
            Ok(Message::Close) | Err(ReadError::Ended) => return Client::Gone,
            Err(ReadError::Protocol(_)) => return Client::Bad(CLOSE_PROTOCOL, "protocol_error"),
            Err(ReadError::TooBig) => return Client::Bad(CLOSE_TOO_BIG, "message_too_big"),
        };
        // A stream that has ended takes no more: what is read is dropped until the reader is
        // stopped with it.
        let _ = typed.send(bytes).await;
    }
}

/// Removes the stream's session from the table (only its own: a later session of the host
/// stays), and writes its `console_close` line.
fn finish(attached: &Attached, closing: &Closing) {
    let coordinator = &attached.coordinator;
    let session = &attached.session;
    let now = (coordinator.clock)();
    {
        let mut table = coordinator.console.lock_table();
        table.sessions.remove(&session.id);
        table.live.retain(|_, live| live.id != session.id);
    }
    let attached_at = session
        .attachment
        .as_ref()
        .map_or(now, |attachment| attachment.at);
    let secs = now.saturating_sub(attached_at).as_secs().to_string();
    let to_device = attached.moved.to_device().to_string();
    let to_browser = attached.moved.to_browser().to_string();
    let peer = attached.peer.map(|peer| peer.to_string());
    let mut more = vec![
        ("secs", secs.as_str()),
        ("to_device", to_device.as_str()),
        ("to_client", to_browser.as_str()),
    ];
    if let Some(peer) = &peer {
        more.insert(0, ("peer", peer.as_str()));
    }
    let ended = coordinator.ops.unix_now();
    coordinator.closed(&closing.by, session, closing.cause, ended, &more);
    // Told last: whoever waits for the end (the drain) finds the line written.
    coordinator.console.ended.notify_waiters();
}
