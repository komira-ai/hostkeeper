//! Console sessions (`docs/design/console-broker.md`, decisions D2, D3, D4, D11 and D12): one per
//! host, at most a set number in all, each with the window in which a WebSocket must attach to
//! it, and, once attached, the idle and total limits that close it.
//!
//! The table is pure: the caller passes the time, as the time since its start, and the session's
//! id, which the caller draws at random, and tells it how many bytes a stream has carried so far.
//! What the table holds is what `Console/List` shows; an id is never in that list.

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

/// The bytes of a session's id: 256 bits.
pub const SESSION_ID_BYTES: usize = 32;

/// A session's id: 256 random bits, written as 64 lower-case hex digits. It is not a credential:
/// attaching also takes the bearer of the operator name that opened the session.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SessionId([u8; SESSION_ID_BYTES]);

/// Why text is no session id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a session id is 64 lower-case hex digits")]
pub struct BadSessionId;

impl SessionId {
    /// The id of `bytes`.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; SESSION_ID_BYTES]) -> Self {
        Self(bytes)
    }

    /// The id `text` writes: exactly 64 lower-case hex digits.
    ///
    /// # Errors
    /// [`BadSessionId`]: another length, or a character that is not `0-9a-f`.
    pub fn parse(text: &str) -> Result<Self, BadSessionId> {
        let digits = text.as_bytes();
        if digits.len() != SESSION_ID_BYTES * 2 {
            return Err(BadSessionId);
        }
        let mut bytes = [0u8; SESSION_ID_BYTES];
        for (byte, pair) in bytes.iter_mut().zip(digits.chunks_exact(2)) {
            *byte = (nibble(pair[0])? << 4) | nibble(pair[1])?;
        }
        Ok(Self(bytes))
    }
}

/// The value of one lower-case hex digit.
fn nibble(digit: u8) -> Result<u8, BadSessionId> {
    match digit {
        b'0'..=b'9' => Ok(digit - b'0'),
        b'a'..=b'f' => Ok(digit - b'a' + 10),
        _ => Err(BadSessionId),
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

/// The WebSocket attached to a session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attachment {
    /// When it attached, as the time since the caller's start.
    pub at: Duration,
    /// When a byte last crossed the stream either way, as far as the caller told
    /// ([`Sessions::observe`]); at first, when it attached.
    pub active_at: Duration,
    /// The bytes the stream had carried, both ways together, when the caller last told.
    pub moved: u64,
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
    /// The WebSocket attached to it, once one is.
    pub attachment: Option<Attachment>,
}

/// Why a session cannot be opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The host has a session (`console_busy`).
    Busy,
    /// The table holds as many sessions as it allows (`too_many_sessions`).
    Full,
}

/// Why a WebSocket cannot attach to a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachRefused {
    /// No session has the id, or its window to be attached has passed.
    NoSession,
    /// Another operator name opened the session.
    NotOpener,
    /// A WebSocket is attached to it already.
    Attached,
}

/// Which limit closes an attached session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limit {
    /// No byte crossed the stream for the idle limit.
    Idle,
    /// The stream has been attached for the total limit.
    Total,
}

/// The table's bounds: `--console-max-sessions`, `--console-attach-s`, `--console-idle-s` and
/// `--console-total-s`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    /// The most sessions at once, attached or not.
    pub max: usize,
    /// How long a session waits to be attached.
    pub window: Duration,
    /// How long an attached stream may carry no byte either way.
    pub idle: Duration,
    /// How long a stream may stay attached.
    pub total: Duration,
}

/// The open sessions, by host id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sessions {
    by_host: BTreeMap<String, Session>,
    bounds: Bounds,
}

impl Sessions {
    /// An empty table within `bounds`.
    #[must_use]
    pub fn new(bounds: Bounds) -> Self {
        Self {
            by_host: BTreeMap::new(),
            bounds,
        }
    }

    /// The window in which a session must be attached.
    #[must_use]
    pub fn window(&self) -> Duration {
        self.bounds.window
    }

    /// The table's bounds.
    #[must_use]
    pub fn bounds(&self) -> Bounds {
        self.bounds
    }

