//! How a fetch is refused: an outcome, a class and a reason token. A token that no code of this
//! crate reports any more (the release index and the attestation proof are gone from the fetch)
//! stays: tokens are only ever added, so a record of an earlier release still names one.

use std::fmt;

use hostkeeper_hook::{Class, Outcome};

/// Not reported any more. The release index is not the index of the version asked for: not JSON,
/// another schema, service or version, a field too many or too few, a file or digest that is not
/// one, a platform listed twice or out of order, or no package. Also an index over 64 KiB.
pub const INDEX_INVALID: &str = "index_invalid";
/// Not reported any more. The release index received does not hash to the SHA-256 the order
/// names for it. Checked before the index is parsed.
pub const INDEX_DIGEST_MISMATCH: &str = "index_digest_mismatch";
/// Not reported any more. The release index lists no package for this host's platform.
pub const NO_PACKAGE_FOR_PLATFORM: &str = "no_package_for_platform";
/// The package's size or SHA-256 is not what the entry says, or its size is over
/// `max_package_bytes`.
pub const PACKAGE_DIGEST_MISMATCH: &str = "package_digest_mismatch";
/// The archive could not be downloaded: no connection, a status other than 2xx, a body cut
/// short, the deadline, or a staging file that could not be written for a reason other than a
/// full disk.
pub const FETCH_FAILED: &str = "fetch_failed";
/// Not reported any more. The attestation bundle could not be downloaded, or the verifier could
/// not run or ran out of time.
pub const ATTESTATION_UNAVAILABLE: &str = "attestation_unavailable";
/// The disk was full while a staging file was written.
pub const NO_SPACE: &str = "no_space";
/// Not reported any more. The bundle is not an attestation of this archive by the trusted signer,
/// or the verifier refused it.
pub const NOT_ATTESTED: &str = "not_attested";

/// No store of the host can be asked for the bytes of an order's entry: no mirror is defined and
/// none of the entry's locations names a store the host defines.
pub const NO_STORE: &str = "no_store";
/// Only mirrors answered, and every one of them gave bytes other than the entry's.
pub const STORE_DIGEST_MISMATCH: &str = "store_digest_mismatch";

/// Every reason token this crate has reported. The crate README lists the same set; a token is
/// only ever added (`outcome_tokens_are_the_documented_set`).
pub const REASONS: [&str; 10] = [
    INDEX_INVALID,
    INDEX_DIGEST_MISMATCH,
    NO_PACKAGE_FOR_PLATFORM,
    PACKAGE_DIGEST_MISMATCH,
    FETCH_FAILED,
    ATTESTATION_UNAVAILABLE,
    NO_SPACE,
    NOT_ATTESTED,
    NO_STORE,
    STORE_DIGEST_MISMATCH,
];

/// Why a package was not fetched.
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

    /// `Outcome::Failure`, `Class::Environment`: this host cannot fetch the package.
    pub(crate) const fn environment(reason: &'static str) -> PackageFailure {
        PackageFailure {
            outcome: Outcome::Failure,
            class: Class::Environment,
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
