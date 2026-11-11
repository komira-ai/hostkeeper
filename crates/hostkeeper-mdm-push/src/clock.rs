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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_clock_says_a_time_after_2023() {
        // Not a measure of time: a clock that read 0 (before the epoch) would say 0.
        assert!(SystemClock.now_unix() > 1_700_000_000);
    }
}
