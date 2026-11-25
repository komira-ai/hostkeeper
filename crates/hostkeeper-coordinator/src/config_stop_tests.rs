//! The stop's order and its failures, over a rig's coordinator (no tunnels, no console session)
//! and a stand-in for the listeners: a future the test makes fail, or leaves pending. The drain
//! delay is 0, and nothing here waits on a clock: each step is a poll by hand or the end of a
//! future.

use std::convert::Infallible;
use std::future::Future;
use std::io;
use std::pin::{Pin, pin};
use std::task::{Context, Poll};
use std::time::Duration;

use futures::task::noop_waker_ref;

use super::stopped;
use crate::config::StartError;
use crate::graceful::Graceful;
use crate::health::Serving;
use crate::testutil::Rig;

/// One poll of `future` with a waker that does nothing: what it says at this moment.
fn poll_once<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(noop_waker_ref()))
}

/// Listeners that never fail.
fn serving_on() -> impl Future<Output = Result<Infallible, StartError>> {
    std::future::pending()
}

/// Proves: with no connection open, the stop drains the health listener (`/readyz` draining),
/// stops the listeners' accepting and returns `Ok`; with one connection open it is still pending
/// after the signal, the drain and the listeners' stop, and returns `Ok` when that connection
/// ends.
///
/// Catches: a stop that returns before the connections it waits for have ended, one that never
/// tells the listeners to stop, and one that forgets to drain the health listener first.
#[tokio::test(start_paused = true)]
async fn the_stop_drains_stops_accepting_and_waits_for_the_last_connection() {
    let rig = Rig::new("stop-order");
    let (health, graceful) = (Serving::default(), Graceful::default());
    let ended = stopped(
        &rig.coordinator,
        async { Ok(()) },
        Duration::ZERO,
        health.clone(),
        &graceful,
        serving_on(),
    )
    .await;
    assert!(matches!(ended, Ok(())), "{ended:?}");
    assert!(health.is_draining());
    graceful.stopped().await;

    let (health, graceful) = (Serving::default(), Graceful::default());
    let open = graceful.open();
    let mut stopping = pin!(stopped(
        &rig.coordinator,
        async { Ok(()) },
        Duration::ZERO,
        health.clone(),
        &graceful,
        serving_on(),
    ));
    // The zero delay is a timer on the paused clock: the stop gets past it once the runtime has
    // run its timers, which a yield lets it do.
    while poll_once(pin!(graceful.stopped())).is_pending() {
        assert!(poll_once(stopping.as_mut()).is_pending());
        tokio::task::yield_now().await;
    }
    assert!(health.is_draining());
    assert!(
        poll_once(stopping.as_mut()).is_pending(),
        "returned with a connection open"
    );
    drop(open);
    assert!(matches!(stopping.await, Ok(())));
}

/// Proves: a stop signal that fails is the start's error (`Io`) at once, with an hour's drain
/// delay given: the first poll answers it, the delay is not waited, and the listeners are not told
/// to stop.
///
/// Catches: a failed signal handler read as a clean stop, so the process exits 0 having never
/// been told to, and one that holds the process for the drain delay before it says so.
#[tokio::test]
async fn a_failed_stop_signal_ends_the_start_with_its_error_at_once() {
    let rig = Rig::new("stop-failed");
    let graceful = Graceful::default();
    let mut ending = pin!(stopped(
        &rig.coordinator,
        async { Err(io::Error::other("no signal handler")) },
        Duration::from_secs(3600),
        Serving::default(),
        &graceful,
        serving_on(),
    ));
    let Poll::Ready(ended) = poll_once(ending.as_mut()) else {
        panic!("the failed signal waited");
    };
    assert!(matches!(ended, Err(StartError::Io(_))), "{ended:?}");
    assert!(poll_once(pin!(graceful.stopped())).is_pending());
}

/// Proves: after a good signal with an hour's drain delay, the coordinator drains (`/readyz`
/// draining) at once while the listeners are not told to stop and the stop is pending; listeners
/// that fail during the delay end the stop with their error.
///
/// Catches: a delay that stops accepting at the signal, one that comes before the drain, and a
/// listener failure in the delay that the stop sleeps through.
#[tokio::test]
async fn the_delay_follows_the_drain_with_the_listeners_serving() {
    let rig = Rig::new("stop-delay");
    let (health, graceful) = (Serving::default(), Graceful::default());
    let told = health.clone();
    let failing = std::future::poll_fn(move |_| {
        if told.is_draining() {
            Poll::Ready(Err(StartError::Io(io::Error::other("failed in the delay"))))
        } else {
            Poll::Pending
        }
    });
    let mut stopping = pin!(stopped(
        &rig.coordinator,
        async { Ok(()) },
        Duration::from_secs(3600),
        health.clone(),
        &graceful,
        failing,
    ));
    let Poll::Ready(ended) = poll_once(stopping.as_mut()) else {
        panic!("a listener failure in the delay was slept through");
    };
    assert!(health.is_draining());
    assert!(poll_once(pin!(graceful.stopped())).is_pending());
    assert!(
        matches!(&ended, Err(StartError::Io(why)) if why.to_string() == "failed in the delay"),
        "{ended:?}"
    );

    let (health, graceful) = (Serving::default(), Graceful::default());
    let mut waiting = pin!(stopped(
        &rig.coordinator,
        async { Ok(()) },
        Duration::from_secs(3600),
        health.clone(),
        &graceful,
        serving_on(),
    ));
    assert!(poll_once(waiting.as_mut()).is_pending());
    assert!(health.is_draining(), "drained at the signal");
    assert!(
        poll_once(pin!(graceful.stopped())).is_pending(),
        "the listeners accept through the delay"
    );
}

/// Proves: listeners that fail before the signal end the start with their error, and the health
/// listener is not drained; listeners that fail while the stop waits for a connection end it with
/// their error too, rather than the wait going on for ever.
///
/// Catches: a listener's failure swallowed by the stop, so the process keeps running with no
/// listener, or exits 0.
#[tokio::test]
async fn listeners_that_fail_before_or_during_the_stop_end_it_with_their_error() {
    let rig = Rig::new("stop-serving-failed");
    let (health, graceful) = (Serving::default(), Graceful::default());
    let ended = stopped(
        &rig.coordinator,
        std::future::pending(),
        Duration::ZERO,
        health.clone(),
        &graceful,
        async { Err(StartError::Io(io::Error::other("bound twice"))) },
    )
    .await;
    assert!(
        matches!(&ended, Err(StartError::Io(why)) if why.to_string() == "bound twice"),
        "{ended:?}"
    );
    assert!(!health.is_draining());

    let graceful = Graceful::default();
    let _open = graceful.open();
    let told = graceful.clone();
    let failing = async move {
        told.stopped().await;
        Err(StartError::Io(io::Error::other("failed while stopping")))
    };
    let ended = stopped(
        &rig.coordinator,
        async { Ok(()) },
        Duration::ZERO,
        Serving::default(),
        &graceful,
        failing,
    )
    .await;
    assert!(
        matches!(&ended, Err(StartError::Io(why)) if why.to_string() == "failed while stopping"),
        "{ended:?}"
    );
}
