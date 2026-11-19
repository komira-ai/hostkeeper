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
#[tokio::test]
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
    // The zero delay is a timer: the stop gets past it once the runtime has run it.
    while !health.is_draining()
        || graceful.count() == 1 && poll_once(pin!(graceful.stopped())).is_pending()
    {
        assert!(poll_once(stopping.as_mut()).is_pending());
        tokio::task::yield_now().await;
    }
    assert!(
        poll_once(stopping.as_mut()).is_pending(),
        "returned with a connection open"
    );
    drop(open);
    assert!(matches!(stopping.await, Ok(())));
}

/// Proves: a stop signal that fails is the start's error (`Io`), and the listeners are not told to
/// stop.
///
/// Catches: a failed signal handler read as a clean stop, so the process exits 0 having never
/// been told to.
#[tokio::test]
async fn a_failed_stop_signal_ends_the_start_with_its_error() {
    let rig = Rig::new("stop-failed");
    let graceful = Graceful::default();
    let ended = stopped(
        &rig.coordinator,
        async { Err(io::Error::other("no signal handler")) },
        Duration::ZERO,
        Serving::default(),
        &graceful,
        serving_on(),
    )
    .await;
    assert!(matches!(ended, Err(StartError::Io(_))), "{ended:?}");
    assert!(poll_once(pin!(graceful.stopped())).is_pending());
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
