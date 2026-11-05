//! The heartbeat loop's unix clock (`hostkeeper_link::UnixClock`): the second a deferral's retry
//! time is counted from. The deferral file keeps it in unix seconds, the format kbf's agent
//! shares, because a restart has no memory of the monotonic clock; the loop caps a deferral at
//! `hostkeeper_link::deferral::MAX_RETRY_SECS` from now at every read, so a wrong clock can run
//! a deferred target early, or hold it for at most that long, and nothing else. This file is
//! the daemon's one read of the wall clock (crates/xtask/tests/wallclock/allowed.txt).

use std::time::{SystemTime, UNIX_EPOCH};

use hostkeeper_link::UnixClock;

/// The host's wall clock in unix seconds; `0` for a clock set before 1970.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemUnixClock;

impl UnixClock for SystemUnixClock {
    fn unix_secs(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }
}
