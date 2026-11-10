//! Steps 1 and 2 of fetching a package (docs/design/hostkeeper-design.md, section 5.5): download
//! the archive from the release channel and prove that it is ours.
//!
//! The daemon fetches no package through `fetch`, `prove` or `entry` any more: it stages a
//! package its installer names from the host's package manager (`hostkeeper-install`), and uses
//! this crate for `stores` alone (a Mac's start renders `stores.toml` from managed preferences).
//! `hostkeeper-pxe` uses `bundle`, `net`, `gh` and `ports`, `hostkeeper-mdm-gate` uses
//! `stores`, and `xtask`'s release channel staging tests use `fetch`, `index` and `prove`;
//! nothing outside this crate uses `entry`.
//!
//! - [`fetch::fetch_package`] reads the release index
//!   `<release_channel>/<service>/<version>/index.json` ([`index`]), takes the entry for the host's [`index::Platform`], and downloads that package
//!   into a root-only staging file under `packages/<service>/`, counting and hashing the bytes as
//!   they arrive, within `max_package_bytes` and `fetch_timeout_secs`, and refuses bytes whose size
//!   or SHA-256 are not the entry's. It returns a [`fetch::StagedArchive`].
//! - [`prove::prove`] downloads the attestation bundle beside it, reads the commit and the ref it
//!   names, and asks the [`ports::Verifier`] whether the daemon config's trusted signer attested
//!   the archive's digest, within `attestation_timeout_secs`. It returns a
//!   [`prove::VerifiedArchive`], the only way out of this crate to the archive's bytes.
//!
//! - [`bundle::read`] reads an attestation bundle for the commit, the ref and the digests the
//!   verifier is asked to confirm; `hostkeeper-pxe fetch` uses it with the same verifier.
//!
//! - [`entry::fetch_entry`] does both through an order's entry (a `hostkeeper.release.v1.Target`)
//!   instead of the release index: the artifact, then its attestation bundle, each from the
//!   host's [`stores::Stores`] (the mirrors first, then the entry's locations, each source with
//!   its own deadline, credential and redirects), the first bytes with the expected size and
//!   SHA-256 winning, then the same verifier. It returns a [`entry::Proven`]: the same
//!   [`prove::VerifiedArchive`] and the stores that gave other bytes.
//! - [`stores::load`] reads `/etc/hostkeeper/stores.toml`; a host without the file has no store.
//!
//! The network and the verifier are ports ([`ports::Fetcher`], [`ports::Verifier`]); [`net`] and
//! [`gh`] are their real adapters ([`net::NetFetcher`] also follows a store's redirects, sends
//! its credential to its own origin only and answers an OCI token challenge). Every deadline is a
//! point on the injected [`hostkeeper_steps::Clock`]. A refusal is a [`PackageFailure`]: an outcome, a class and one of
//! the reason tokens in [`REASONS`].
//!
//! The release channel is not trusted: its bytes are only counted, hashed and written to the
//! staging file, and the bundle is read only for the commit and the ref the verifier is asked to
//! confirm. Nothing here opens, decompresses or unpacks the archive.

pub mod bundle;
pub mod entry;
pub mod fetch;
pub mod gh;
pub mod index;
pub mod net;
pub mod ports;
pub mod prove;
pub mod stores;

mod der;
mod failure;
mod stage;
mod unique;

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
pub use fetch::{PackageOrder, StagedArchive, fetch_package, remove_orphan_fetches};
pub use index::{IndexEntry, IndexInvalid, Platform, ReleaseIndex, index_url, package_url};
pub use prove::{VerifiedArchive, prove};
