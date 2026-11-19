//! The coordinator's `Console` calls of `hostkeeper.operator.v1` and the console's WebSocket
//! (`docs/design/console-broker.md`, rows C2 and C4 of section 15): a Linux server's power and boot
//! device through its BMC, the console sessions, and the streams attached to them (a Linux
//! server's serial port over Serial over LAN, a Mac's Screen Sharing).
//!
//! - [`Coordinator::console_power`] and [`Coordinator::console_set_boot_device`]
//!   ([`power`]): one session of the BMC tool each, after the check that the BMC at the host's
//!   address has the host's BMC MAC, and serialized per BMC.
//! - [`Coordinator::console_open`], [`Coordinator::console_list`] and
//!   [`Coordinator::console_close`] ([`sessions`]): the one session per host, without contacting
//!   the device; a session nothing attaches to ends at the end of its window
//!   (`--console-attach-s`).
//! - `GET /console/v1/stream/<session>` ([`route`], row C4): the WebSocket that attaches to a
//!   session, with the bearer, or a browser's session cookie and CSRF token, in its subprotocol; after the upgrade, [`stream`] contacts the
//!   device (Serial over LAN through the BMC tool for a Linux server, Screen Sharing's TCP port
//!   for a Mac, [`device`]) and bridges it until either end ends, a limit or a close stops it, and
//!   cleans up (the payload deactivated, one close line).
//! - [`sweep`]: the tick that ends unattached sessions, closes attached ones at their idle and
//!   total limits or when their operator's token (or browser session) or entry is gone, the drain at stop and the
//!   start line.
//!
//! Who may call: an operator's token (a CI token is `forbidden`, no token `unauthorized`) whose
//! name the console operators file lists (`no_console_access` otherwise, and with no file);
//! `Power` and `SetBootDevice` also need the entry's `power` (`power_not_permitted`). A refusal of
//! an operator the file lists is audited as `console_denied` with its cause, but for a request
//! that is malformed (`invalid_request`, `reason_required`, `reason_too_long`) and for one whose
//! audit line could not be written; `List` is a read and is not audited. `Open`, `Power` and
//! `SetBootDevice` write their audit line before anything is done, and do nothing without it
//! (`audit_unavailable`).
//!
//! How a host is found: by its enrollment (`host_not_found` for a host not enrolled, or denied),
//! then the one inventory record its claims match (`ambiguous_record` for more than one). A Mac is
//! a host whose claims carry a Mac serial: its device is its screen, at the address the targets
//! file gives one of its record's data MACs. Any other host is a Linux server: its device is its
//! BMC, at the address of its record's BMC MAC (`no_bmc_mac` without one, and for a Mac's power).
//! A MAC the targets file does not list is `no_address`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use hostkeeper_console::bridge::Moved;
use hostkeeper_console::close::Close;
use hostkeeper_console::ipmi::{Settings, Tool};
use hostkeeper_console::operators::{Denied, Need};
use hostkeeper_console::reason::MAX_REASON_BYTES;
use hostkeeper_console::session::{Bounds, Kind, SessionId, Sessions};
use hostkeeper_console::targets::Address;
use hostkeeper_enrollment::names::MacAddress;
use hostkeeper_proto::hostkeeper::operator::v1::ConsoleRefusal as Refusal;
use hostkeeper_wire::HostId;

use crate::audit_actions::AuditOnly;
use crate::enrollment::nonce::{Random, os_random};
use crate::inventory::{InventoryMatch, match_claims};
use crate::operator::Caller;
use crate::service::Coordinator;

pub(crate) use device::Duplex;
pub use device::{Dial, tcp_dial};
pub(crate) use files::ConsoleFiles;
pub use files::ConsoleFilesError;
pub use flags::ConsoleFlags;
pub use ipmitool::Ipmitool;
pub(crate) use sweep::{drained, sweep_every};

mod device;
mod files;
mod flags;
mod ipmitool;
mod power;
pub(crate) mod route;
mod sessions;
mod stream;
mod sweep;

#[cfg(test)]
pub(crate) mod attach_support;
#[cfg(test)]
mod device_tests;
#[cfg(test)]
mod power_tests;
#[cfg(test)]
mod route_tests;
#[cfg(test)]
mod sessions_tests;
#[cfg(test)]
mod stream_tests;
#[cfg(test)]
pub(crate) mod support;
#[cfg(test)]
mod sweep_tests;
#[cfg(test)]
mod tests;

