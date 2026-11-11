use std::time::Duration;

use super::{Clock, SystemClock};

/// Proves: the system clock reads the wall time in unix seconds (after the year 2020), and its
/// sleep returns.
#[tokio::test]
async fn the_system_clock_reads_the_time_and_sleeps() {
    assert!(SystemClock.now_unix() > 1_600_000_000);
    SystemClock.sleep(Duration::ZERO).await;
}
