//! Steps 1 and 2 of fetching a package (docs/design/hostkeeper-design.md, section 5.5): download
//! the archive from the release channel and prove that it is ours.
//!
//! - [`fetch::fetch_package`] downloads `<release_channel>/<service>/<service>-<version>.tar.zst`
//!   into a root-only staging file under `packages/<service>/`, counting and hashing the bytes as
//!   they arrive, within `max_package_bytes` and `fetch_timeout_secs`, and refuses bytes that do
//!   not hash to the order's digest. It returns a [`fetch::StagedArchive`].
//! - [`prove::prove`] downloads the attestation bundle beside it, reads the commit and the ref it
//!   names, and asks the [`ports::Verifier`] whether the daemon config's trusted signer attested
//!   the archive's digest, within `attestation_timeout_secs`. It returns a
//!   [`prove::VerifiedArchive`], the only way out of this crate to the archive's bytes.
//!
//! The network and the verifier are ports ([`ports::Fetcher`], [`ports::Verifier`]); [`net`] and
//! [`gh`] are their real adapters. Every deadline is a point on the injected
//! [`hostkeeper_steps::Clock`]. A refusal is a [`PackageFailure`]: an outcome, a class and one of
//! the reason tokens in [`REASONS`].
//!
//! The release channel is not trusted: its bytes are only counted, hashed and written to the
//! staging file, and the bundle is read only for the commit and the ref the verifier is asked to
//! confirm. Nothing here opens, decompresses or unpacks the archive.

pub mod fetch;
pub mod gh;
pub mod net;
pub mod ports;
pub mod prove;

mod bundle;
mod der;
mod failure;
mod stage;

#[cfg(test)]
mod testing;

#[cfg(test)]
mod crate_tests;
#[cfg(test)]
mod fixtures_tests;

pub use failure::{
    ATTESTATION_UNAVAILABLE, DIGEST_MISMATCH, FETCH_FAILED, NO_SPACE, NOT_ATTESTED, PackageFailure,
    REASONS,
};
pub use fetch::{PackageOrder, StagedArchive, fetch_package, package_url, remove_orphan_fetches};
pub use prove::{VerifiedArchive, prove};
