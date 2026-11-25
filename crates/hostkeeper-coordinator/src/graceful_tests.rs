//! The count of open connections, the stop signal the listeners and their connections share, and
//! the wait for the connections to end. Nothing here waits on a clock: each step is a poll of a
//! future by hand, or the end of a task.

use std::convert::Infallible;
use std::future::Future;
use std::pin::{Pin, pin};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::{Context, Poll};

use futures::task::noop_waker_ref;

use super::{Graceful, accept_until_stopped};
use crate::testutil::Log;

/// One poll of `future` with a waker that does nothing: what it says at this moment.
fn poll_once<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(noop_waker_ref()))
}

/// Proves: the count of open connections goes up by one for each [`Graceful::open`] and down by
/// one when each guard is dropped, in any order, and is shared by the clones of a handle.
///
/// Catches: a count that is per handle, so a listener's connections are not seen by the stop, or
/// one that a dropped guard leaves behind.
#[test]
fn the_count_follows_the_connections_open_on_any_handle() {
    let graceful = Graceful::default();
    let other = graceful.clone();
    assert_eq!(graceful.count(), 0);
    let first = graceful.open();
    let second = other.open();
    assert_eq!((graceful.count(), other.count()), (2, 2));
    drop(first);
    assert_eq!(other.count(), 1);
    drop(second);
    assert_eq!(graceful.count(), 0);
}

/// Proves: [`Graceful::stopped`] does not resolve while nobody has stopped the handle, resolves
/// for every waiter once [`Graceful::stop`] runs (on a clone too), and resolves at once for a
/// waiter that comes after.
///
/// Catches: a stop that reaches only the waiters of the handle that called it, one that a late
/// connection (a handshake that ends after the stop) never hears, and a signal that fires on
/// its own.
#[tokio::test]
async fn the_stop_reaches_every_waiter_on_any_handle_even_a_late_one() {
    let graceful = Graceful::default();
    let early = graceful.clone();
    let mut waiting = pin!(early.stopped());
    assert!(poll_once(waiting.as_mut()).is_pending());

    graceful.clone().stop();
    assert!(poll_once(waiting.as_mut()).is_ready());
    graceful.stopped().await;
}

/// Proves: [`Graceful::stop`] logs one line with the number of connections open then (0 and 2),
/// and [`Graceful::closed`] resolves at once with none open and logs that every connection ended.
///
/// Catches: a stop that leaves no line in the journal for an operator to find, a count read
/// after the connections are gone, and a wait for a connection that does not exist.
#[tokio::test]
async fn the_stop_says_how_many_are_open_and_the_end_says_none_is() {
    let log = Log::default();
    let _guard = log.install();
    let graceful = Graceful::default();
    graceful.stop();
    graceful.closed().await;
    let busy = Graceful::default();
    let (_first, _second) = (busy.open(), busy.open());
    busy.stop();
    let text = log.text();
    assert!(
        text.contains(
            "stopping: the listeners accept no more connections, and the open ones finish open=0"
        ) && text.contains(
            "stopping: the listeners accept no more connections, and the open ones finish open=2"
        ) && text.matches("stopped: every connection has ended").count() == 1,
        "{text}"
    );
}

/// Proves: [`Graceful::closed`] with two connections open does not resolve while one is still
/// open, and resolves when the last is dropped (on another task); it says so only then.
///
/// Catches: a stop that returns at the first connection to end, or on the stop signal alone.
#[tokio::test]
async fn closed_waits_for_the_last_open_connection() {
    let log = Log::default();
    let _guard = log.install();
    let graceful = Graceful::default();
    let first = graceful.open();
    let second = graceful.open();
    graceful.stop();
    let mut closing = pin!(graceful.closed());
    assert!(poll_once(closing.as_mut()).is_pending());

    drop(first);
    assert!(poll_once(closing.as_mut()).is_pending());
    assert!(!log.text().contains("stopped: every"), "{}", log.text());
    tokio::spawn(async move { drop(second) })
        .await
        .expect("joined");
    closing.await;
    assert!(log.text().contains("stopped: every connection has ended"));
}

/// A future that never finishes, which says when it is dropped.
struct Listening {
    polled: Arc<AtomicUsize>,
    dropped: Arc<AtomicBool>,
}

impl Future for Listening {
    type Output = Infallible;

    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Infallible> {
        self.polled.fetch_add(1, Ordering::SeqCst);
        Poll::Pending
    }
}

impl Drop for Listening {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

/// Proves: [`accept_until_stopped`] polls the accepting it is given while the handle is not
/// stopped, drops it (a listener's socket with it) once the handle is stopped, and then stays
/// pending itself, so the futures it is joined with go on.
///
/// Catches: accepting that goes on after the stop, so a connection is accepted that nothing
/// waits for, and one that returns at the stop and takes the periodic jobs down with it.
#[tokio::test]
async fn accepting_is_dropped_at_the_stop_and_the_rest_goes_on() {
    let graceful = Graceful::default();
    let polled = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicBool::new(false));
    let accepting = Listening {
        polled: Arc::clone(&polled),
        dropped: Arc::clone(&dropped),
    };
    let mut guarded = pin!(accept_until_stopped(&graceful, accepting));
    assert!(poll_once(guarded.as_mut()).is_pending());
    assert_eq!(polled.load(Ordering::SeqCst), 1);
    assert!(!dropped.load(Ordering::SeqCst));

    graceful.stop();
    assert!(poll_once(guarded.as_mut()).is_pending());
    assert!(dropped.load(Ordering::SeqCst), "the accepting is dropped");
    assert!(poll_once(guarded.as_mut()).is_pending());
}
