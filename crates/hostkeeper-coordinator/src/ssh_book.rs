//! The book of the SSH tunnels (docs/design/ssh-through-coordinator.md, sections 7 and 8): where
//! each host was last heard from, the host keys it reports and the ones an operator pinned, and
//! the tunnels open on every coordinator instance.
//!
//! Like the ledger of the certificates ([`crate::ssh_ledger`]), every time here is the database's
//! clock and every write that says something happened writes its audit line in the same
//! transaction, chained with the certificates' lines: a tunnel whose line cannot be written is not
//! opened. Nothing another instance needs is held in a process: an instance knows its own tunnels'
//! sockets, and the book knows the rest, so one tunnel per operator per host holds across any
//! number of instances, and a tunnel whose instance vanished is closed in the audit by another's
//! reaper.

use serde::Serialize;

/// The `action` of the line of a tunnel opened.
pub(crate) const TUNNEL_OPEN: &str = "ssh_tunnel_open";
/// The `action` of the line of a tunnel closed.
pub(crate) const TUNNEL_CLOSE: &str = "ssh_tunnel_close";
/// The `action` of the line written when a host is heard from another address than before.
pub(crate) const ADDRESS_CHANGED: &str = "ssh_address_changed";
/// The `action` of the line written when a host reports other host keys than the pinned ones.
pub(crate) const HOSTKEY_CHANGED: &str = "ssh_hostkey_changed";
/// The `action` of the line of host keys an operator pinned.
pub(crate) const HOSTKEY_PINNED: &str = "ssh_hostkey_pinned";

/// Why a tunnel was closed: the closed set of `Close.cause` (ssh.proto).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Cause {
    /// The client ended the stream.
    Eof,
    /// No byte moved either way for `--ssh-idle-s`.
    Idle,
    /// The tunnel was open for `--ssh-max-session-s`.
    MaxSession,
    /// The operator's token is gone.
    TokenRevoked,
    /// The certificate the tunnel was opened with was revoked.
    Revoked,
    /// The operators file no longer permits the operator the host.
    NotPermitted,
    /// The host closed the connection.
    HostClosed,
    /// The instance stopped, after its drain.
    CoordinatorStopping,
    /// The instance that held the tunnel stopped refreshing it (written by another's reaper).
    InstanceLost,
    /// The stream or the connection failed.
    Error,
}

impl Cause {
    /// The token of the cause, as `Close.cause` and the audit carry it.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Eof => "eof",
            Self::Idle => "idle",
            Self::MaxSession => "max_session",
            Self::TokenRevoked => "token_revoked",
            Self::Revoked => "revoked",
            Self::NotPermitted => "not_permitted",
            Self::HostClosed => "host_closed",
            Self::CoordinatorStopping => "coordinator_stopping",
            Self::InstanceLost => "instance_lost",
            Self::Error => "error",
        }
    }
}

/// A tunnel to be opened, as the book checks and records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Opening {
    /// The caller.
    pub(crate) operator: String,
    /// The host asked for.
    pub(crate) host_id: String,
    /// The certificate serial the client says it holds.
    pub(crate) serial: u64,
    /// The instance that will hold the tunnel.
    pub(crate) instance: String,
    /// How recent the host's last heartbeat must be, in seconds (`--ssh-live-s`).
    pub(crate) live_secs: u64,
    /// The most tunnels the operator may hold over every instance
    /// (`--ssh-max-tunnels-per-operator`).
    pub(crate) per_operator: u64,
}

/// A tunnel the book recorded: its id, the address to dial (the host's last heartbeat's peer
/// address, without a port) and when it was opened, by the database's clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Opened {
    pub(crate) id: u64,
    pub(crate) address: String,
    pub(crate) opened_unix: u64,
}

/// Why the book did not open a tunnel. Nothing was recorded.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum OpenRefused {
    /// No certificate of that serial was made for the caller and the host, or it expired or was
    /// revoked.
    #[error("bad_serial")]
    BadSerial,
    /// The host was never heard from with an address.
    #[error("no_address")]
    NoAddress,
    /// The host's last heartbeat is older than `live_secs`.
    #[error("host_not_live")]
    HostNotLive,
    /// The operator holds a tunnel to the host already, or `per_operator` tunnels.
    #[error("too_many_tunnels")]
    TooMany,
    /// The database could not take the write (the audit line among it).
    #[error("the book could not be written: {0}")]
    Unwritten(String),
}

/// A tunnel closed by its instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Closing {
    pub(crate) id: u64,
    pub(crate) cause: Cause,
    /// Bytes from the client to the host.
    pub(crate) bytes_up: u64,
    /// Bytes from the host to the client.
    pub(crate) bytes_down: u64,
}

/// What a poll of an instance found: which of its tunnels must close and why, and how many
/// tunnels of lost instances it closed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Polled {
    /// The ids among the instance's own whose certificate was revoked.
    pub(crate) revoked: Vec<u64>,
    /// The ids among the instance's own the book no longer holds (another's reaper closed them).
    pub(crate) gone: Vec<u64>,
    /// How many tunnels of other instances were closed as `instance_lost`.
    pub(crate) reaped: u64,
}

/// The host keys of a host: the pinned set, if any, and the set its last heartbeat reported.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct HostKeys {
    pub(crate) pinned: Option<Vec<String>>,
    pub(crate) reported: Vec<String>,
}

impl HostKeys {
    /// `HostKeysResponse.state`: `no_pin` without a pin; `changed` when the host reports a set
    /// that is not the pinned one; else `pinned`. A host that reports nothing is not `changed`.
    pub(crate) fn state(&self) -> &'static str {
        match &self.pinned {
            None => "no_pin",
            Some(pinned) if !self.reported.is_empty() && !same_set(pinned, &self.reported) => {
                "changed"
            }
            Some(_) => "pinned",
        }
    }
}

