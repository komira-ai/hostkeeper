//! The coordinator's private Xcode store (docs/design/maintenance-updates.md, sections 2.0 and
//! 2.2): the directory of `--xcode-dir`, where each archive an operator pushes is kept exactly as
//! Apple ships it, named by its SHA-256, and read only by the hosts of a group whose `xcode_keep` is at
//! least 1. Nothing of it is ever served to anyone else, repackaged or changed.
//!
//! The directory holds, for each archive, two files: `<sha256>.xip`, the archive's bytes, and
//! `<sha256>.json`, its record ([`Record`]: version, build, size, who uploaded it and when). An
//! upload is written to `.upload-<sha256>-<token>-<n>` beside them (a name no listing reads;
//! `<token>` is random bytes drawn when the store is opened, so two coordinators that share the
//! directory never make one name, and `<n>` counts the store's uploads), and becomes
//! an archive only once its SHA-256 is its name: the record is written first and the archive
//! renamed into place after it, so a release is held ([`XcodeStore::archives`]) only while both
//! files exist. Every file is created with mode 0600; the directory is the operator's to make, as
//! `--ui-dir` is, and the store refuses to start without it. Opening the store removes the upload
//! files whose last write is older than [`STALE_UPLOAD`] (an upload a stopped coordinator left);
//! a newer one may be another coordinator's upload in progress and stays.
//!
//! The store keeps the newest releases, as many as the largest `xcode_keep` of any group (at least
//! one), plus each release a host reports installed, and removes the rest ([`kept`]). A version
//! names one build on our hosts: a second build under a version the store holds is refused, and
//! the refusal stands as the alert `xcode_version_taken` ([`XcodeStore::taken`]) until the held
//! archive of that version is removed. That record is the coordinator's memory, like a failed
//! canary's: a restart forgets it.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

use hostkeeper_release::grammar::is_sha256;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::desired::{sync_dir, write_whole};
use crate::enrollment::nonce::{Random, os_random};
use crate::xcode_feed::{XcodeHeld, XcodeId, XcodeVersion};

/// The path under which the store's routes are: `PUT /v1/xcode/<sha256>` on the private listener
/// ([`crate::xcode_put`]) and `GET /v1/xcode/<sha256>` on the heartbeat listener
/// ([`crate::xcode_get`]).
pub(crate) const PREFIX: &str = "/v1/xcode/";

/// The default of `--xcode-max-bytes`: 20 GiB, above any Xcode archive Apple has shipped.
pub(crate) const DEFAULT_MAX_BYTES: u64 = 20 * 1024 * 1024 * 1024;

/// The ending of an archive's file.
const ARCHIVE: &str = ".xip";

/// The ending of an archive's record.
const RECORD: &str = ".json";

/// The start of the name of an upload in progress.
const UPLOAD: &str = ".upload-";

/// How old an upload file's last write must be for [`XcodeStore::open`] to remove it: far longer
/// than any upload, so that a coordinator that starts never removes the upload another coordinator
/// of the same directory has in progress.
pub(crate) const STALE_UPLOAD: Duration = Duration::from_secs(24 * 60 * 60);

/// The bytes of random the token of a store's upload names is made of.
const TOKEN_BYTES: usize = 8;

/// The most bytes of a record read: a record is about 200 bytes.
const MAX_RECORD_BYTES: u64 = 4096;

/// What the store keeps of an archive beside its bytes: `<sha256>.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Record {
    /// The Xcode version (`27.2`).
    pub(crate) version: String,
    /// Apple's build (`27B52`).
    pub(crate) build: String,
    /// The archive's size in bytes.
    pub(crate) size: u64,
    /// The operator token's name, or the user's, that uploaded it.
    pub(crate) by: String,
    /// When it was stored, in seconds since the epoch.
    pub(crate) at_unix: u64,
}

