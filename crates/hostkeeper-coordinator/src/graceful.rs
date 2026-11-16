//! The stop of the listeners' connections.
//!
//! A [`Graceful`] is shared by the heartbeat, control and LAN listeners (the health listener has
//! one of its own that is never stopped, since it answers until the process exits). Each
//! connection the listeners accept is counted from the accept until it ends ([`Graceful::open`]),
//! and each is told, once [`Graceful::wind_down`] runs, to finish: an HTTP/2 connection to go
//! away (no new stream; the calls in flight finish) and an HTTP/1.1 connection to close after the
//! request in flight ([`crate::listen`]). The listeners stop accepting at the same moment
//! ([`accept_until_stopped`]), and [`Graceful::wind_down`] returns once the count is zero.
//!
//! It adds no bound of its own: a connection ends by the limits it has already (the patience for
//! a handshake, a head, a body and each write, and the time an HTTP/2 connection is kept
//! ([`crate::listen`])), so the wait is as long as the longest of those.

use std::convert::Infallible;
use std::pin::pin;
use std::sync::Arc;

use futures::future::{Either, select};
use tokio::sync::watch;

/// What the listeners of one coordinator share to stop together: whether they were told to stop
/// and how many connections are open. A clone is another handle on the same state.
#[derive(Clone, Debug)]
pub(crate) struct Graceful {
    /// Whether the stop was called: it only moves from `false` to `true`.
    stopping: Arc<watch::Sender<bool>>,
    /// The connections open: counted by [`Graceful::open`], until the [`Open`] is dropped.
    open: Arc<watch::Sender<usize>>,
}

impl Default for Graceful {
    /// A handle that nobody has told to stop.
    fn default() -> Self {
        Self {
            stopping: Arc::new(watch::channel(false).0),
            open: Arc::new(watch::channel(0).0),
        }
    }
}

/// One connection counted as open: the count goes down when this is dropped.
#[derive(Debug)]
pub(crate) struct Open(Arc<watch::Sender<usize>>);

impl Drop for Open {
    fn drop(&mut self) {
        self.0.send_modify(|n| *n -= 1);
    }
}

impl Graceful {
    /// Counts one more open connection, until the returned guard is dropped. The count goes up
    /// before the connection is spawned, so the stop never misses a connection that was
    /// accepted.
    pub(crate) fn open(&self) -> Open {
        self.open.send_modify(|n| *n += 1);
        Open(Arc::clone(&self.open))
    }

    /// How many connections are open now.
    pub(crate) fn count(&self) -> usize {
        *self.open.borrow()
    }

    /// Resolves once [`Graceful::wind_down`] was called, at once when it was before; never when
    /// nobody calls it.
    pub(crate) async fn stopped(&self) {
        drop(
            self.stopping
                .subscribe()
                .wait_for(|stopping| *stopping)
                .await,
        );
    }

    /// Resolves once no connection is open.
    async fn closed(&self) {
        drop(self.open.subscribe().wait_for(|n| *n == 0).await);
    }

    /// Tells every listener to stop accepting and every open connection to finish, and
    /// returns once none is open. It logs one line when it starts waiting, with the number of
    /// connections it waits for, and one when it is done.
    pub(crate) async fn wind_down(&self) {
        self.stopping.send_replace(true);
        let waiting = self.count();
        tracing::info!(
            open = waiting,
            "stopping: waiting for the open connections to finish"
        );
        self.closed().await;
        tracing::info!(waited_for = waiting, "stopped: every connection has ended");
    }
}

/// Polls `accepting` (the accept loops of listeners, which never finish) until `graceful` is
/// told to stop, then drops it, which closes the listeners' sockets and leaves each connection
/// to its own task, and never returns: the caller's other futures go on being polled beside it.
pub(crate) async fn accept_until_stopped(
    graceful: &Graceful,
    accepting: impl Future<Output = Infallible>,
) -> Infallible {
    match select(pin!(accepting), pin!(graceful.stopped())).await {
        Either::Left((never, _)) => match never {},
        Either::Right(((), _)) => {}
    }
    std::future::pending().await
}

#[cfg(test)]
#[path = "graceful_tests.rs"]
mod tests;
