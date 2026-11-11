//! The certificate for the MDM name, issued and renewed in the coordinator (`docs/design/
//! coordinator-mdm.md`, section 4.10), with no separate stack.
//!
//! - [`https`]: the one HTTPS port every request goes through, so a test answers the ACME
//!   directory and the DNS provider with a fake.
//! - [`dns`]: the DNS provider port, the credential (its kind is stored with it) and the name of
//!   the challenge record in the zone setting; [`godaddy`] is the first provider.
//! - [`order`]: one ACME order for the name by DNS-01, the record deleted after the order whether
//!   it succeeded or failed.
//! - [`renewal`]: when the certificate is due, the backoff after a failure, the single flight, the
//!   timer, and the status the alerts read ([`status`]).
//!
//! [`Clock`] is the one clock the renewal reads and sleeps by, injected so that no test waits
//! (T10).

use std::time::Duration;

use hostkeeper_enrollment::validity::UnixClock;
use hostkeeper_mdm_push::BoxFuture;

pub(crate) mod alerts;
pub(crate) mod dns;
pub(crate) mod godaddy;
pub(crate) mod https;
pub(crate) mod order;
pub(crate) mod renewal;
pub(crate) mod status;

#[cfg(test)]
pub(crate) mod testkit;

/// The time the renewal reads, and the pause between its polls.
pub trait Clock: Send + Sync {
    /// Now, in unix seconds.
    fn now_unix(&self) -> u64;
    /// Returns after `pause`.
    fn sleep(&self, pause: Duration) -> BoxFuture<'_, ()>;
}

/// The wall clock and the runtime's timer.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix(&self) -> u64 {
        hostkeeper_enrollment::validity::SystemClock.now_unix()
    }

    fn sleep(&self, pause: Duration) -> BoxFuture<'_, ()> {
        Box::pin(tokio::time::sleep(pause))
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