/// An archive the store holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Archive {
    /// Its SHA-256, as 64 lowercase hex characters: its name.
    pub(crate) sha256: String,
    /// The release it is.
    pub(crate) id: XcodeId,
    /// Its size in bytes.
    pub(crate) size: u64,
    /// Who uploaded it.
    pub(crate) by: String,
    /// When it was stored, in seconds since the epoch.
    pub(crate) at_unix: u64,
}

/// A second build refused under a version the store holds: what the alert
/// `xcode_version_taken` says.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Taken {
    /// The version.
    pub(crate) version: String,
    /// The build the store holds of it.
    pub(crate) held_build: String,
    /// The build that was refused.
    pub(crate) refused_build: String,
}

/// Why the store could not be opened, read or written.
#[derive(Debug, thiserror::Error)]
pub(crate) enum StoreError {
    /// `--xcode-dir` is not a directory the coordinator can read.
    #[error("{path}: not a directory the coordinator can read: {source}")]
    NotADirectory {
        /// The directory.
        path: PathBuf,
        /// What the system said.
        source: io::Error,
    },
    /// A file of the store could not be read, written or removed.
    #[error("{path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// What the system said.
        source: io::Error,
    },
}

/// The store in the directory of `--xcode-dir`.
#[derive(Debug)]
pub(crate) struct XcodeStore {
    dir: PathBuf,
    max_bytes: u64,
    /// Serialises the check of what the store holds with the change that follows it: an upload's
    /// commit, a removal.
    changing: Mutex<()>,
    /// The versions an upload of is in progress.
    uploading: Mutex<BTreeSet<String>>,
    /// The refusals of a second build, by version.
    taken: Mutex<BTreeMap<String, Taken>>,
    /// The random token of this store's upload names, as hex.
    token: String,
    /// The number of the next upload's file.
    next: AtomicU64,
}

/// `mutex` locked: a panic while it was held leaves nothing half done that a later holder trusts.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `bytes` as lowercase hex.
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::with_capacity(64), |mut text, b| {
        text.push(char::from(HEX[usize::from(b >> 4)]));
        text.push(char::from(HEX[usize::from(b & 0xf)]));
        text
    })
}

/// The digits of [`hex`].
const HEX: &[u8; 16] = b"0123456789abcdef";

/// Of `archives`, those the store keeps and those it removes: the `keep` newest releases (at
/// least one), and each one of `installed`. Both lists are newest first. A release just pushed
/// always stays only because an upload that this would remove at once (an older release than the
/// kept ones, that no host has installed) is refused before its body is read
/// ([`crate::xcode_put`]).
pub(crate) fn kept(
    archives: &[Archive],
    keep: u32,
    installed: &BTreeSet<XcodeId>,
) -> (Vec<Archive>, Vec<Archive>) {
    let mut newest_first = archives.to_vec();
    newest_first.sort_by(|a, b| b.id.cmp(&a.id).then_with(|| a.sha256.cmp(&b.sha256)));
    let keep = usize::try_from(keep.max(1)).unwrap_or(usize::MAX);
    let mut stays = Vec::new();
    let mut goes = Vec::new();
    for (rank, archive) in newest_first.into_iter().enumerate() {
        if rank < keep || installed.contains(&archive.id) {
            stays.push(archive);
        } else {
            goes.push(archive);
        }
    }
    (stays, goes)
}

/// What an upload is refused for before its body is read, by what the store holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Held {
    /// The store holds neither the release nor its version: the upload may go on.
    Free,
    /// The store holds the release, or an archive of these bytes.
    Already,
    /// The store holds another build of the version: the build it holds.
    VersionTaken(String),
}