    /// Whether a session of `host_id` can be opened now. The caller writes the session's audit
    /// line between this and [`Self::insert`], holding the table all the while. An attached
    /// session counts against the most as an unattached one does: each WebSocket upgraded for the
    /// console holds one.
    ///
    /// # Errors
    /// [`Refused`]: the host has a session, or the table is full.
    pub fn admit(&self, host_id: &str) -> Result<(), Refused> {
        if self.by_host.contains_key(host_id) {
            Err(Refused::Busy)
        } else if self.by_host.len() >= self.bounds.max {
            Err(Refused::Full)
        } else {
            Ok(())
        }
    }

    /// Holds `session`, which [`Self::admit`] admitted.
    pub fn insert(&mut self, session: Session) {
        self.by_host.insert(session.host_id.clone(), session);
    }

    /// Attaches a WebSocket of the operator `by` to the session `id` at `now`, and answers the
    /// session. The checks, in order: the session exists, `by` opened it, it is not attached, and
    /// its window has not passed (a session whose window has passed is no session, even before
    /// [`Self::expire`] ends it).
    ///
    /// # Errors
    /// [`AttachRefused`]: no such session in its window, another operator's, or attached already.
    pub fn attach(
        &mut self,
        id: &SessionId,
        by: &str,
        now: Duration,
    ) -> Result<&Session, AttachRefused> {
        let window = self.bounds.window;
        let session = self
            .by_host
            .values_mut()
            .find(|session| session.id == *id)
            .ok_or(AttachRefused::NoSession)?;
        // Another operator learns nothing of the session's state.
        if session.opened_by != by {
            return Err(AttachRefused::NotOpener);
        }
        if session.attachment.is_some() {
            return Err(AttachRefused::Attached);
        }
        if now >= session.opened_at.saturating_add(window) {
            return Err(AttachRefused::NoSession);
        }
        session.attachment = Some(Attachment {
            at: now,
            active_at: now,
            moved: 0,
        });
        Ok(session)
    }

    /// Tells the table that the stream of the session of `host_id` has carried `moved` bytes so
    /// far, both ways together, as of `now`: when that is more than it was last told, a byte
    /// crossed, and the session was active at `now`.
    pub fn observe(&mut self, host_id: &str, moved: u64, now: Duration) {
        let attachment = self
            .by_host
            .get_mut(host_id)
            .and_then(|session| session.attachment.as_mut());
        if let Some(attachment) = attachment.filter(|attachment| moved != attachment.moved) {
            attachment.moved = moved;
            attachment.active_at = now;
        }
    }

    /// The attached sessions a limit closes at `now`, by host id, with the limit: the total limit
    /// first when both are reached. The sessions stay in the table until their stream has ended
    /// and the caller removes them.
    #[must_use]
    pub fn due(&self, now: Duration) -> Vec<(String, Limit)> {
        let Bounds { idle, total, .. } = self.bounds;
        self.by_host
            .values()
            .filter_map(|session| {
                let attachment = session.attachment.as_ref()?;
                let limit = if now >= attachment.at.saturating_add(total) {
                    Limit::Total
                } else if now >= attachment.active_at.saturating_add(idle) {
                    Limit::Idle
                } else {
                    return None;
                };
                Some((session.host_id.clone(), limit))
            })
            .collect()
    }

    /// Ends and answers every session not attached within its window by `now`, by host id.
    pub fn expire(&mut self, now: Duration) -> Vec<Session> {
        let window = self.bounds.window;
        let (ended, kept) =
            std::mem::take(&mut self.by_host)
                .into_iter()
                .partition(|(_, session)| {
                    session.attachment.is_none() && now >= session.opened_at.saturating_add(window)
                });
        self.by_host = kept;
        ended.into_values().collect()
    }

    /// Ends and answers the session of `host_id`, if it has one.
    pub fn close(&mut self, host_id: &str) -> Option<Session> {
        self.by_host.remove(host_id)
    }

    /// Ends and answers the session `id`, if it is still in the table: the end of an attached
    /// stream removes its own session and never a later one of the same host.
    pub fn remove(&mut self, id: &SessionId) -> Option<Session> {
        let host = self
            .by_host
            .values()
            .find(|session| session.id == *id)
            .map(|session| session.host_id.clone())?;
        self.by_host.remove(&host)
    }

    /// The session of `host_id`, if it has one.
    #[must_use]
    pub fn get(&self, host_id: &str) -> Option<&Session> {
        self.by_host.get(host_id)
    }

    /// The sessions, by host id.
    pub fn iter(&self) -> impl Iterator<Item = &Session> {
        self.by_host.values()
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