/// Whether `a` and `b` hold the same fingerprints, in any order.
pub(crate) fn same_set(a: &[String], b: &[String]) -> bool {
    let mut a: Vec<&str> = a.iter().map(String::as_str).collect();
    let mut b: Vec<&str> = b.iter().map(String::as_str).collect();
    a.sort_unstable();
    a.dedup();
    b.sort_unstable();
    b.dedup();
    a == b
}

/// Why a pin was not written.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum PinRefused {
    /// The fingerprints are not the set the host's last heartbeat reported (or it reported none).
    #[error("not_reported")]
    NotReported,
    /// The database could not take the write (the audit line among it).
    #[error("the book could not be written: {0}")]
    Unwritten(String),
}

/// Why a read or a plain write of the book failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the book could not be read or written: {0}")]
pub(crate) struct BookError(pub(crate) String);

/// Where the book is kept: the database's tables (`ssh_host`, `ssh_host_key_pin`, `ssh_tunnel`,
/// and the audit chain of [`crate::ssh_ledger`]), or, in a test, memory. Calls block: an async
/// caller runs them off its runtime's threads.
pub(crate) trait Book: Send + Sync {
    /// Notes a heartbeat of `host_id` from `address` (an IP address, no port) reporting
    /// `reported` host keys, at the database's time. Writes `ssh_address_changed` when an earlier
    /// heartbeat came from another address, and `ssh_hostkey_changed` when a pin exists and the
    /// reported set differs from it and from the set reported before (once per change, not per
    /// heartbeat). A heartbeat with no report (`reported` empty) keeps the reported set.
    ///
    /// # Errors
    /// [`BookError`]: nothing was noted.
    fn heard(&self, host_id: &str, address: &str, reported: &[String]) -> Result<(), BookError>;

    /// The host keys of `host_id`.
    ///
    /// # Errors
    /// [`BookError`].
    fn host_keys(&self, host_id: &str) -> Result<HostKeys, BookError>;

    /// Pins `fingerprints` for `host_id` by `by`, replacing an earlier pin, with its
    /// `ssh_hostkey_pinned` line: only when they are, as a set, what the host's last heartbeat
    /// reported.
    ///
    /// # Errors
    /// [`PinRefused`]: nothing was written.
    fn pin(&self, by: &str, host_id: &str, fingerprints: &[String]) -> Result<(), PinRefused>;

    /// Checks and records a tunnel, with its `ssh_tunnel_open` line, in one write: the serial is
    /// a certificate made for the operator and the host, unexpired and unrevoked; the host was
    /// heard from within `live_secs`; the operator holds no tunnel to the host and fewer than
    /// `per_operator` in all.
    ///
    /// # Errors
    /// [`OpenRefused`]: nothing was recorded.
    fn open(&self, opening: &Opening) -> Result<Opened, OpenRefused>;

    /// Removes the tunnel and writes its `ssh_tunnel_close` line (bytes each way, seconds open,
    /// cause) in one write. A tunnel the book no longer holds (another instance's reaper closed
    /// it, and wrote its line) is left as it is.
    ///
    /// # Errors
    /// [`BookError`].
    fn close(&self, closing: &Closing) -> Result<(), BookError>;

    /// The poll of `instance`, holding the tunnels `ids`, in one write: refreshes their last-seen
    /// time; finds those whose certificate was revoked and those the book no longer holds; and
    /// closes, as `instance_lost`, every tunnel of another instance not refreshed for
    /// `stale_secs`.
    ///
    /// # Errors
    /// [`BookError`].
    fn poll(&self, instance: &str, ids: &[u64], stale_secs: u64) -> Result<Polled, BookError>;
}

/// The detail of the line of a tunnel opened.
pub(crate) fn open_detail(id: u64, serial: u64, address: &str, instance: &str) -> String {
    #[derive(Serialize)]
    struct Open<'a> {
        tunnel: u64,
        serial: u64,
        address: &'a str,
        instance: &'a str,
    }
    json(&Open {
        tunnel: id,
        serial,
        address,
        instance,
    })
}

/// The detail of the line of a tunnel closed; the byte counts are absent when the instance that
/// held it was lost.
pub(crate) fn close_detail(
    id: u64,
    cause: Cause,
    bytes: Option<(u64, u64)>,
    seconds_open: u64,
) -> String {
    #[derive(Serialize)]
    struct Close {
        tunnel: u64,
        cause: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        bytes_up: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        bytes_down: Option<u64>,
        seconds_open: u64,
    }
    json(&Close {
        tunnel: id,
        cause: cause.as_str(),
        bytes_up: bytes.map(|(up, _)| up),
        bytes_down: bytes.map(|(_, down)| down),
        seconds_open,
    })
}

/// The detail of the line of a changed address.
pub(crate) fn address_detail(before: &str, after: &str) -> String {
    #[derive(Serialize)]
    struct Address<'a> {
        before: &'a str,
        after: &'a str,
    }
    json(&Address { before, after })
}

/// The detail of the line of host keys pinned, or reported other than the pin.
pub(crate) fn keys_detail(pinned: &[String], reported: &[String]) -> String {
    #[derive(Serialize)]
    struct Keys<'a> {
        pinned: &'a [String],
        reported: &'a [String],
    }
    json(&Keys { pinned, reported })
}

/// `value` as JSON text, its keys in the order its struct declares them: the text is hashed into
/// the chain.
fn json(value: &impl Serialize) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

#[cfg(test)]
pub(crate) use memory::Memory;

#[cfg(test)]
#[path = "ssh_book_memory.rs"]
mod memory;

#[cfg(test)]
#[path = "ssh_book_tests.rs"]
mod tests;
