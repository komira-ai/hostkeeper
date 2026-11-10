//! Step 1 of section 5.5 through an order's entry (docs/design/packages.md, section 7.4): fetch the
//! entry's artifact from the host's stores by its pin alone, for an entry with no attestation (a
//! `macos-pkg` entry, whose package the installer checks the signature of itself). [`fetch_pinned`]
//! is the whole of it, and [`Pinned`] is what it gives: the staging file with the entry's size and
//! SHA-256.
//!
//! The sources are the host's `mirror` stores in file order (asked for the digest, listed in the
//! order or not), then the artifact's own locations in order. A location of a store the host does
//! not define, or whose locator breaks the locator grammar, is skipped. Each source is asked with
//! its store's credential and redirects ([`crate::stores`]) and has its own deadline,
//! `fetch_timeout_secs`, from the start of that source's request. The first source whose bytes have
//! the entry's size and SHA-256 wins; a source that sends more than the size is stopped and counts
//! as having given other bytes. An entry whose `bytes` is over `max_package_bytes` is refused before
//! anything is asked.
//!
//! | what happened | outcome, class, reason |
//! |---|---|
//! | some source gave the right bytes | go on; the stores that gave other bytes first are in [`Pinned::other_bytes`] |
//! | no source is defined | `Failure`, `environment`, `no_store` |
//! | every source failed to answer | `Deferred`, `environment`, `fetch_failed` |
//! | every source that answered gave other bytes, and one was a location | `Failure`, `release`, `package_digest_mismatch` |
//! | only mirrors answered, all with other bytes | `Deferred`, `environment`, `store_digest_mismatch` |
//! | the entry's digest or size is not one a fetch can ask for (a size of 0, over `max_package_bytes`) | `Failure`, `release`, `package_digest_mismatch` |
//! | the disk is full while a staging file is written | `Deferred`, `environment`, `no_space` |
//! | the service directory is a link, is not a directory, or is writable by group or others; a staging file cannot be made or written for another reason (a storage fault stops at once: it is the host's, not the source's) | `Deferred`, `environment`, `fetch_failed` |
//!
//! The staging files are `packages/<service>/.fetch-<pid>-<n>`; orphans are removed first
//! ([`remove_orphan_fetches`]), and every refusal removes them all.

use std::path::{Path, PathBuf};

use hostkeeper_config::daemon::{Bounds, Sha256Hex};
use hostkeeper_config::names::ServiceName;
use hostkeeper_proto::hostkeeper::release::v1::{Location, Target};
use hostkeeper_release::grammar::check_locator;
use hostkeeper_steps::{Clock, Stamp};

use crate::failure::{
    FETCH_FAILED, NO_SPACE, NO_STORE, PACKAGE_DIGEST_MISMATCH, PackageFailure,
    STORE_DIGEST_MISMATCH,
};
use crate::ports::{Fetcher, Request, Url, after};
use crate::stage::{self, Download, Fault, Job, OpenWriter, remove_orphan_fetches};
use crate::stores::{Kind, Store, Stores};

/// What a fetch of an entry's artifact by its pin alone ([`fetch_pinned`]) needs: the network, the
/// clock, the config's bounds, the host's stores and where packages live
/// (`/var/lib/hostkeeper/packages` on a host). No verifier and no trusted signer: nothing is
/// proven.
#[derive(Clone, Copy)]
pub struct PinnedEnv<'a> {
    pub fetcher: &'a dyn Fetcher,
    pub clock: &'a dyn Clock,
    pub bounds: &'a Bounds,
    pub stores: &'a Stores,
    pub packages_root: &'a Path,
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

/// Fetches the artifact `entry` names, for `service`, by its pin alone. See the module's table
/// for every refusal. The caller holds the service's lock (see [`remove_orphan_fetches`]).
pub fn fetch_pinned(
    env: &PinnedEnv<'_>,
    service: &ServiceName,
    entry: &Target,
) -> Result<Pinned, PackageFailure> {
    fetch_pinned_with(env, service, entry, stage::plain)
}

/// [`fetch_pinned`], writing the staging file through `writer`.
pub(crate) fn fetch_pinned_with(
    env: &PinnedEnv<'_>,
    service: &ServiceName,
    entry: &Target,
    writer: OpenWriter,
) -> Result<Pinned, PackageFailure> {
    let digest = Sha256Hex::parse(&entry.digest)
        .filter(|_| (1..=env.bounds.max_package_bytes).contains(&entry.bytes))
        .ok_or(PackageFailure::release(PACKAGE_DIGEST_MISMATCH))?;
    let dir = staging_dir(env, service)?;
    let mut other_bytes = Vec::new();
    let got = fetch_artifact(env, &dir, &digest, entry, writer, &mut other_bytes)?;
    Ok(Pinned {
        file: got.file,
        sha256: digest,
        size: got.size,
        other_bytes,
    })
}

/// The service's directory under the packages root, made if need be, with the orphans of earlier
/// fetches removed.
fn staging_dir(env: &PinnedEnv<'_>, service: &ServiceName) -> Result<PathBuf, PackageFailure> {
    let dir = env.packages_root.join(service.as_str());
    stage::make_dir(&dir).map_err(|e| storage_failure(&e))?;
    remove_orphan_fetches(env.packages_root, service).map_err(|e| storage_failure(&e))?;
    Ok(dir)
}

/// The refusal of a fault of the host's files: `no_space` for a full disk, `fetch_failed` for any
/// other.
fn storage_failure(e: &std::io::Error) -> PackageFailure {
    if stage::is_full(e) {
        PackageFailure::deferred(NO_SPACE)
    } else {
        PackageFailure::deferred(FETCH_FAILED)
    }
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

/// Downloads `entry`'s artifact, whose SHA-256 is `digest`, into a staging file in `dir` from the
/// first source that gives its size and SHA-256, and notes in `other_bytes` the store of each
/// source that gave other bytes.
fn fetch_artifact(
    env: &PinnedEnv<'_>,
    dir: &Path,
    digest: &Sha256Hex,
    entry: &Target,
    writer: OpenWriter,
    other_bytes: &mut Vec<String>,
) -> Result<Download, PackageFailure> {
    let sources = sources(env.stores, digest, &entry.locations);
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
            cap: entry.bytes,
            deadline: after(&start, env.bounds.fetch_timeout_secs),
        };
        match stage::download(&job, writer) {
            Ok(got) if (got.size == entry.bytes) & (got.sha256 == digest.as_str()) => {
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
            Err(Fault::Storage(e)) => return Err(storage_failure(&e)),
            Err(Fault::Deadline | Fault::Net | Fault::Unexplained) => {}
        }
    }
    Err(match (named_wrong, any_wrong) {
        (true, _) => PackageFailure::release(PACKAGE_DIGEST_MISMATCH),
        (false, true) => PackageFailure::deferred(STORE_DIGEST_MISMATCH),
        (false, false) => PackageFailure::deferred(FETCH_FAILED),
    })
}

#[cfg(test)]
#[path = "entry_tests.rs"]
mod tests;
