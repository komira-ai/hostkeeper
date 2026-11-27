//! The lookup's fake ([`FakeLookup`]), beside the other fakes of [`crate::testing`].

use std::io;
use std::sync::Mutex;

use crate::dial::{IpAddr, SocketAddr};
use crate::reach::Lookup;

/// The words of the error of a lookup the fake fails.
pub const LOOKUP_FAILED: &str = "the resolver did not answer";

/// A lookup that answers what the test last set: addresses, or a failure of a kind with the
/// words [`LOOKUP_FAILED`]. It keeps the names and ports it was asked.
pub struct FakeLookup {
    answer: Mutex<Result<Vec<IpAddr>, io::ErrorKind>>,
    asked: Mutex<Vec<(String, u16)>>,
}

impl FakeLookup {
    /// A lookup that answers `addresses`.
    pub fn answering(addresses: &[IpAddr]) -> FakeLookup {
        FakeLookup {
            answer: Mutex::new(Ok(addresses.to_vec())),
            asked: Mutex::new(Vec::new()),
        }
    }

    /// A lookup that fails with `kind`.
    pub fn failing(kind: io::ErrorKind) -> FakeLookup {
        let lookup = FakeLookup::answering(&[]);
        lookup.fail(kind);
        lookup
    }

    /// From now on, answers `addresses`.
    pub fn answer(&self, addresses: &[IpAddr]) {
        *super::lock(&self.answer) = Ok(addresses.to_vec());
    }

    /// From now on, fails with `kind`.
    pub fn fail(&self, kind: io::ErrorKind) {
        *super::lock(&self.answer) = Err(kind);
    }

    /// The names and ports asked, in order.
    pub fn asked(&self) -> Vec<(String, u16)> {
        super::lock(&self.asked).clone()
    }
}

impl Lookup for FakeLookup {
    fn lookup(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        super::lock(&self.asked).push((host.to_owned(), port));
        match &*super::lock(&self.answer) {
            Ok(addresses) => Ok(addresses
                .iter()
                .map(|ip| SocketAddr::new(*ip, port))
                .collect()),
            Err(kind) => Err(io::Error::new(*kind, LOOKUP_FAILED)),
        }
    }
}
