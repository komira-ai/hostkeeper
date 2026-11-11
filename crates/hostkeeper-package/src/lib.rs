//! Step 1 of fetching a package (docs/design/hostkeeper-design.md, section 5.5), for an entry that
//! is pinned by its digest, and what the host's stores and the attestation checks need.
//!
//! The daemon uses [`stores`] (a Mac's start renders `stores.toml` from managed preferences),
//! [`entry::fetch_pinned`] (a `macos-pkg` entry's artifact) and, at its start,
//! [`remove_orphan_fetches`], for the staging files a fetch left. `hostkeeper-pxe` uses [`bundle`],
//! [`net`], [`gh`] and [`ports`], and `hostkeeper-mdm-gate` uses [`stores`].
//!
//! - [`entry::fetch_pinned`] downloads the artifact an order's entry (a
//!   `hostkeeper.release.v1.Target`) names, from the host's [`stores::Stores`] (the mirrors first,
//!   then the entry's locations, each source with its own deadline, credential and redirects),
//!   into a root-only staging file under `packages/<service>/`, counting and hashing the bytes as
//!   they arrive, within `max_package_bytes` and `fetch_timeout_secs`; the first bytes with the
//!   entry's size and SHA-256 win. It returns an [`entry::Pinned`], the staging file with the
//!   entry's size and SHA-256, and the stores that gave other bytes. No attestation is read: the
//!   `macos-pkg` installer checks the package's signature itself.
//! - [`bundle::read`] reads an attestation bundle for the commit, the ref and the digests the
//!   verifier is asked to confirm; `pxe update-hook prepare` uses it with the same verifier.
//! - [`stores::load`] reads `/etc/hostkeeper/stores.toml`; a host without the file has no store.
//!
//! The network and the verifier are ports ([`ports::Fetcher`], [`ports::Verifier`]); [`net`] and
//! [`gh`] are their real adapters ([`net::NetFetcher`] also follows a store's redirects, sends
//! its credential to its own origin only and answers an OCI token challenge). Every deadline is a
//! point on the injected [`hostkeeper_steps::Clock`]. A refusal is a [`PackageFailure`]: an
//! outcome, a class and one of the reason tokens in [`REASONS`].
//!
//! The stores are not trusted: their bytes are only counted, hashed and written to the staging
//! file. Nothing here opens, decompresses or unpacks the archive.

pub mod bundle;
pub mod entry;
pub mod gh;
pub mod net;
pub mod ports;
pub mod stores;

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
    ATTESTATION_UNAVAILABLE, FETCH_FAILED, INDEX_DIGEST_MISMATCH, INDEX_INVALID,
    NO_PACKAGE_FOR_PLATFORM, NO_SPACE, NO_STORE, NOT_ATTESTED, PACKAGE_DIGEST_MISMATCH,
    PackageFailure, REASONS, STORE_DIGEST_MISMATCH,
};
pub use stage::remove_orphan_fetches;
