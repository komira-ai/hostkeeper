//! `Console/Probe`: a read-only look at a Linux server's serial console, in one call.
//!
//! The call is `Open` and an attach in the coordinator, so that it is audited, refused and
//! cleaned up as every console session is: the caller's checks, the host's device (a Mac is
//! `no_bmc_mac`), `console_busy`, `too_many_sessions` and the `console_open` line are `Open`'s
//! own, and a refusal of an operator the file lists is the same `console_denied` line. The
//! session is then attached by the operator who opened it, with the credential of the call (the
//! sweep holds it to it, and its close line names its API token), and its stream runs as
//! [`super::stream`] runs one, with a client in the coordinator in place of a WebSocket.
//!
//! The client types nothing: its side of the bridge never sends a byte, so no keystroke, not even
//! a newline, reaches the host. A newline is no lighter: at a firmware's setup screen or GRUB's
//! menu it selects an entry, at a shell it runs what is typed, and at the stock getty it moves the
//! port to 9600 baud (docs/operations/console.md). A host that would show a prompt only after a
//! key is `silent`.
//!
//! The read starts once the payload is active, and ends at the first of: `max_bytes` read; no byte
//! for `quiet_secs`, from its start or the last byte; `wait_secs` from its start; or the stream's
//! end (the host's console ended, another session took the payload, an operator's `Close`, the
//! drain at stop). The client then leaves, which ends the stream as a browser that left ends one:
//! the BMC tool stopped, the payload deactivated (unless another holder took it), the session out
//! of the table and one `console_close` line by the operator, `client_left`, with the bytes each
//! way. A device that is not reached ends the session the same way, by `coordinator`, and the
//! call is refused with the device's refusal. The adapter runs the call in a task of its own, so
//! that a caller who leaves does not cut the clean-up short.

use std::sync::Arc;
use std::time::Duration;

use hostkeeper_console::bridge::Pipe;
use hostkeeper_console::close::Close;
use hostkeeper_console::operators::Need;
use hostkeeper_console::session::SessionId;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::ConsoleProbeEnd as End;
use hostkeeper_proto::hostkeeper::operator::v1::ConsoleRefusal as Refusal;
use tokio::sync::{mpsc, oneshot};

use super::Timer;
use super::classify::Reading;
use super::route::attach;
use super::stream::{Attached, Carried, carry_inside};
use crate::operator::Caller;
use crate::service::Coordinator;
use crate::ssh_tunnel::Credential;

/// The longest read, in seconds.
pub(crate) const MAX_WAIT_SECS: u32 = 900;

/// The bytes a read ends at when the request names none.
pub(crate) const DEFAULT_MAX_BYTES: u32 = 64 * 1024;

/// The most bytes a request may name.
pub(crate) const MOST_BYTES: u32 = 1024 * 1024;

/// When a probe's read ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Window {
    pub(super) wait: Duration,
    pub(super) quiet: Duration,
    pub(super) max_bytes: u64,
}

impl Window {
    /// The window of `request`, `quiet` when it names no quiet time.
    ///
    /// # Errors
    /// `invalid_request`: `wait_secs` is 0 or above [`MAX_WAIT_SECS`], or `max_bytes` above
    /// [`MOST_BYTES`].
    pub(super) fn of(
        request: &proto::ProbeConsoleRequest,
        quiet: Duration,
    ) -> Result<Self, Refusal> {
        if !(1..=MAX_WAIT_SECS).contains(&request.wait_secs) || request.max_bytes > MOST_BYTES {
            return Err(Refusal::InvalidRequest);
        }
        let quiet = match request.quiet_secs {
            0 => quiet,
            secs => Duration::from_secs(u64::from(secs)),
        };
        let max_bytes = match request.max_bytes {
            0 => DEFAULT_MAX_BYTES,
            bytes => bytes,
        };
        Ok(Self {
            wait: Duration::from_secs(u64::from(request.wait_secs)),
            quiet,
            max_bytes: u64::from(max_bytes),
        })
    }
}

