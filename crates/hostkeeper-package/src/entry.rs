//! Steps 1 and 2 of section 5.5 through an order's entry (docs/design/packages.md, section 7.4):
//! fetch the entry's artifact and its attestation bundle from the host's stores, and prove the
//! artifact with the pinned verifier. [`fetch_entry`] is the whole of it, and [`Proven`] is what
//! it gives.
//!
//! For the artifact, and then for the bundle, the sources are the host's `mirror` stores in file
//! order (asked for the digest, listed in the order or not), then the blob's own locations in
//! order. A location of a store the host does not define, or whose locator breaks the locator
//! grammar, is skipped. Each source is asked with its store's credential and redirects
//! ([`crate::stores`]) and has its own deadline: `fetch_timeout_secs` for the artifact,
//! `attestation_timeout_secs` for the bundle, from the start of that source's request. The first
//! source whose bytes have the blob's size and SHA-256 wins; a source that sends more than the
//! size is stopped and counts as having given other bytes. An entry whose `bytes` is over
//! `max_package_bytes` (or whose bundle is over 1 MiB) is refused before anything is asked.
//!
//! | what happened (artifact / bundle) | outcome, class, reason |
//! |---|---|
//! | some source gave the right bytes | go on; the stores that gave other bytes first are in [`Proven::other_bytes`] |
//! | no source is defined | `Failure`, `environment`, `no_store` |
//! | every source failed to answer | `Deferred`, `environment`, `fetch_failed` / `attestation_unavailable` |
//! | every source that answered gave other bytes, and one was a location | `Failure`, `release`, `package_digest_mismatch` / `not_attested` |
//! | only mirrors answered, all with other bytes | `Deferred`, `environment`, `store_digest_mismatch` |
//! | the entry's digest or size, or the bundle's, is not one a fetch can ask for (a size of 0, over `max_package_bytes`, a bundle over 1 MiB, no bundle) | `Failure`, `release`, `package_digest_mismatch` / `not_attested` |
//! | the disk is full while a staging file is written | `Deferred`, `environment`, `no_space` |
//! | a staging file cannot be made or written for another reason (a storage fault stops at once: it is the host's, not the source's) | `Deferred`, `environment`, `fetch_failed` / `attestation_unavailable` |
//! | the bundle is not a bundle, or does not list the entry's digest | `Failure`, `release`, `not_attested` |
//! | the verifier is unavailable or out of time | `Deferred`, `environment`, `attestation_unavailable` |
//! | the verifier rejects the attestation | `Failure`, `release`, `not_attested` |
//!
//! [`fetch_pinned`] is the artifact half alone (the rows of the table that name the artifact, and
//! no bundle is asked for), for an entry with no attestation: it gives a [`Pinned`], the staging
//! file with the entry's size and SHA-256, to the `macos-pkg` installer, which checks the
//! package's signature itself.
//!
//! The staging files are the ones of the channel path: `packages/<service>/.fetch-<pid>-<n>`,
//! the bundle's ending in `.json`; orphans are removed first, and every refusal removes them all.
//! The proof is [`crate::prove`]'s: the bundle is read for its commit and ref, and the verifier
//! has `attestation_timeout_secs` from the start of the proof.

use std::path::{Path, PathBuf};

use hostkeeper_config::daemon::{Bounds, Sha256Hex, TrustedSigners};
use hostkeeper_config::names::ServiceName;
use hostkeeper_proto::hostkeeper::release::v1::{Location, Target};
use hostkeeper_release::grammar::check_locator;
use hostkeeper_steps::{Clock, Stamp};

use crate::bundle::MAX_BUNDLE_BYTES;
use crate::failure::{
    ATTESTATION_UNAVAILABLE, FETCH_FAILED, NO_SPACE, NO_STORE, NOT_ATTESTED,
    PACKAGE_DIGEST_MISMATCH, PackageFailure, STORE_DIGEST_MISMATCH,
};
use crate::fetch::{failure, remove_orphan_fetches};
use crate::index::Platform;
use crate::ports::{Fetcher, Request, Url, Verifier, after};
use crate::prove::{ProveEnv, Subject, VerifiedArchive, attest};
use crate::stage::{self, Download, Fault, Job, OpenWriter};
use crate::stores::{Kind, Store, Stores};

/// What a fetch through an entry needs from the daemon: the network, the verifier, the clock, the
/// config's bounds and trusted signer, the host's stores, where packages live
/// (`/var/lib/hostkeeper/packages` on a host), and the host's platform
/// ([`Platform::current`] on a host).
#[derive(Clone, Copy)]
pub struct EntryEnv<'a> {
    pub fetcher: &'a dyn Fetcher,
    pub verifier: &'a dyn Verifier,
    pub clock: &'a dyn Clock,
    pub bounds: &'a Bounds,
    pub stores: &'a Stores,
    pub packages_root: &'a Path,
    pub signers: &'a TrustedSigners,
    pub platform: Platform,
}

