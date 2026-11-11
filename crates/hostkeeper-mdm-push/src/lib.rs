//! The MDM push side of the coordinator (`docs/design/coordinator-mdm.md`, sections 4.3 and 4.4,
//! task CM7).
//!
//! - [`apns`]: the APNs sender, an HTTP/2 client of `api.push.apple.com` behind the
//!   [`apns::PushSender`] port, and how every answer of APNs maps to a [`apns::PushError`].
//! - [`policy`]: when a Mac is pushed, a state machine over an injected [`clock::Clock`], with the
//!   rules the model checks of `hostkeeper-mdm-model` found; [`pusher`] sends what it allows.
//! - [`token`]: the 32 raw bytes of a device token and their hex form in the URL.
//! - [`identity`]: a push certificate and its key, parsed and checked against each other.
//! - [`request`]: the key and CSR of a push certificate request and the encryption certificate
//!   mdmcert.download encrypts its reply to.
//! - [`mdmcert`]: the request to mdmcert.download, behind the [`mdmcert::MdmcertTransport`] port,
//!   and the decryption of its emailed reply ([`p7m`]).
//! - [`tls`]: the TLS client both senders use.
//!
//! The crate keeps no secret and writes no audit: its callers do. With the `testkit` feature it
//! also holds the fakes of the ports ([`testkit`]).

pub mod apns;
pub mod ber;
pub mod clock;
pub mod identity;
pub mod mdmcert;
pub mod p7m;
pub mod policy;
pub mod pusher;
pub mod request;
pub mod tls;
pub mod token;

#[cfg(any(test, feature = "testkit"))]
pub mod testkit;

#[cfg(test)]
mod tlsfixture;

use std::future::Future;
use std::pin::Pin;

/// The future a port's method returns, boxed so that a port is an object the callers hold.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Key material in memory: its `Debug` prints that there is some and nothing of it.
#[derive(Clone, PartialEq, Eq)]
pub struct KeyBytes(Vec<u8>);

impl KeyBytes {
    /// `bytes` as key material.
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// The bytes, for the code that uses the key and for nothing that prints.
    #[must_use]
    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Debug for KeyBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("KeyBytes(<redacted>)")
    }
}
