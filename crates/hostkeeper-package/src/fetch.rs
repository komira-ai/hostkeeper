//! Step 1 of section 5.5, under the release layout of one package per platform: read the release
//! index of the version an order names, take the entry for this host's platform, download that
//! package into a staging file, and refuse it unless its size and SHA-256 are the entry's (and its
//! size is within `max_package_bytes`). When the order names the SHA-256 of the index, an index
//! that hashes to another is refused before it is parsed, and nothing after it is fetched.
//!
//! | what happened | outcome, class, reason |
//! |---|---|
//! | the order names the SHA-256 of the index and the index received hashes to another (checked before the index is read) | `Failure`, `release`, `index_digest_mismatch` |
//! | the index is not the index of this version (see [`crate::index`]), or is over 64 KiB | `Failure`, `release`, `index_invalid` |
//! | the index lists no package for this host's platform | `Failure`, `release`, `no_package_for_platform` |
//! | the entry's size, the announced length or the bytes received pass `max_package_bytes` or the entry's size (the transfer stops at once) | `Failure`, `release`, `package_digest_mismatch` |
//! | the bytes, all received, are fewer than the entry's size or do not hash to its SHA-256 | `Failure`, `release`, `package_digest_mismatch` |
//! | no connection, a status other than 2xx (404 included), a body cut short, `fetch_timeout_secs` passed, for the index or the package | `Deferred`, `environment`, `fetch_failed` |
//! | the disk is full while a staging file is written | `Deferred`, `environment`, `no_space` |
//! | the service directory is a link, is not a directory, or is writable by group or others; a staging file cannot be made, written or read for another reason | `Deferred`, `environment`, `fetch_failed` |
//!
//! Where files are made: only `packages_root/<service>/.fetch-<pid>-<n>`. The staging file is
//! never named after the package (`<service>-<version>-<platform>.tar.zst`, which for the longest
//! service name and version the grammar allows is longer than a file name may be), so a package
//! whose derived name is too long for a file system is still fetched: only its URL is built from
//! the name, and a channel that cannot serve it answers with a status (`fetch_failed`).
//!
//! The packages root is the daemon's: it makes it, owns it and keeps it from other accounts
//! (mode 0755 or tighter, owned by the daemon's user); that is its precondition, and this crate
//! does not check it. The service directory under it is checked on every use: it must be a real
//! directory (not a link) that is not writable by its group or by others, because anyone who can
//! write there can replace a staging file between its creation and its use.
//!
//! Every refusal removes the staging files. Nothing else is removed to make room, and no version
//! is compared with anything: the downgrade bound is the coordinator's `min_version` (design
//! Q13). The index is not trusted: whatever it says, the package must still be attested
//! ([`crate::prove`]) and its manifest must name the service, version and platform asked for (the
//! manifest step).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use hostkeeper_config::daemon::{Bounds, HttpsUrl, Sha256Hex};
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_steps::{Clock, Stamp};

use crate::failure::{
    FETCH_FAILED, INDEX_DIGEST_MISMATCH, INDEX_INVALID, NO_PACKAGE_FOR_PLATFORM, NO_SPACE,
    PACKAGE_DIGEST_MISMATCH, PackageFailure,
};
use crate::index::{MAX_INDEX_BYTES, Platform, ReleaseIndex, index_url, package_url};
use crate::ports::{Fetcher, Request, Url, after};
use crate::stage::{self, Download, Fault, Job, OpenWriter, PREFIX, TempFile};

/// What a fetch needs from the daemon: the network, the clock, the config's bounds and channel,
/// where packages live (`/var/lib/hostkeeper/packages` on a host), and the host's platform
/// ([`Platform::current`] on a host).
#[derive(Clone, Copy)]
pub struct FetchEnv<'a> {
    pub fetcher: &'a dyn Fetcher,
    pub clock: &'a dyn Clock,
    pub bounds: &'a Bounds,
    pub release_channel: &'a HttpsUrl,
    pub packages_root: &'a Path,
    pub platform: Platform,
}

/// The package an order names, already checked by the order grammar: a service and a version,
/// never a platform or a place, and the SHA-256 of the release index when the order carries one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageOrder {
    pub service: ServiceName,
    pub version: Version,
    /// The SHA-256 the index's bytes must have; `None` (an order from a coordinator that does not
    /// send it) checks nothing.
    pub index_digest: Option<Sha256Hex>,
}

/// The package for this host's platform, whose size and SHA-256 are the release index's, in a
/// staging file that is removed when this is dropped. Its bytes are not yet attested: the only
/// thing to do with it is [`crate::prove::prove`].
#[derive(Debug)]
pub struct StagedArchive {
    pub(crate) file: TempFile,
    pub(crate) dir: PathBuf,
    pub(crate) url: Url,
    pub(crate) platform: Platform,
    pub(crate) sha256: Sha256Hex,
    pub(crate) size: u64,
}

