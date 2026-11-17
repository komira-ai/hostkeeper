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
/// for every waiter once [`Graceful::wind_down`] runs (on a clone too), and resolves at once for
/// a waiter that comes after.
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

    graceful.wind_down().await;
    assert!(poll_once(waiting.as_mut()).is_ready());
    graceful.clone().stopped().await;
}

/// Proves: [`Graceful::wind_down`] returns at once when no connection is open, and logs two
/// lines that say it waited for none: one when it starts waiting and one when it is done.
///
/// Catches: a wait for a connection that does not exist, and a stop that leaves no line in the
/// journal for an operator to find.
#[tokio::test]
async fn winding_down_with_no_connection_open_returns_at_once_and_says_so() {
    let log = Log::default();
    let _guard = log.install();
    Graceful::default().wind_down().await;
    let text = log.text();
    assert!(
        text.contains("stopping: waiting for the open connections to finish")
            && text.contains("open=0")
            && text.contains("stopped: every connection has ended")
            && text.contains("waited_for=0"),
        "{text}"
    );
}

/// Proves: [`Graceful::wind_down`] with two connections open does not return while one is still
/// open, returns when the last is dropped (the guards dropped on another task), and logs the
/// number it waited for, 2, in the line that starts the wait and in the one that ends it, the
/// second line only after the last connection ended.
///
/// Catches: a stop that returns at the first connection to end, or on the stop signal alone,
/// and a count in the journal that is read after the connections are gone.
#[tokio::test]
async fn winding_down_waits_for_the_last_open_connection_and_logs_how_many() {
    let log = Log::default();
    let _guard = log.install();
    let graceful = Graceful::default();
    let first = graceful.open();
    let second = graceful.open();
    let mut winding = pin!(graceful.wind_down());
    assert!(poll_once(winding.as_mut()).is_pending());
    assert!(
        log.text().contains("open=2") && !log.text().contains("stopped: every"),
        "{}",
        log.text()
    );

    drop(first);
    assert!(poll_once(winding.as_mut()).is_pending());
    tokio::spawn(async move { drop(second) })
        .await
        .expect("joined");
    winding.await;
    let text = log.text();
    assert!(
        text.contains("waited_for=2") && !text.contains("waited_for=1"),
        "{text}"
    );
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

    graceful.wind_down().await;
    assert!(poll_once(guarded.as_mut()).is_pending());
    assert!(dropped.load(Ordering::SeqCst), "the accepting is dropped");
}
