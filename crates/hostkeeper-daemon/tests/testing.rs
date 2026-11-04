//! The test doubles themselves, where the tests of the daemon do not reach every path of them.

use std::sync::atomic::Ordering;

use hostkeeper_daemon::testing::{Counted, FakeClock, ScriptedLauncher};
use hostkeeper_steps::{Clock, StepLauncher};
use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};

/// Catches a fake clock that moves on its own, or does not move when slept on: a sleep moves
/// it, a frozen one only by `advance`, and a new boot id is reported.
#[test]
fn the_fake_clock_moves_only_when_told() {
    let clock = FakeClock::new();
    let start = clock.mono_secs();
    clock.sleep_secs(5);
    assert_eq!(clock.mono_secs(), start + 5);
    let frozen = FakeClock::frozen();
    frozen.sleep_secs(5);
    assert_eq!(frozen.mono_secs(), start);
    frozen.advance(2);
    assert_eq!(frozen.mono_secs(), start + 2);
    assert_eq!(clock.boot_id(), "boot-1");
    clock.set_boot_id("boot-2");
    assert_eq!(clock.boot_id(), "boot-2");
}

/// Catches a scripted launcher whose stops always fail: a stop of a service not marked failing
/// works, and is seen.
#[test]
fn a_stop_the_script_does_not_fail_works() {
    let launcher = ScriptedLauncher::new();
    let seen = launcher.stops_seen();
    launcher.fail_stops("other");
    launcher
        .stop("updater-web-prepare-1")
        .expect("a stop that works");
    assert!(launcher.stop("updater-other-prepare-1").is_err());
    assert_eq!(seen.recv().expect("seen"), "updater-web-prepare-1");
    assert_eq!(launcher.stops().len(), 2);
}

/// Catches a counting connection that counts what it does not read, or does not pass writes,
/// flushes and shutdowns through.
#[tokio::test]
async fn a_counted_connection_counts_what_is_read_and_passes_the_rest() {
    let (near, far) = duplex(64);
    let (mut counted, read) = Counted::new(far);
    let (mut near_read, mut near_write) = tokio::io::split(near);
    near_write.write_all(b"abc").await.expect("write");
    let mut buf = [0u8; 3];
    counted.read_exact(&mut buf).await.expect("read");
    assert_eq!((&buf, read.load(Ordering::SeqCst)), (b"abc", 3));
    counted.write_all(b"de").await.expect("write back");
    counted.flush().await.expect("flush");
    counted.shutdown().await.expect("shutdown");
    let mut back = Vec::new();
    near_read
        .read_to_end(&mut back)
        .await
        .expect("read to the end");
    assert_eq!(back, b"de");
}
