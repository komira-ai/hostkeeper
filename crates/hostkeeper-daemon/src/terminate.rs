//! The daemon's SIGTERM: what [`crate::start::serve_until`] waits for beside the daemon's exit for
//! its own update, so that a SIGTERM ends the daemon after its clean-shutdown marker is judged
//! ([`crate::host_health::HealthKeeper::stopping`]) rather than by the signal's default action.

use std::future::Future;
use std::pin::Pin;

use hostkeeper_link::Log;
use tokio::signal::unix::{SignalKind, signal};

/// A future that ends at the daemon's first SIGTERM.
pub type Terminated = Pin<Box<dyn Future<Output = ()> + Send>>;

/// The daemon's SIGTERM, taken from its default action from this call on: a future that ends at
/// the first one. Called on the runtime the daemon serves on. When the signal cannot be taken,
/// that is logged to `log` and the future never ends, the signal keeping its default action.
#[must_use]
pub fn sigterm(log: &dyn Log) -> Terminated {
    match signal(SignalKind::terminate()) {
        Ok(mut term) => Box::pin(async move {
            term.recv().await;
        }),
        Err(e) => {
            log.line(&format!(
                "error: SIGTERM is not taken, so the daemon stops at it with no clean-shutdown \
                 marker: {e}"
            ));
            Box::pin(std::future::pending())
        }
    }
}
