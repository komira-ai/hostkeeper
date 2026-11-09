//! The runner's clocks, behind a port so its tests move time themselves.
//!
//! The wall clock only stamps records ([`Clock::stamp_ms`]); every deadline and poll reads the
//! monotonic clock ([`Clock::mono_ms`]), so a wall clock stepped during a run changes no decision.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The runner's clocks and its sleep.
pub trait Clock {
    /// Unix milliseconds, the stamp of a record.
    fn stamp_ms(&self) -> u64;
    /// Milliseconds of a clock that never goes back, for deadlines.
    fn mono_ms(&self) -> u64;
    fn sleep_ms(&self, ms: u64);
}

/// The machine's clocks.
#[derive(Debug)]
pub struct SystemClock {
    start: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn stamp_ms(&self) -> u64 {
        let since = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO);
        u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
    }

    fn mono_ms(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn sleep_ms(&self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }
}
