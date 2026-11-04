//! How a fetch or a proof is refused: an outcome, a class and a reason token.

use std::fmt;

use hostkeeper_hook::{Class, Outcome};

/// The package is larger than `max_package_bytes`, or its bytes do not hash to the order's
/// digest.
pub const DIGEST_MISMATCH: &str = "digest_mismatch";
/// The archive could not be downloaded: no connection, a status other than 2xx, a body cut
/// short, the deadline, or a staging file that could not be written for a reason other than a
/// full disk.
pub const FETCH_FAILED: &str = "fetch_failed";
/// The attestation bundle could not be downloaded, or the verifier could not run or ran out of
/// time.
pub const ATTESTATION_UNAVAILABLE: &str = "attestation_unavailable";
/// The disk was full while a staging file was written.
pub const NO_SPACE: &str = "no_space";
/// The bundle is not an attestation of this archive by the trusted signer, or the verifier
/// refused it.
pub const NOT_ATTESTED: &str = "not_attested";

/// Every reason token this crate reports. The crate README lists the same set; a token is only
/// ever added (`outcome_tokens_are_the_documented_set`).
pub const REASONS: [&str; 5] = [
    DIGEST_MISMATCH,
    FETCH_FAILED,
    ATTESTATION_UNAVAILABLE,
    NO_SPACE,
    NOT_ATTESTED,
];

/// Why a package was not fetched or not proven.
///
/// `Outcome::Deferred` with `Class::Environment` means nothing changed on the host and the order
/// may be tried again; `Outcome::Failure` with `Class::Release` means this package is bad.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackageFailure {
    pub outcome: Outcome,
    pub class: Class,
    /// One of [`REASONS`].
    pub reason: &'static str,
}

impl PackageFailure {
    /// `Outcome::Failure`, `Class::Release`: the package is bad.
    pub(crate) const fn release(reason: &'static str) -> PackageFailure {
        PackageFailure {
            outcome: Outcome::Failure,
            class: Class::Release,
            reason,
        }
    }

    /// `Outcome::Deferred`, `Class::Environment`: try again later.
    pub(crate) const fn deferred(reason: &'static str) -> PackageFailure {
        PackageFailure {
            outcome: Outcome::Deferred,
            class: Class::Environment,
            reason,
        }
    }
}

impl fmt::Display for PackageFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?}, class {}, reason {}",
            self.outcome,
            self.class.as_str(),
            self.reason
        )
    }
}

impl std::error::Error for PackageFailure {}
