//! The wall clock, and the only three questions this crate asks it: is renewal due, has the
//! certificate expired, and is a certificate just issued valid now.
//!
//! Nothing else in the crate reads the wall clock: retries are paced by the monotonic
//! `hostkeeper_steps::Clock`. A [`WallClock`] is asked through a [`Site`], which only this module
//! can make, so a read anywhere else does not compile; the wall-clock lint
//! (`crates/xtask/tests/wallclock`) allows the one real read, in [`SystemWallClock`], in this file
//! only.
//!
//! Times are whole seconds since the Unix epoch. A certificate is valid from `not_before` through
//! `not_after`, both included (the Validity field of RFC 5280).

use std::time::{SystemTime, UNIX_EPOCH};

/// Who reads the wall clock. Only this module makes one: the three checks below.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Site(&'static str);

impl Site {
    /// The name of the check: `renewal_due`, `expired` or `leaf_valid_now`.
    pub fn name(self) -> &'static str {
        self.0
    }
}

const RENEWAL_DUE: Site = Site("renewal_due");
const EXPIRED: Site = Site("expired");
const LEAF_VALID_NOW: Site = Site("leaf_valid_now");

/// The wall clock: seconds since the Unix epoch, read for `site`.
pub trait WallClock: Send {
    fn now_unix_secs(&self, site: Site) -> u64;
}

/// The host's wall clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemWallClock;

impl WallClock for SystemWallClock {
    fn now_unix_secs(&self, _site: Site) -> u64 {
        unix_secs(SystemTime::now())
    }
}

/// Whole seconds from the epoch to `time`; `0` for a time before it.
fn unix_secs(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Renewal is due from the middle of the validity on: `not_before + (not_after - not_before) / 2`.
pub(crate) fn renewal_due(wall: &dyn WallClock, not_before: u64, not_after: u64) -> bool {
    let midpoint = not_before + not_after.saturating_sub(not_before) / 2;
    wall.now_unix_secs(RENEWAL_DUE) >= midpoint
}

/// The certificate expired: the wall clock is past `not_after`, so the certificate cannot
/// authenticate a renewal.
pub(crate) fn expired(wall: &dyn WallClock, not_after: u64) -> bool {
    wall.now_unix_secs(EXPIRED) > not_after
}

/// A certificate just issued is valid now.
pub(crate) fn leaf_valid_now(wall: &dyn WallClock, not_before: u64, not_after: u64) -> bool {
    (not_before..=not_after).contains(&wall.now_unix_secs(LEAF_VALID_NOW))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    struct At(u64);

    impl WallClock for At {
        fn now_unix_secs(&self, _site: Site) -> u64 {
            self.0
        }
    }

    /// Catches a time before the epoch wrapping to a huge number instead of 0.
    #[test]
    fn a_time_before_the_epoch_is_zero() {
        assert_eq!(unix_secs(UNIX_EPOCH - Duration::from_secs(5)), 0);
        assert_eq!(unix_secs(UNIX_EPOCH + Duration::from_millis(7_900)), 7);
    }

    /// Catches the host's wall clock reading nothing: it counts from the epoch, which is long
    /// past on any host this runs on. Nothing is asserted about how two reads relate.
    #[test]
    fn the_system_wall_clock_counts_from_the_epoch() {
        assert!(SystemWallClock.now_unix_secs(LEAF_VALID_NOW) > 0);
    }

    /// Catches an off-by-one at either end of the validity window or at its middle.
    #[test]
    fn the_windows_include_their_ends() {
        assert!(!leaf_valid_now(&At(99), 100, 200));
        assert!(leaf_valid_now(&At(100), 100, 200));
        assert!(leaf_valid_now(&At(200), 100, 200));
        assert!(!leaf_valid_now(&At(201), 100, 200));
        assert!(!expired(&At(200), 200));
        assert!(expired(&At(201), 200));
        assert!(!renewal_due(&At(149), 100, 200));
        assert!(renewal_due(&At(150), 100, 200));
        // An inverted window has its middle at `not_before`.
        assert!(renewal_due(&At(300), 300, 200));
    }

    #[test]
    fn each_check_has_its_name() {
        assert_eq!(
            [RENEWAL_DUE, EXPIRED, LEAF_VALID_NOW].map(Site::name),
            ["renewal_due", "expired", "leaf_valid_now"]
        );
    }
}