/// The default most sessions at once (`--console-max-sessions`).
pub const DEFAULT_MAX_SESSIONS: usize = 8;

/// The default window in which a session must be attached (`--console-attach-s`).
pub const DEFAULT_ATTACH: Duration = Duration::from_secs(30);

/// The default time an attached stream may carry no byte either way (`--console-idle-s`).
pub const DEFAULT_IDLE: Duration = Duration::from_secs(30 * 60);

/// The default time a stream may stay attached (`--console-total-s`).
pub const DEFAULT_TOTAL: Duration = Duration::from_secs(8 * 60 * 60);

/// The default time the connection to a Mac's Screen Sharing may take
/// (`--console-connect-timeout-s`).
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The default time the Console page waits for a serial console's first byte before it says that
/// no output came yet (`--console-quiet-s`).
pub const DEFAULT_QUIET: Duration = Duration::from_secs(10);

/// The TCP port of a Mac's Screen Sharing (RFB).
pub const SCREEN_SHARING_PORT: u16 = 5900;

/// The default bounds of the sessions.
#[must_use]
pub const fn default_bounds() -> Bounds {
    Bounds {
        max: DEFAULT_MAX_SESSIONS,
        window: DEFAULT_ATTACH,
        idle: DEFAULT_IDLE,
        total: DEFAULT_TOTAL,
    }
}

/// Why a stream is stopped from outside it, and by whom: an operator's name, or `coordinator`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stop {
    pub(crate) close: Close,
    pub(crate) by: String,
}

/// A stream attached to a session, as the sessions table knows it.
pub(crate) struct Live {
    /// The session's id: the stream's end removes this session and no later one of the host.
    pub(crate) id: SessionId,
    /// Stops the stream; the first cause sent is the one it closes with.
    pub(crate) stop: tokio::sync::watch::Sender<Option<Stop>>,
    /// The bytes it has carried, which the sweep tells the table.
    pub(crate) moved: Arc<Moved>,
    /// What the stream attached with, which the sweep holds it to: an operator token, or the
    /// browser's signed-in session.
    pub(crate) credential: crate::ssh_tunnel::Credential,
}

impl Live {
    /// Stops the stream with `close` by `by`, unless it was stopped already.
    pub(crate) fn stop(&self, close: Close, by: &str) {
        self.stop.send_if_modified(|stop| {
            let first = stop.is_none();
            if first {
                *stop = Some(Stop {
                    close,
                    by: by.to_owned(),
                });
            }
            first
        });
    }
}

/// The sessions, and the streams attached to them, by host id; one lock for both.
pub(crate) struct Table {
    pub(crate) sessions: Sessions,
    pub(crate) live: BTreeMap<String, Live>,
}

/// The console's state: its files, the BMC tool and how it is run, the sessions and their
/// streams, how a Mac is dialled, and one lock per BMC that serializes the commands sent to it.
pub struct Console {
    files: ConsoleFiles,
    tool: Arc<dyn Tool>,
    settings: Settings,
    table: Mutex<Table>,
    /// Told each time a session leaves the table, once its close line is written: the drain
    /// waits on it.
    ended: tokio::sync::Notify,
    dial: Dial,
    /// How long the page waits for a serial console's first byte (`--console-quiet-s`), which
    /// `Open` tells it.
    quiet: Duration,
    bmcs: Mutex<BTreeMap<MacAddress, Arc<Mutex<()>>>>,
    random: Random,
    /// Told when a call finds its BMC's lock held and waits for it: a test's progress event.
    #[cfg(test)]
    waits: Mutex<Option<std::sync::mpsc::Sender<()>>>,
}

impl Default for Console {
    /// No files (nobody has console access), the tool at its default path, the default settings,
    /// sessions and window, and the system's random source.
    fn default() -> Self {
        let settings = Settings::default();
        Self::new(
            ConsoleFiles::default(),
            Arc::new(Ipmitool::new(
                std::path::Path::new(flags::DEFAULT_IPMITOOL),
                &settings,
            )),
            settings,
            Sessions::new(default_bounds()),
        )
    }
}