impl XcodeStore {
    /// The store in `dir`, taking archives of at most `max_bytes`. An upload file a stopped
    /// coordinator left half written (one last written more than [`STALE_UPLOAD`] before `now`,
    /// the coordinator's clock of its audit) is removed; a newer one is left, as it may be another
    /// coordinator's upload in progress.
    ///
    /// # Errors
    /// [`StoreError::NotADirectory`]: `dir` is not a directory that can be read;
    /// [`StoreError::Io`]: a left upload could not be examined or removed, or no random bytes
    /// could be had for the token of the upload names.
    pub(crate) fn open(dir: &Path, max_bytes: u64, now: SystemTime) -> Result<Self, StoreError> {
        Self::open_with(dir, max_bytes, now, &os_random())
    }

    /// [`Self::open`], drawing the token of the upload names from `random`.
    fn open_with(
        dir: &Path,
        max_bytes: u64,
        now: SystemTime,
        random: &Random,
    ) -> Result<Self, StoreError> {
        let not_dir = |source| StoreError::NotADirectory {
            path: dir.to_owned(),
            source,
        };
        let entries = fs::read_dir(dir).map_err(not_dir)?;
        for entry in entries {
            let entry = entry.map_err(|source| StoreError::Io {
                path: dir.to_owned(),
                source,
            })?;
            if entry.file_name().to_string_lossy().starts_with(UPLOAD) {
                let path = entry.path();
                let stale = entry
                    .metadata()
                    .and_then(|meta| meta.modified())
                    .map(|written| {
                        now.duration_since(written)
                            .is_ok_and(|age| age > STALE_UPLOAD)
                    })
                    .map_err(|source| StoreError::Io {
                        path: path.clone(),
                        source,
                    })?;
                if stale {
                    fs::remove_file(&path).map_err(|source| StoreError::Io { path, source })?;
                }
            }
        }
        let mut bytes = [0u8; TOKEN_BYTES];
        if !random(&mut bytes) {
            return Err(StoreError::Io {
                path: dir.to_owned(),
                source: io::Error::other("no random bytes for the names of uploads"),
            });
        }
        Ok(Self {
            dir: dir.to_owned(),
            max_bytes,
            changing: Mutex::new(()),
            uploading: Mutex::new(BTreeSet::new()),
            taken: Mutex::new(BTreeMap::new()),
            token: hex(&bytes),
            next: AtomicU64::new(0),
        })
    }

    /// The most bytes an archive may have (`--xcode-max-bytes`).
    pub(crate) fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    /// The file of the archive `sha256`.
    pub(crate) fn archive_path(&self, sha256: &str) -> PathBuf {
        self.dir.join(format!("{sha256}{ARCHIVE}"))
    }

    /// The record of the archive `sha256`.
    fn record_path(&self, sha256: &str) -> PathBuf {
        self.dir.join(format!("{sha256}{RECORD}"))
    }

    /// `source` at `path`.
    fn io(path: &Path, source: io::Error) -> StoreError {
        StoreError::Io {
            path: path.to_owned(),
            source,
        }
    }

