//! The fetcher of the `coordinator` store (`hostkeeper_package::stores::COORDINATOR_STORE`): the
//! package layer's [`Fetcher`] over the coordinator's agent listener, which the host reaches by
//! its mutual TLS ([`Fetch`], [`hostkeeper_link::CoordinatorGet`]: the host's current
//! certificate, the CA the config pins).
//!
//! - **What it asks.** The URL it is given is the store's blob URL, a path of the coordinator
//!   (`/` and the entry's locator); the path is sent as it is, and no credential, redirect or
//!   challenge of a [`Request`] is used: the coordinator is told who the host is by its
//!   certificate.
//! - **What it takes.** At most `max_bytes` of body (the daemon passes `max_package_bytes`), in
//!   the time left to the transfer's deadline on the injected clock. The sink is told no length
//!   (`None`), because the link gives the sink the body, not the head.
//! - **How it fails.** A 404 and any other status is [`NetError::Status`]; a body longer than
//!   the bound, a connection that failed or was refused, a host with no certificate are
//!   [`NetError::Connect`]; a short body is [`NetError::Truncated`]; the deadline is
//!   [`NetError::DeadlineExceeded`]. The package layer defers every one of them as
//!   `fetch_failed`.

use std::io;
use std::sync::Arc;

use hostkeeper_link::{GetError, Got as Answer, Limit as Bound};
use hostkeeper_package::ports::{Fetcher, Got, Limit, NetError, Sink, Url, remaining_secs};
use hostkeeper_steps::Clock;

use crate::mirror::Fetch;

/// The [`Fetcher`] of the coordinator store.
pub struct CoordinatorFetcher {
    fetch: Arc<dyn Fetch>,
    clock: Arc<dyn Clock>,
    max_bytes: u64,
}

impl CoordinatorFetcher {
    /// The coordinator read through `fetch`, a body at most `max_bytes` long, the deadline of
    /// each transfer read on `clock`.
    pub fn new(fetch: Arc<dyn Fetch>, clock: Arc<dyn Clock>, max_bytes: u64) -> CoordinatorFetcher {
        CoordinatorFetcher {
            fetch,
            clock,
            max_bytes,
        }
    }
}

/// Why the sink ended the transfer: the error the link hands back to say so.
const STOPPED: io::ErrorKind = io::ErrorKind::Interrupted;

impl Fetcher for CoordinatorFetcher {
    fn get(&self, url: &Url, sink: &mut dyn Sink, limit: Limit) -> Result<Got, NetError> {
        let bound = Bound {
            max_bytes: self.max_bytes,
            deadline_secs: remaining_secs(&limit.deadline, &*self.clock),
        };
        let mut begun = false;
        let mut give = |bytes: &[u8]| {
            if !begun {
                begun = true;
                sink.begin(None).map_err(|_| io::Error::from(STOPPED))?;
            }
            sink.chunk(bytes).map_err(|_| io::Error::from(STOPPED))
        };
        let answer = self.fetch.get(url.as_str(), bound, &mut give);
        match answer {
            Ok(Answer::Body(_)) if begun => Ok(Got::Complete),
            Ok(Answer::Body(_)) => sink
                .begin(None)
                .map(|()| Got::Complete)
                .or(Ok(Got::Stopped)),
            Ok(Answer::NotFound) => Err(NetError::Status(404)),
            Err(GetError::Sink(STOPPED)) => Ok(Got::Stopped),
            Err(GetError::Status(code)) => Err(NetError::Status(code)),
            Err(GetError::Truncated) => Err(NetError::Truncated),
            Err(GetError::Deadline) => Err(NetError::DeadlineExceeded),
            Err(GetError::Sink(_)) => Err(NetError::Io),
            Err(
                GetError::Path(_)
                | GetError::NotEnrolled
                | GetError::Link(_)
                | GetError::TooLong { .. },
            ) => Err(NetError::Connect),
        }
    }
}

#[cfg(test)]
#[path = "coordinator_store_tests.rs"]
mod tests;
