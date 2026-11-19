use std::sync::mpsc;
use std::thread;

use super::Writers;

/// Proves: the thread that holds the lock takes it again at once, and it is free only when every
/// take of it is dropped; a try is refused until then, the holder's own included.
/// Catches: a lock that deadlocks its own holder (a decision that holds the stores' writers'
/// locks, then makes a store's change on the same thread), or frees it at the inner drop.
#[test]
fn the_holder_takes_the_lock_again_and_frees_it_at_the_last_drop() {
    let writers = Writers::default();
    let outer = writers.lock();
    let inner = writers.lock();
    assert!(
        writers.try_lock().is_none(),
        "the holder's own try is refused"
    );
    let again = writers.lock();
    let other =
        |writers: &Writers| thread::scope(|s| s.spawn(|| writers.try_lock().is_some()).join());
    assert_eq!(
        other(&writers).ok(),
        Some(false),
        "held: another thread's try is refused"
    );
    drop(again);
    drop(inner);
    assert_eq!(
        other(&writers).ok(),
        Some(false),
        "still held by the outer take"
    );
    drop(outer);
    assert_eq!(other(&writers).ok(), Some(true), "free after the last drop");
}

/// Proves: a thread that asks while another holds the lock waits, and takes it once the holder
/// lets go.
/// Catches: a lock that lets two threads in at once, or never wakes the waiter.
#[test]
fn another_thread_waits_until_the_holder_lets_go() {
    let writers = Writers::default();
    let held = writers.lock();
    let (took, taken) = mpsc::channel();
    thread::scope(|s| {
        s.spawn(|| {
            let _writing = writers.lock();
            let _ = took.send(());
        });
        // A progress event, not a time: the other thread is counted as waiting.
        while writers.waiting() == 0 {
            thread::yield_now();
        }
        assert!(taken.try_recv().is_err(), "it waits while the lock is held");
        drop(held);
        assert_eq!(
            taken.recv().ok(),
            Some(()),
            "it takes the lock once it is free"
        );
    });
    assert_eq!(writers.waiting(), 0);
}
