//! Certificate validity: the clock port, and the window a certificate is valid for.
//!
//! This is the one module of the crate that reads the wall clock ([`SystemClock`]); the
//! wall-clock lint (crates/xtask/tests/wallclock) allows this path and no other of the crate.
//! Everything else that needs the time takes a [`UnixClock`] or a `now` argument.

use std::time::{SystemTime, UNIX_EPOCH};

use time::OffsetDateTime;

/// The time, as unix seconds.
pub trait UnixClock: Send + Sync {
    /// Seconds since the unix epoch.
    fn now_unix(&self) -> u64;
}

/// The system's wall clock. A clock before the epoch reads as 0.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl UnixClock for SystemClock {
    fn now_unix(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs())
    }
}

/// The window a certificate is valid for, in unix seconds, both ends included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    /// The first second of validity: the clock's `now` at signing.
    pub not_before: u64,
    /// `not_before` plus the validity.
    pub not_after: u64,
}

impl Window {
    /// The window of `secs` seconds from `now`, or `None` when its end does not fit a
    /// certificate's time (past the year 9999, or past `u64`).
    pub fn from_now(now: u64, secs: u64) -> Option<Window> {
        let not_after = now.checked_add(secs)?;
        // Both ends must be times a certificate can hold.
        to_time(now)?;
        to_time(not_after)?;
        Some(Window {
            not_before: now,
            not_after,
        })
    }

    /// The window's ends in the type rcgen takes.
    pub(crate) fn times(&self) -> (OffsetDateTime, OffsetDateTime) {
        // `from_now` checked both ends, so neither falls back.
        let start = to_time(self.not_before).unwrap_or(OffsetDateTime::UNIX_EPOCH);
        let end = to_time(self.not_after).unwrap_or(OffsetDateTime::UNIX_EPOCH);
        (start, end)
    }
}

/// `secs` after the epoch, when it is before the year 10000 (the `time` crate's range without
/// its `large-dates` feature).
fn to_time(secs: u64) -> Option<OffsetDateTime> {
    let secs = i64::try_from(secs).ok()?;
    OffsetDateTime::from_unix_timestamp(secs).ok()
}
