//! The device end of a console stream (`docs/design/console-broker.md`, sections 6.1, 6.3, 9.1,
//! 11.1, 11.2 and 12): how the coordinator reaches a host's console after the upgrade, and how
//! the device's bytes become one side of the bridge ([`hostkeeper_console::bridge::Pipe`]).
//!
//! - A Linux server: Serial over LAN through its BMC ([`Bmc::activate_sol`]), the BMC's commands
//!   serialized with the power calls' while the session is being set up (not while it is used:
//!   power must work beside an open console). The tool's standard output and input are carried
//!   by two threads of their own, since they block.
//! - A Mac: a TCP connection to its Screen Sharing port, by the [`Dial`] (`tcp_dial` in the
//!   coordinator, a double in the tests): a refused connection is `mac_screen_sharing_off`, any
//!   other failure, the `--console-connect-timeout-s` passing among them,
//!   `mac_unreachable_or_asleep`. The bytes cross unread: RFB authenticates inside them.
//!
//! Each direction reads only once what it read last was handed on, through channels of one
//! message, so neither end is read ahead of the other by more than a message.

use std::io::{self, Read, Write};
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use futures::{FutureExt, TryStreamExt};
use hostkeeper_console::bmc::{Bmc, BmcError, Step};
use hostkeeper_console::bridge::Pipe;
use hostkeeper_console::close::Close;
use hostkeeper_console::ipmi::{Activated, Ending, ToolText};
use hostkeeper_console::operators::Need;
use hostkeeper_console::session::{Kind, Session};
use hostkeeper_console::targets::Address;
use hostkeeper_wire::HostId;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

use super::{Device, serialized};
use crate::service::Coordinator;

/// The most bytes of one read of a device, and so of one message to the browser.
pub(crate) const READ_BYTES: usize = 16 * 1024;

/// A connection to a Mac.
pub trait Duplex: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> Duplex for T {}

/// How the coordinator connects to a Mac's Screen Sharing at an address.
pub type Dial =
    Arc<dyn Fn(Address) -> BoxFuture<'static, io::Result<Box<dyn Duplex>>> + Send + Sync>;

/// The dial of a TCP connection to `port` at the address, which fails as
/// [`io::ErrorKind::TimedOut`] when it is not made within `timeout`.
#[must_use]
pub fn tcp_dial(port: u16, timeout: Duration) -> Dial {
    Arc::new(move |address: Address| {
        async move {
            let host = address.to_string();
            let connecting = tokio::net::TcpStream::connect((host.as_str(), port));
            let timed = tokio::time::timeout(timeout, connecting).await;
            let connected = timed.unwrap_or_else(|_| Err(io::ErrorKind::TimedOut.into()));
            connected.map(|stream| Box::new(stream) as Box<dyn Duplex>)
        }
        .boxed()
    })
}

/// The close of a failed dial of a Mac.
pub(crate) fn dial_failure(why: &io::Error) -> Close {
    match why.kind() {
        io::ErrorKind::ConnectionRefused => Close::MacScreenSharingOff,
        _ => Close::MacUnreachableOrAsleep,
    }
}

/// A device reached: its side of the bridge, and how it ends.
pub(crate) struct Reached {
    pub(crate) pipe: Pipe,
    pub(crate) end: End,
}

/// How a reached device ends.
pub(crate) enum End {
    /// A serial console: the tool is stopped and says what it wrote to its standard error.
    Serial(Ending),
    /// A screen: the tasks that read and write the connection, ended with the stream, which
    /// closes the connection.
    Screen([tokio::task::AbortHandle; 2]),
}

/// Reaches the device of `session` for the operator who opened it.
///
/// # Errors
/// The [`Close`] the stream ends with: the operator's entry is gone (`token_revoked`), the host's
/// device can no longer be found (`device_unreachable`, logged), or the device's own failure.
pub(crate) async fn reach(
    coordinator: &Arc<Coordinator>,
    session: &Session,
) -> Result<Reached, Close> {
    let grant = coordinator
        .console
        .files
        .grant(&session.opened_by, Need::Console)
        .map_err(|_| Close::TokenRevoked)?;
    let serial = session.kind == Kind::Serial;
    let found = session
        .host_id
        .parse::<HostId>()
        .map_err(|_| hostkeeper_proto::hostkeeper::operator::v1::ConsoleRefusal::InvalidRequest)
        .and_then(|host| coordinator.console_device(&host, &grant.targets, serial));
    let Device { mac, address, .. } = found.map_err(|refusal| {
        tracing::warn!(host_id = %session.host_id, refusal = refusal.as_str_name(), "a console's device could not be found at its attach");
        Close::DeviceUnreachable
    })?;
    if !serial {
        let stream = (coordinator.console.dial)(address)
            .await
            .map_err(|why| dial_failure(&why))?;
        return Ok(carry_socket(stream));
    }
    let shared = Arc::clone(coordinator);
    let host_id = session.host_id.clone();
    let activated = tokio::task::spawn_blocking(move || {
        let console = &shared.console;
        let gate = console.bmc(mac);
        let _serial = serialized(console, &gate);
        Bmc {
            tool: console.tool.as_ref(),
            address: &address,
            credential: &grant.credential,
            settings: &console.settings,
            mac,
        }
        .activate_sol()
    })
    .await;
    // A task that did not finish ran a tool that did not answer as it must.
    let activated = activated.unwrap_or(Err(BmcError::Failed(Step::Start)));
    activated.map(carry_tool).map_err(|why| {
        tracing::info!(%host_id, cause = %why, "a serial console was not activated");
        Close::from(why)
    })
}