/// An entry's artifact, whose size and SHA-256 are the entry's and which the trusted signer
/// attested, and the names of the stores that gave other bytes before the right ones came.
#[derive(Debug)]
pub struct Proven {
    pub archive: VerifiedArchive,
    /// The store of each source that gave bytes other than the expected ones, for the artifact
    /// and then the bundle, in the order asked, each once. The caller logs
    /// `store_digest_mismatch` for each.
    pub other_bytes: Vec<String>,
}

/// What a blob is asked for and how its failures are reported.
struct Want<'a> {
    digest: &'a Sha256Hex,
    bytes: u64,
    locations: &'a [Location],
    timeout_secs: u64,
    /// What the staging file's name ends in.
    suffix: &'static str,
    /// The refusal when a location gave other bytes.
    wrong: PackageFailure,
    /// The refusal when no source answered, or a staging file failed other than by a full disk.
    down: PackageFailure,
}

/// Fetches and proves the artifact `entry` names, for `service`. See the module's table for every
/// refusal. The caller holds the service's lock (see [`remove_orphan_fetches`]).
pub fn fetch_entry(
    env: &EntryEnv<'_>,
    service: &ServiceName,
    entry: &Target,
) -> Result<Proven, PackageFailure> {
    fetch_entry_with(env, service, entry, stage::plain)
}

/// [`fetch_entry`], writing the staging files through `writer`.
pub(crate) fn fetch_entry_with(
    env: &EntryEnv<'_>,
    service: &ServiceName,
    entry: &Target,
    writer: OpenWriter,
) -> Result<Proven, PackageFailure> {
    let artifact_digest = artifact_digest(env, entry)?;
    let blob = entry
        .attestation
        .as_ref()
        .filter(|blob| (1..=MAX_BUNDLE_BYTES).contains(&blob.bytes))
        .ok_or(PackageFailure::release(NOT_ATTESTED))?;
    let bundle_digest =
        Sha256Hex::parse(&blob.digest).ok_or(PackageFailure::release(NOT_ATTESTED))?;

    let dir = staging_dir(env, service)?;
    let mut other_bytes = Vec::new();
    let archive = fetch_artifact(env, &dir, &artifact_digest, entry, writer, &mut other_bytes)?;
    let bundle = Want {
        digest: &bundle_digest,
        bytes: blob.bytes,
        locations: &blob.locations,
        timeout_secs: env.bounds.attestation_timeout_secs,
        // `gh` reads the extension of the bundle file it is given to choose its parser.
        suffix: ".json",
        wrong: PackageFailure::release(NOT_ATTESTED),
        down: PackageFailure::deferred(ATTESTATION_UNAVAILABLE),
    };
    let bundle = fetch_blob(env, &dir, &bundle, writer, &mut other_bytes)?;

    let proof = Stamp::now(env.clock);
    let deadline = after(&proof, env.bounds.attestation_timeout_secs);
    let subject = Subject {
        file: archive.file,
        platform: env.platform,
        sha256: artifact_digest,
        size: archive.size,
    };
    let prove = ProveEnv {
        fetcher: env.fetcher,
        verifier: env.verifier,
        clock: env.clock,
        bounds: env.bounds,
        signers: env.signers,
    };
    let archive = attest(&prove, subject, bundle.file.path(), &deadline)?;
    Ok(Proven {
        archive,
        other_bytes,
    })
}

/// The digest of `entry`'s artifact, when it and the entry's size are ones a fetch can ask for.
fn artifact_digest(env: &EntryEnv<'_>, entry: &Target) -> Result<Sha256Hex, PackageFailure> {
    Sha256Hex::parse(&entry.digest)
        .filter(|_| (1..=env.bounds.max_package_bytes).contains(&entry.bytes))
        .ok_or(PackageFailure::release(PACKAGE_DIGEST_MISMATCH))
}

/// The service's directory under the packages root, made if need be, with the orphans of earlier
/// fetches removed.
fn staging_dir(env: &EntryEnv<'_>, service: &ServiceName) -> Result<PathBuf, PackageFailure> {
    let dir = env.packages_root.join(service.as_str());
    stage::make_dir(&dir).map_err(|e| failure(Fault::Storage(e)))?;
    remove_orphan_fetches(env.packages_root, service).map_err(|e| failure(Fault::Storage(e)))?;
    Ok(dir)
}

/// Downloads `entry`'s artifact, whose SHA-256 is `digest`, into a staging file in `dir`.
fn fetch_artifact(
    env: &EntryEnv<'_>,
    dir: &Path,
    digest: &Sha256Hex,
    entry: &Target,
    writer: OpenWriter,
    other_bytes: &mut Vec<String>,
) -> Result<Download, PackageFailure> {
    let artifact = Want {
        digest,
        bytes: entry.bytes,
        locations: &entry.locations,
        timeout_secs: env.bounds.fetch_timeout_secs,
        suffix: "",
        wrong: PackageFailure::release(PACKAGE_DIGEST_MISMATCH),
        down: PackageFailure::deferred(FETCH_FAILED),
    };
    fetch_blob(env, dir, &artifact, writer, other_bytes)
}