impl Coordinator {
    /// `Console/Probe`, as the module says.
    ///
    /// # Errors
    /// The caller's refusal, `no_console_access`, `invalid_request` (and a window out of its
    /// bounds), `reason_required`, `reason_too_long`, the host's, `no_bmc_mac` for a Mac,
    /// `console_busy`, `too_many_sessions`, `store_failed`, `audit_unavailable`; and once the
    /// session is open, the device's: `device_unreachable`, `bmc_sol_busy`, `bmc_busy`,
    /// `bmc_auth_failed`, `address_mismatch` or `bmc_failed`.
    pub(crate) async fn console_probe(
        self: Arc<Self>,
        caller: Caller,
        request: proto::ProbeConsoleRequest,
    ) -> Result<proto::ProbeConsoleResponse, Refusal> {
        self.console_operator(&caller, Need::Console)?;
        let window = Window::of(&request, self.console.quiet)?;
        let timer = Arc::clone(&self.console.timer);
        let opening = tokio::task::spawn_blocking(move || {
            let opened = self.open_session(&caller, &request.host_id, &request.reason, true)?;
            let operator = caller.operator()?;
            attach_opened(&self, &opened.id, operator)
        });
        let attached = opening.await.unwrap_or(Err(Refusal::StoreFailed))?;
        let (findings, carried, ended) = probe(attached, window, timer).await;
        if let Some(close) = carried.unreached {
            return Err(unreached(close));
        }
        Ok(proto::ProbeConsoleResponse {
            findings: findings.findings,
            from_boot: findings.from_boot,
            bytes_read: findings.bytes,
            ended: ended.into(),
            cause: carried.cause.to_owned(),
        })
    }
}

/// Attaches the session `id`, just opened by `operator`, with the operator's credential.
///
/// # Errors
/// `store_failed`: it could not be attached, as when `--console-attach-s` is 0 and its window
/// passed as it opened; the session then ends at once, with its `not_attached` close line, and
/// is not left to the sweep.
fn attach_opened(
    coordinator: &Arc<Coordinator>,
    id: &SessionId,
    operator: &crate::tokens::Operator,
) -> Result<Attached, Refusal> {
    let credential = Credential::of_bearer(operator);
    attach(coordinator, id, operator.name(), credential, None).map_err(|why| {
        tracing::error!(?why, "a probe's session could not be attached");
        let mut table = coordinator.console.lock_table();
        coordinator.end_unattached(&mut table.sessions, (coordinator.clock)());
        Refusal::StoreFailed
    })
}

/// The refusal of a device not reached for the close `close`.
pub(super) fn unreached(close: Close) -> Refusal {
    match close {
        Close::BmcSolBusy => Refusal::BmcSolBusy,
        Close::BmcBusy => Refusal::BmcBusy,
        Close::BmcAuthFailed => Refusal::BmcAuthFailed,
        Close::AddressMismatch => Refusal::AddressMismatch,
        Close::BmcFailed => Refusal::BmcFailed,
        Close::TokenRevoked => Refusal::NoConsoleAccess,
        Close::DeviceUnreachable
        | Close::Idle
        | Close::TotalLimit
        | Close::ClosedByOperator
        | Close::CoordinatorStopping
        | Close::MacScreenSharingOff
        | Close::MacUnreachableOrAsleep => Refusal::DeviceUnreachable,
    }
}

/// Runs the probe of `attached`: what it saw, how its stream ended, and why its read ended.
async fn probe(
    attached: Attached,
    window: Window,
    timer: Timer,
) -> (super::classify::Seen, Carried, End) {
    let (typed, typed_rx) = mpsc::channel::<Vec<u8>>(1);
    let (shown, shown_rx) = mpsc::channel(1);
    let (reached, reached_rx) = oneshot::channel();
    let client = Pipe {
        from: typed_rx,
        to: shown,
    };
    let carrying = carry_inside(attached, client, reached);
    let reading = read(shown_rx, typed, reached_rx, window, timer);
    let (carried, (reading, ended)) = tokio::join!(carrying, reading);
    (reading.finish(), carried, ended)
}

/// Reads what the stream shows the client, once the device is `reached`, until the window ends
/// or the stream does: the reading and why it ended. `typed` is held to the end and never sent
/// on: the probe types nothing, and its end is the end of the client's side.
async fn read(
    mut shown: mpsc::Receiver<Vec<u8>>,
    typed: mpsc::Sender<Vec<u8>>,
    reached: oneshot::Receiver<()>,
    window: Window,
    timer: Timer,
) -> (Reading, End) {
    let _typed = typed;
    let mut reading = Reading::new();
    if reached.await.is_err() {
        return (reading, End::StreamEnded);
    }
    let mut wait = timer(window.wait);
    loop {
        let quiet = timer(window.quiet);
        tokio::select! {
            biased;
            bytes = shown.recv() => {
                let Some(bytes) = bytes else {
                    return (reading, End::StreamEnded);
                };
                let room = window.max_bytes.saturating_sub(reading.bytes());
                let take = usize::try_from(room).unwrap_or(usize::MAX).min(bytes.len());
                reading.take(bytes.get(..take).unwrap_or_default());
                if reading.bytes() >= window.max_bytes {
                    return (reading, End::MaxBytes);
                }
            }
            () = quiet => return (reading, End::Quiet),
            () = &mut wait => return (reading, End::Wait),
        }
    }
}
