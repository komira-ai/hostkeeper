//! The stop of `serve` (`docs/design/coordinator-multi-instance.md`, section 11, item 2).
//!
//! From SIGTERM or SIGINT the health listener answers `/readyz` `503 draining`, so an edge that
//! checks it stops choosing this coordinator, and for `--drain-delay-secs` every listener goes on
//! accepting and serving: an edge that has not yet seen the `503`, or not yet seen the instance
//! that replaces this one healthy, still reaches a coordinator that answers. The delay starts at
//! the signal, not at the start. Then, in this order:
//!
//! 1. the heartbeat, private and LAN listeners accept no more connections, and each open one is
//!    told to finish ([`crate::graceful`]);
//! 2. every held `Releases.WaitConverged` call is answered as at its `wait_secs`
//!    ([`crate::wait::Waits::stop`]);
//! 3. the console's streams close and clean up ([`crate::console::drained`]);
//! 4. the SSH tunnels end on their own or are closed after `--ssh-drain-s`;
//! 5. once no connection of those listeners is open, the stop returns and the process exits 0.
//!
//! The periodic work and the health listener go on until then. Nothing here checks the delay
//! against the edge's values: the deploy does, once.

use std::convert::Infallible;
use std::io;
use std::pin::pin;
use std::time::Duration;

use futures::future::{Either, select};

use super::StartError;
use crate::graceful::Graceful;
use crate::health::{self, Serving};
use crate::service::Coordinator;

/// Serves (`serving`: the coordinator's listeners and periodic work, which end only by failing)
/// until `stop` resolves, then stops `coordinator` as the module says: `health` drains, `delay`
/// passes, then `graceful` stops and the stop waits for its connections to end.
///
/// # Errors
/// [`StartError`]: `stop` failed, or `serving` failed before the stop ended.
pub(super) async fn stopped<S>(
    coordinator: &Coordinator,
    stop: impl Future<Output = io::Result<()>>,
    delay: Duration,
    health: Serving,
    graceful: &Graceful,
    serving: S,
) -> Result<(), StartError>
where
    S: Future<Output = Result<Infallible, StartError>>,
{
    let mut serving = pin!(serving);
    match select(pin!(health::drain_on(stop, health)), serving.as_mut()).await {
        Either::Left((stopped, _)) => stopped?,
        Either::Right((served, _)) => {
            let Err(why) = served;
            return Err(why);
        }
    }
    // Made after the signal, so its deadline counts from the signal; the listeners serve on.
    match select(pin!(tokio::time::sleep(delay)), serving.as_mut()).await {
        Either::Left(((), _)) => {}
        Either::Right((served, _)) => {
            let Err(why) = served;
            return Err(why);
        }
    }
    graceful.stop();
    coordinator.waits.stop();
    crate::console::drained(coordinator, serving.as_mut()).await?;
    super::ssh::drained(coordinator, serving.as_mut()).await?;
    match select(pin!(graceful.closed()), serving).await {
        Either::Left(((), _)) => Ok(()),
        Either::Right((served, _)) => {
            let Err(why) = served;
            Err(why)
        }
    }
}

#[cfg(test)]
#[path = "config_stop_tests.rs"]
mod tests;