/// Removes the staging files (`.fetch-*`) left in `packages_root/<service>/` by a fetch that did
/// not end: regular files, and links, which are unlinked and never followed. Directories and
/// everything else are left, and so is every other name. A service with no directory has
/// nothing to remove.
///
/// [`fetch_package`] calls it before each fetch, and the daemon's start for each service, to
/// remove what an earlier release's fetch left. The caller holds the service's lock: a sweep while another fetch of the
/// same service runs would remove that fetch's staging file.
pub fn remove_orphan_fetches(packages_root: &Path, service: &ServiceName) -> io::Result<()> {
    let dir = packages_root.join(service.as_str());
    if !stage::is_dir(&dir)? {
        return Ok(());
    }
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let ours = entry
            .file_name()
            .as_encoded_bytes()
            .starts_with(PREFIX.as_bytes());
        if ours & (kind.is_file() | kind.is_symlink()) {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

/// Reads the release index of the version `order` names and downloads the package for
/// `env.platform` into a staging file in `packages/<service>/`, which it makes when it is
/// missing. See the module's table for every refusal. The caller holds the service's lock (see
/// [`remove_orphan_fetches`]).
///
/// The deadline is `fetch_timeout_secs` from the start of this call on the injected clock, for
/// the index and the package together: it is passed to the fetcher and checked again on every
/// chunk. The connect timeout is 30 s.
pub fn fetch_package(
    env: &FetchEnv<'_>,
    order: &PackageOrder,
) -> Result<StagedArchive, PackageFailure> {
    fetch_with(env, order, stage::plain)
}

/// [`fetch_package`], writing the staging files through `writer`.
pub(crate) fn fetch_with(
    env: &FetchEnv<'_>,
    order: &PackageOrder,
    writer: OpenWriter,
) -> Result<StagedArchive, PackageFailure> {
    let start = Stamp::now(env.clock);
    let deadline = after(&start, env.bounds.fetch_timeout_secs);
    let dir = env.packages_root.join(order.service.as_str());
    stage::make_dir(&dir).map_err(|e| failure(Fault::Storage(e)))?;
    remove_orphan_fetches(env.packages_root, &order.service)
        .map_err(|e| failure(Fault::Storage(e)))?;

    let index_at = index_url(env.release_channel, &order.service, &order.version);
    let index_job = Job {
        fetcher: env.fetcher,
        clock: env.clock,
        dir: &dir,
        request: Request::plain(&index_at),
        suffix: "",
        cap: MAX_INDEX_BYTES,
        deadline: deadline.clone(),
    };
    let fetched = stage::download(&index_job, writer).map_err(index_failure)?;
    let named = order.index_digest.as_ref();
    if named.is_some_and(|expected| fetched.sha256 != expected.as_str()) {
        return Err(PackageFailure::release(INDEX_DIGEST_MISMATCH));
    }
    let text = stage::read_back(fetched.file.path(), MAX_INDEX_BYTES)
        .map_err(|e| failure(Fault::Storage(e)))?;
    drop(fetched);
    let index = ReleaseIndex::parse(&text, &order.service, &order.version)
        .map_err(|_| PackageFailure::release(INDEX_INVALID))?;
    let entry = index
        .package(env.platform)
        .ok_or(PackageFailure::release(NO_PACKAGE_FOR_PLATFORM))?;
    if entry.bytes > env.bounds.max_package_bytes {
        return Err(PackageFailure::release(PACKAGE_DIGEST_MISMATCH));
    }

    let url = package_url(
        env.release_channel,
        &order.service,
        &order.version,
        env.platform,
    );
    let job = Job {
        fetcher: env.fetcher,
        clock: env.clock,
        dir: &dir,
        request: Request::plain(&url),
        suffix: "",
        cap: entry.bytes,
        deadline,
    };
    let Download { file, sha256, size } = stage::download(&job, writer).map_err(failure)?;
    if (size != entry.bytes) | (sha256 != entry.sha256.as_str()) {
        return Err(PackageFailure::release(PACKAGE_DIGEST_MISMATCH));
    }
    Ok(StagedArchive {
        file,
        dir,
        url,
        platform: env.platform,
        sha256: entry.sha256.clone(),
        size,
    })
}

/// The refusal a fault of the package's download is.
pub(crate) fn failure(fault: Fault) -> PackageFailure {
    match fault {
        Fault::TooLarge => PackageFailure::release(PACKAGE_DIGEST_MISMATCH),
        Fault::Storage(e) if stage::is_full(&e) => PackageFailure::deferred(NO_SPACE),
        // A 404 is `Net`, deferred like any other status: the channel may not have the version
        // yet, and the coordinator retries a deferred order.
        Fault::Storage(_) | Fault::Deadline | Fault::Net | Fault::Unexplained => {
            PackageFailure::deferred(FETCH_FAILED)
        }
    }
}

/// The refusal a fault of the index's download is: an index over its cap is not one; anything
/// else is as for the package.
fn index_failure(fault: Fault) -> PackageFailure {
    match fault {
        Fault::TooLarge => PackageFailure::release(INDEX_INVALID),
        other => failure(other),
    }
}

#[cfg(test)]
#[path = "fetch_tests.rs"]
mod tests;