impl std::fmt::Debug for Console {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Console")
            .field("files", &self.files)
            .field("settings", &self.settings)
            .field("bounds", &self.lock_table().sessions.bounds())
            .field("quiet", &self.quiet)
            .finish_non_exhaustive()
    }
}

impl Console {
    /// The console over `files`, running `tool` as `settings` say, with the empty `sessions`,
    /// dialling a Mac's Screen Sharing port within [`DEFAULT_CONNECT_TIMEOUT`].
    #[must_use]
    pub fn new(
        files: ConsoleFiles,
        tool: Arc<dyn Tool>,
        settings: Settings,
        sessions: Sessions,
    ) -> Self {
        Self {
            files,
            tool,
            settings,
            table: Mutex::new(Table {
                sessions,
                live: BTreeMap::new(),
            }),
            ended: tokio::sync::Notify::new(),
            dial: tcp_dial(SCREEN_SHARING_PORT, DEFAULT_CONNECT_TIMEOUT),
            quiet: DEFAULT_QUIET,
            bmcs: Mutex::new(BTreeMap::new()),
            random: os_random(),
            #[cfg(test)]
            waits: Mutex::new(None),
        }
    }

    /// The files, which the reload tick reads again.
    #[must_use]
    pub fn files(&self) -> &ConsoleFiles {
        &self.files
    }

    /// The same console dialling a Mac with `dial`.
    #[must_use]
    pub fn with_dial(mut self, dial: Dial) -> Self {
        self.dial = dial;
        self
    }

    /// The same console telling the page to wait `quiet` for a serial console's first byte.
    #[must_use]
    pub fn with_quiet(mut self, quiet: Duration) -> Self {
        self.quiet = quiet;
        self
    }

    /// The same console drawing session ids from `random`.
    #[cfg(test)]
    pub(crate) fn with_random(mut self, random: Random) -> Self {
        self.random = random;
        self
    }

    /// Tells `waits` each time a call finds its BMC's lock held.
    #[cfg(test)]
    pub(crate) fn tell_waits(&self, waits: std::sync::mpsc::Sender<()>) {
        *self.waits.lock().unwrap_or_else(PoisonError::into_inner) = Some(waits);
    }

    fn lock_table(&self) -> MutexGuard<'_, Table> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The lock of the BMC whose MAC is `mac`, held while a call sends it commands.
    fn bmc(&self, mac: MacAddress) -> Arc<Mutex<()>> {
        let mut bmcs = self.bmcs.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(bmcs.entry(mac).or_default())
    }
}

/// Holds `gate` for the commands to one BMC: a call that finds it held waits its turn.
fn serialized<'a>(console: &Console, gate: &'a Mutex<()>) -> MutexGuard<'a, ()> {
    if let Ok(held) = gate.try_lock() {
        return held;
    }
    #[cfg(test)]
    console
        .waits
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .for_each(|waits| {
            let _ = waits.send(());
        });
    #[cfg(not(test))]
    let _ = console;
    gate.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the coordinator dials for a host's console: a BMC for a Linux server, a screen for a Mac.
struct Device {
    kind: Kind,
    mac: MacAddress,
    address: Address,
}

impl Coordinator {
    /// The same coordinator with the console `console` instead of the default, which grants
    /// nobody access.
    #[must_use]
    pub fn with_console(mut self, console: Console) -> Self {
        self.console = console;
        self
    }

    /// The console's state.
    #[must_use]
    pub fn console(&self) -> &Console {
        &self.console
    }

