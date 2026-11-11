//! The clock the push policy and the sender read, injected so that a test sets the time.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch.
pub trait Clock: Send + Sync {
    /// The time now, in whole seconds since the epoch.
    fn now_unix(&self) -> u64;
}

/// The system's clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs())
    }
}