/// An entry's artifact fetched by its pin alone: the staging file holds the bytes with the
/// entry's size and SHA-256, and nothing says who made them. It is removed when this is dropped.
#[derive(Debug)]
pub struct Pinned {
    file: stage::TempFile,
    sha256: Sha256Hex,
    size: u64,
    /// The store of each source that gave bytes other than the expected ones, in the order asked,
    /// each once. The caller logs `store_digest_mismatch` for each.
    pub other_bytes: Vec<String>,
}

impl Pinned {
    /// The staging file.
    pub fn path(&self) -> &Path {
        self.file.path()
    }

    /// The artifact's SHA-256, which is the entry's.
    pub fn sha256(&self) -> &Sha256Hex {
        &self.sha256
    }

    /// The artifact's size in bytes, which is the entry's.
    pub fn size(&self) -> u64 {
        self.size
    }
}

/// Fetches the artifact `entry` names, for `service`, by its pin alone: the artifact half of
/// [`fetch_entry`] (the same refusals, sources and staging files), with no bundle asked for and
/// no attestation read. The caller holds the service's lock (see [`remove_orphan_fetches`]).
pub fn fetch_pinned(
    env: &EntryEnv<'_>,
    service: &ServiceName,
    entry: &Target,
) -> Result<Pinned, PackageFailure> {
    let digest = artifact_digest(env, entry)?;
    let dir = staging_dir(env, service)?;
    let mut other_bytes = Vec::new();
    let got = fetch_artifact(env, &dir, &digest, entry, stage::plain, &mut other_bytes)?;
    Ok(Pinned {
        file: got.file,
        sha256: digest,
        size: got.size,
        other_bytes,
    })
}

/// One place to ask: a store and the URL of the blob in it.
struct Source<'a> {
    store: &'a Store,
    url: Url,
    /// Whether the entry named this store (a mirror is asked whether it is named or not).
    named: bool,
}

/// The sources of a blob whose SHA-256 is `digest`, in the order they are asked.
fn sources<'a>(stores: &'a Stores, digest: &Sha256Hex, locations: &[Location]) -> Vec<Source<'a>> {
    let mirrors = stores.mirrors().filter_map(|store| {
        let url = store.mirror_url(digest.as_str())?;
        Some(Source {
            store,
            url,
            named: false,
        })
    });
    let named = locations.iter().filter_map(|location| {
        let store = stores.get(&location.store)?;
        check_locator(&location.locator).ok()?;
        let url = store.blob_url(&location.locator, digest.as_str());
        Some(Source {
            store,
            url,
            named: true,
        })
    });
    mirrors.chain(named).collect()
}

/// Downloads the blob `want` into a staging file in `dir` from the first source that gives its
/// size and SHA-256, and notes in `other_bytes` the store of each source that gave other bytes.
fn fetch_blob(
    env: &EntryEnv<'_>,
    dir: &Path,
    want: &Want<'_>,
    writer: OpenWriter,
    other_bytes: &mut Vec<String>,
) -> Result<Download, PackageFailure> {
    let sources = sources(env.stores, want.digest, want.locations);
    if sources.is_empty() {
        return Err(PackageFailure::environment(NO_STORE));
    }
    let mut named_wrong = false;
    let mut any_wrong = false;
    for source in &sources {
        let start = Stamp::now(env.clock);
        let job = Job {
            fetcher: env.fetcher,
            clock: env.clock,
            dir,
            request: Request {
                url: &source.url,
                authorization: source.store.authorization(),
                origin: source.store.origin(),
                redirects: source.store.redirects(),
                answer_challenge: source.store.kind() == Kind::Oci,
            },
            suffix: want.suffix,
            cap: want.bytes,
            deadline: after(&start, want.timeout_secs),
        };
        match stage::download(&job, writer) {
            Ok(got) if (got.size == want.bytes) & (got.sha256 == want.digest.as_str()) => {
                return Ok(got);
            }
            Ok(_) | Err(Fault::TooLarge) => {
                any_wrong = true;
                named_wrong |= source.named;
                let name = source.store.name();
                if !other_bytes.iter().any(|seen| seen == name) {
                    other_bytes.push(name.to_owned());
                }
            }
            // A storage fault is the host's: no other source would fare better.
            Err(Fault::Storage(e)) if stage::is_full(&e) => {
                return Err(PackageFailure::deferred(NO_SPACE));
            }
            Err(Fault::Storage(_)) => return Err(want.down),
            Err(Fault::Deadline | Fault::Net | Fault::Unexplained) => {}
        }
    }
    Err(match (named_wrong, any_wrong) {
        (true, _) => want.wrong,
        (false, true) => PackageFailure::deferred(STORE_DIGEST_MISMATCH),
        (false, false) => want.down,
    })
}

#[cfg(test)]
#[path = "entry_tests.rs"]
mod tests;
