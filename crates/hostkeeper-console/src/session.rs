//! Console sessions (`docs/design/console-broker.md`, decisions D2, D11 and D12): one per host,
//! at most a set number in all, each with the window in which a WebSocket must attach to it.
//!
//! The table is pure: the caller passes the time, as the time since its start, and the session's
//! id, which the caller draws at random. What the table holds is what `Console/List` shows; an
//! id is never in that list.

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

/// The bytes of a session's id: 256 bits.
pub const SESSION_ID_BYTES: usize = 32;

/// A session's id: 256 random bits, written as 64 lower-case hex digits. It is not a credential:
/// attaching also takes the bearer of the operator name that opened the session.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SessionId([u8; SESSION_ID_BYTES]);

impl SessionId {
    /// The id of `bytes`.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; SESSION_ID_BYTES]) -> Self {
        Self(bytes)
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

impl fmt::Debug for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SessionId({self})")
    }
}

/// What a session carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A Linux server's serial port, over its BMC's Serial over LAN.
    Serial,
    /// A Mac's screen.
    Screen,
}

impl Kind {
    /// The kind's name, as the audit line writes it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Kind::Serial => "serial",
            Kind::Screen => "screen",
        }
    }
}

/// An open session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    /// Its id.
    pub id: SessionId,
    /// The host's id.
    pub host_id: String,
    /// What it carries.
    pub kind: Kind,
    /// The operator name that opened it.
    pub opened_by: String,
    /// When it was opened, in unix seconds.
    pub opened_unix: u64,
    /// When it was opened, as the time since the caller's start.
    pub opened_at: Duration,
    /// The reason it was opened with.
    pub reason: String,
    /// Whether a WebSocket is attached to it.
    pub attached: bool,
}

/// Why a session cannot be opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The host has a session (`console_busy`).
    Busy,
    /// The table holds as many sessions as it allows (`too_many_sessions`).
    Full,
}

/// The open sessions, by host id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sessions {
    by_host: BTreeMap<String, Session>,
    max: usize,
    window: Duration,
}

impl Sessions {
    /// An empty table of at most `max` sessions, each of which must be attached within `window`.
    #[must_use]
    pub fn new(max: usize, window: Duration) -> Self {
        Self {
            by_host: BTreeMap::new(),
            max,
            window,
        }
    }

    /// The window in which a session must be attached.
    #[must_use]
    pub fn window(&self) -> Duration {
        self.window
    }

    /// Whether a session of `host_id` can be opened now. The caller writes the session's audit
    /// line between this and [`Self::insert`], holding the table all the while.
    ///
    /// # Errors
    /// [`Refused`]: the host has a session, or the table is full.
    pub fn admit(&self, host_id: &str) -> Result<(), Refused> {
        if self.by_host.contains_key(host_id) {
            Err(Refused::Busy)
        } else if self.by_host.len() >= self.max {
            Err(Refused::Full)
        } else {
            Ok(())
        }
    }

    /// Holds `session`, which [`Self::admit`] admitted.
    pub fn insert(&mut self, session: Session) {
        self.by_host.insert(session.host_id.clone(), session);
    }

    /// Ends and answers every session not attached within its window by `now`, by host id.
    pub fn expire(&mut self, now: Duration) -> Vec<Session> {
        let window = self.window;
        let (ended, kept) =
            std::mem::take(&mut self.by_host)
                .into_iter()
                .partition(|(_, session)| {
                    !session.attached && now >= session.opened_at.saturating_add(window)
                });
        self.by_host = kept;
        ended.into_values().collect()
    }

    /// Ends and answers the session of `host_id`, if it has one.
    pub fn close(&mut self, host_id: &str) -> Option<Session> {
        self.by_host.remove(host_id)
    }

    /// The sessions, by host id.
    pub fn iter(&self) -> impl Iterator<Item = &Session> {
        self.by_host.values()
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