/// Deactivates the Serial over LAN payload of the host of `session` after its stream ended on the
/// coordinator's side, in a session of its own; best effort, a failure is logged.
pub(crate) async fn deactivate(coordinator: &Arc<Coordinator>, session: &Session) {
    let shared = Arc::clone(coordinator);
    let host_id = session.host_id.clone();
    let done = tokio::task::spawn_blocking(move || {
        let console = &shared.console;
        // The credential and the address as read now: the operator's entry is not asked for, so
        // a revoked operator's payload is deactivated too.
        let grant = console.files.devices().ok_or("no console files")?;
        let host = host_id.parse::<HostId>().map_err(|_| "a host id")?;
        let Device { mac, address, .. } = shared
            .console_device(&host, &grant.targets, true)
            .map_err(|refusal| refusal.as_str_name())?;
        let gate = console.bmc(mac);
        let _serial = serialized(console, &gate);
        Bmc {
            tool: console.tool.as_ref(),
            address: &address,
            credential: &grant.credential,
            settings: &console.settings,
            mac,
        }
        .deactivate_sol()
        .map_err(|_| "the BMC")
    })
    .await;
    if let Err(step) = done.unwrap_or(Err("an unfinished task")) {
        tracing::warn!(host_id = %session.host_id, step, "a serial console's payload was not deactivated");
    }
}

/// Stops the tool of a serial console, off the runtime's threads, and answers its standard error.
pub(crate) async fn stop_tool(end: Ending) -> ToolText {
    let ended = tokio::task::spawn_blocking(end).await;
    ended.unwrap_or_else(|_| ToolText::new(String::new()))
}

/// The side of the bridge of an activated tool: a thread reads its standard output, one writes
/// its standard input.
fn carry_tool(activated: Activated) -> Reached {
    let Activated { input, output, end } = activated;
    let (said, from) = mpsc::channel(1);
    let (to, typed) = mpsc::channel(1);
    std::thread::spawn(move || read_tool(output, &said));
    std::thread::spawn(move || write_tool(input, typed));
    Reached {
        pipe: Pipe { from, to },
        end: End::Serial(end),
    }
}

/// Hands on what `output` reads, until it ends or the bridge takes no more.
fn read_tool(mut output: Box<dyn Read + Send>, said: &mpsc::Sender<Vec<u8>>) {
    let mut buffer = vec![0u8; READ_BYTES];
    let mut reads = std::iter::from_fn(|| match output.read(&mut buffer) {
        Ok(read @ 1..) => Some(buffer[..read].to_vec()),
        Ok(0) | Err(_) => None,
    });
    // The bridge that takes no more ends the reads.
    let _ = reads.try_for_each(|bytes| said.blocking_send(bytes));
}

/// Writes to `input` what the bridge hands on, until it hands no more or a write fails.
fn write_tool(mut input: Box<dyn Write + Send>, mut typed: mpsc::Receiver<Vec<u8>>) {
    let mut messages = std::iter::from_fn(|| typed.blocking_recv());
    // A write that fails ends the writes.
    let _ = messages.try_for_each(|bytes| input.write_all(&bytes).and_then(|()| input.flush()));
}

/// The side of the bridge of a connection: one task reads it, one writes it, and the write side
/// is shut down when the bridge hands on no more.
fn carry_socket(stream: Box<dyn Duplex>) -> Reached {
    let (read, write) = tokio::io::split(stream);
    let (said, from) = mpsc::channel(1);
    let (to, typed) = mpsc::channel::<Vec<u8>>(1);
    let reading = tokio::spawn(async move {
        let reads = futures::stream::unfold(read, |mut read| async move {
            let mut buffer = vec![0u8; READ_BYTES];
            match read.read(&mut buffer).await {
                Ok(count @ 1..) => Some((Ok(buffer[..count].to_vec()), read)),
                _ => None,
            }
        });
        // The end of the connection, or a bridge that takes no more, ends the reads.
        let _ = reads
            .try_for_each(|bytes| {
                let said = said.clone();
                async move { said.send(bytes).await }
            })
            .await;
    });
    let writing = tokio::spawn(async move {
        let typed = futures::stream::unfold(typed, |mut typed| async move {
            typed.recv().await.map(|bytes| (Ok(bytes), typed))
        });
        // The end of the bridge's channel, or a write that fails, ends the writes.
        let _ = typed
            .try_fold(write, |mut write, bytes| async move {
                write.write_all(&bytes).await.map(|()| write)
            })
            .await;
    });
    Reached {
        pipe: Pipe { from, to },
        end: End::Screen([reading.abort_handle(), writing.abort_handle()]),
    }
}