    /// The name of `caller` once the operators file lets it make a call that needs `need`.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `no_console_access` or `power_not_permitted`.
    fn console_operator<'c>(&self, caller: &'c Caller, need: Need) -> Result<&'c str, Refusal> {
        self.console_grant(caller, need).map(|(by, _)| by)
    }

    /// The name of `caller` and what it may use, once the operators file lets it make a call
    /// that needs `need`.
    ///
    /// # Errors
    /// As [`Self::console_operator`].
    fn console_grant<'c>(
        &self,
        caller: &'c Caller,
        need: Need,
    ) -> Result<(&'c str, files::Grant), Refusal> {
        let by = caller.operator()?;
        let grant = self.console.files.grant(by, need).map_err(denied)?;
        Ok((by, grant))
    }

    /// The device of the host `host_id`, as the module says; `bmc` asks for the BMC of a Linux
    /// server, which a Mac has not.
    ///
    /// # Errors
    /// `store_failed`, `host_not_found`, `ambiguous_record`, `no_bmc_mac` or `no_address`.
    fn console_device(
        &self,
        host_id: &HostId,
        targets: &hostkeeper_console::targets::Targets,
        bmc: bool,
    ) -> Result<Device, Refusal> {
        let host = self
            .enrolled_host(host_id.as_str())
            .map_err(crate::inventory::unreadable(Refusal::StoreFailed))?
            .ok_or(Refusal::HostNotFound)?;
        let record = self
            .ops
            .read(|state| match match_claims(&host.claims, &state.inventory) {
                InventoryMatch::Matched(record) => Ok(Some(record.clone())),
                InventoryMatch::Unmatched => Ok(None),
                InventoryMatch::Ambiguous(_) => Err(Refusal::AmbiguousRecord),
            })?;
        let screen = !host.claims.mac_serial.is_empty();
        if screen {
            if bmc {
                return Err(Refusal::NoBmcMac);
            }
            let found = record
                .iter()
                .flat_map(|record| &record.data_macs)
                .filter_map(|mac| MacAddress::parse(mac).ok())
                .find_map(|mac| Some((mac, targets.address_of(&mac)?.clone())));
            let (mac, address) = found.ok_or(Refusal::NoAddress)?;
            return Ok(Device {
                kind: Kind::Screen,
                mac,
                address,
            });
        }
        let mac = record
            .and_then(|record| MacAddress::parse(&record.bmc_mac).ok())
            .ok_or(Refusal::NoBmcMac)?;
        let address = targets.address_of(&mac).ok_or(Refusal::NoAddress)?.clone();
        Ok(Device {
            kind: Kind::Serial,
            mac,
            address,
        })
    }

    /// Appends the audit-only line of `action` by `by` concerning `host_id`, with `reason`.
    ///
    /// # Errors
    /// `audit_unavailable`: the line could not be appended (logged).
    fn console_audit(
        &self,
        action: AuditOnly,
        by: &str,
        host_id: Option<&str>,
        reason: &str,
    ) -> Result<(), Refusal> {
        self.ops
            .append_audit_only(action, by, host_id, Some(reason))
            .map(drop)
            .map_err(|why| {
                tracing::error!(%why, action = action.as_str(), "a console audit line was not written");
                Refusal::AuditUnavailable
            })
    }

    /// `cause`, once a `console_denied` line by `by` concerning `host_id` says so after the
    /// operator's `reason` and the `call`; best effort: a line that cannot be written is logged.
    fn console_denied(
        &self,
        by: &str,
        host_id: &HostId,
        call: &str,
        reason: &str,
        cause: Refusal,
    ) -> Refusal {
        let token = cause.as_str_name().trim_start_matches("CONSOLE_REFUSAL_");
        let token = token.to_ascii_lowercase();
        let text =
            hostkeeper_console::reason::audit_reason(reason, &[("call", call), ("cause", &token)]);
        let _ = self.console_audit(AuditOnly::ConsoleDenied, by, Some(host_id.as_str()), &text);
        cause
    }
}

/// The refusal of an operator the operators file does not let make a call.
fn denied(why: Denied) -> Refusal {
    match why {
        Denied::NoConsoleAccess => Refusal::NoConsoleAccess,
        Denied::PowerNotPermitted => Refusal::PowerNotPermitted,
    }
}

/// The host id of a request.
///
/// # Errors
/// `invalid_request`: it is not a node id.
fn host_id(text: &str) -> Result<HostId, Refusal> {
    text.parse().map_err(|_| Refusal::InvalidRequest)
}

/// The reason of `Open`, `Power` and `SetBootDevice`, trimmed.
///
/// # Errors
/// `reason_required` (empty once trimmed), `reason_too_long` (over 256 bytes), or
/// `invalid_request` (a control character).
fn required_reason(text: &str) -> Result<String, Refusal> {
    let text = text.trim();
    if text.is_empty() {
        Err(Refusal::ReasonRequired)
    } else if text.len() > MAX_REASON_BYTES {
        Err(Refusal::ReasonTooLong)
    } else if text.chars().any(char::is_control) {
        Err(Refusal::InvalidRequest)
    } else {
        Ok(text.to_owned())
    }
}