    /// Each archive the store holds, newest release first. A record that cannot be read, or
    /// whose archive is missing, is left out and logged.
    ///
    /// # Errors
    /// [`StoreError::Io`]: the directory could not be listed.
    pub(crate) fn archives(&self) -> Result<Vec<Archive>, StoreError> {
        let entries = fs::read_dir(&self.dir).map_err(|why| Self::io(&self.dir, why))?;
        let mut archives = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|why| Self::io(&self.dir, why))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(sha256) = name.strip_suffix(RECORD).filter(|sha| is_sha256(sha)) else {
                continue;
            };
            match self.archive(sha256) {
                Ok(Some(archive)) => archives.push(archive),
                Ok(None) => {}
                Err(why) => tracing::warn!(%why, "an Xcode archive's record could not be read"),
            }
        }
        archives.sort_by(|a, b| b.id.cmp(&a.id).then_with(|| a.sha256.cmp(&b.sha256)));
        Ok(archives)
    }

    /// The archive `sha256`: none when the store does not hold it (no record, or no archive
    /// beside its record).
    ///
    /// # Errors
    /// [`StoreError::Io`]: its record could not be read or is not a record.
    pub(crate) fn archive(&self, sha256: &str) -> Result<Option<Archive>, StoreError> {
        if !is_sha256(sha256) {
            return Ok(None);
        }
        let path = self.record_path(sha256);
        let text = match read_small(&path) {
            Ok(text) => text,
            Err(why) if why.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(why) => return Err(Self::io(&path, why)),
        };
        let invalid =
            |why: String| Self::io(&path, io::Error::new(io::ErrorKind::InvalidData, why));
        let record: Record =
            serde_json::from_slice(&text).map_err(|why| invalid(why.to_string()))?;
        let version =
            XcodeVersion::parse(&record.version).map_err(|why| invalid(why.to_string()))?;
        match fs::metadata(self.archive_path(sha256)) {
            Ok(_) => {}
            Err(why) if why.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(why) => return Err(Self::io(&self.archive_path(sha256), why)),
        }
        Ok(Some(Archive {
            sha256: sha256.to_owned(),
            id: XcodeId {
                version,
                build: record.build,
            },
            size: record.size,
            by: record.by,
            at_unix: record.at_unix,
        }))
    }

    /// What the store holds of `id` and of the archive `sha256`.
    ///
    /// # Errors
    /// [`StoreError`]: the store could not be read.
    pub(crate) fn holds(&self, id: &XcodeId, sha256: &str) -> Result<Held, StoreError> {
        let archives = self.archives()?;
        if archives
            .iter()
            .any(|archive| archive.id == *id || archive.sha256 == sha256)
        {
            return Ok(Held::Already);
        }
        Ok(archives
            .into_iter()
            .find(|archive| archive.id.version.as_str() == id.version.as_str())
            .map_or(Held::Free, |archive| Held::VersionTaken(archive.id.build)))
    }

    /// Notes that `refused_build` of `version` was refused because the store holds `held_build`.
    pub(crate) fn note_taken(&self, version: &str, held_build: &str, refused_build: &str) {
        lock(&self.taken).insert(
            version.to_owned(),
            Taken {
                version: version.to_owned(),
                held_build: held_build.to_owned(),
                refused_build: refused_build.to_owned(),
            },
        );
    }

    /// The refusals of a second build that stand: one per version, in version order.
    pub(crate) fn taken(&self) -> Vec<Taken> {
        lock(&self.taken).values().cloned().collect()
    }

    /// Marks an upload of `version` in progress: none when one already is. The mark ends when
    /// the answer is dropped.
    pub(crate) fn reserve(&self, version: &str) -> Option<Reserved<'_>> {
        lock(&self.uploading)
            .insert(version.to_owned())
            .then(|| Reserved {
                store: self,
                version: version.to_owned(),
            })
    }

    /// A new file for an upload of `sha256`, created with mode 0600: its path and the file.
    ///
    /// # Errors
    /// [`StoreError::Io`]: it could not be created.
    pub(crate) fn upload_file(&self, sha256: &str) -> Result<(PathBuf, fs::File), StoreError> {
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        let path = self
            .dir
            .join(format!("{UPLOAD}{sha256}-{}-{n}", self.token));
        let file = create_private(&path).map_err(|why| Self::io(&path, why))?;
        Ok((path, file))
    }

    /// Takes the change lock: the check of what the store holds and the change after it run
    /// under it.
    pub(crate) fn changing(&self) -> MutexGuard<'_, ()> {
        lock(&self.changing)
    }

    /// Makes the upload at `upload` (whose SHA-256 has been checked to be `archive.sha256`) the
    /// archive: its record written, then its file renamed into place and the directory synced.
    /// The caller holds [`Self::changing`]. On failure nothing of it is kept.
    ///
    /// # Errors
    /// [`StoreError::Io`].
    pub(crate) fn commit(&self, upload: &Path, archive: &Archive) -> Result<(), StoreError> {
        let record = Record {
            version: archive.id.version.as_str().to_owned(),
            build: archive.id.build.clone(),
            size: archive.size,
            by: archive.by.clone(),
            at_unix: archive.at_unix,
        };
        let record_path = self.record_path(&archive.sha256);
        let text = serde_json::to_vec(&record).map_err(|why| Self::io(&record_path, why.into()))?;
        let written = write_whole(&record_path, &text)
            .map_err(|why| Self::io(&record_path, io::Error::other(why.to_string())))
            .and_then(|()| {
                let path = self.archive_path(&archive.sha256);
                fs::rename(upload, &path).map_err(|why| Self::io(&path, why))
            })
            .and_then(|()| sync_dir(&self.dir).map_err(|why| Self::io(&self.dir, why)));
        if written.is_err() {
            remove_quietly(&record_path);
            let mut temp = record_path.clone().into_os_string();
            temp.push(".new");
            remove_quietly(Path::new(&temp));
            remove_quietly(&self.archive_path(&archive.sha256));
        }
        written
    }

    /// Removes the archive `archive`: its file, then its record; the refusal of a second build
    /// of its version no longer stands. The caller holds [`Self::changing`].
    ///
    /// # Errors
    /// [`StoreError::Io`]: a file could not be removed.
    pub(crate) fn remove(&self, archive: &Archive) -> Result<(), StoreError> {
        for path in [
            self.archive_path(&archive.sha256),
            self.record_path(&archive.sha256),
        ] {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(why) if why.kind() == io::ErrorKind::NotFound => {}
                Err(why) => return Err(Self::io(&path, why)),
            }
        }
        lock(&self.taken).remove(archive.id.version.as_str());
        sync_dir(&self.dir).map_err(|why| Self::io(&self.dir, why))
    }
}

impl XcodeHeld for XcodeStore {
    fn held(&self) -> Result<Vec<XcodeId>, String> {
        self.archives()
            .map(|archives| archives.into_iter().map(|archive| archive.id).collect())
            .map_err(|why| why.to_string())
    }
}

/// An upload of a version in progress ([`XcodeStore::reserve`]).
#[derive(Debug)]
pub(crate) struct Reserved<'a> {
    store: &'a XcodeStore,
    version: String,
}

impl Drop for Reserved<'_> {
    fn drop(&mut self) {
        lock(&self.store.uploading).remove(&self.version);
    }
}

/// A file at `path` made for writing, mode 0600, that must not exist yet.
fn create_private(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

/// The file at `path`, at most [`MAX_RECORD_BYTES`] of it.
fn read_small(path: &Path) -> io::Result<Vec<u8>> {
    use std::io::Read;
    let mut text = Vec::new();
    fs::File::open(path)?
        .take(MAX_RECORD_BYTES)
        .read_to_end(&mut text)?;
    Ok(text)
}

/// Removes the file at `path`, if it is there; a failure is logged.
pub(crate) fn remove_quietly(path: &Path) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(why) if why.kind() == io::ErrorKind::NotFound => {}
        Err(why) => {
            tracing::warn!(%why, path = %path.display(), "a file of the Xcode store could not be removed")
        }
    }
}

/// The SHA-256 of bytes fed in order, and how many there were.
#[derive(Default)]
pub(crate) struct Digesting {
    digest: Sha256,
    bytes: u64,
}

impl Digesting {
    /// Feeds `chunk`.
    pub(crate) fn feed(&mut self, chunk: &[u8]) {
        self.digest.update(chunk);
        self.bytes = self
            .bytes
            .saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
    }

    /// How many bytes were fed.
    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }

    /// The SHA-256 of what was fed, as lowercase hex.
    pub(crate) fn finish(self) -> String {
        hex(&self.digest.finalize())
    }
}

#[cfg(test)]
#[path = "xcode_store_tests.rs"]
mod tests;
