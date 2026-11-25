//! The partial uploads of the Xcode store (docs/design/xcode-one-action.md, section 3), a child of
//! [`crate::xcode_store`]: the bytes of an archive received so far, kept so that an upload that
//! was cut off continues from its last byte.
//!
//! A partial upload of the archive `<sha256>` is three names in the store's directory, each made
//! with mode 0600:
//!
//! - `.partial-<sha256>`: the bytes received, appended and synced by each request that sends a
//!   part (`Content-Range`, [`crate::xcode_put`]);
//! - `.partial-<sha256>.json`: what the first request said it was ([`UploadRecord`]: version,
//!   build, size, and the operator who started it). A request whose size, version or build differ
//!   does not continue it;
//! - `.partial-<sha256>.lock`: held by the one request writing it ([`XcodeStore::lock_upload`]).
//!
//! The lock file is made exclusively, so of two writers (two requests of one coordinator, or two
//! coordinators over one directory) one makes it and the other is refused: its text is written to
//! a file of its own, `.partial-<sha256>.lock.<writer>`, made with `O_CREAT|O_EXCL`, which is then
//! linked to the lock's name (`link(2)` fails when the name is taken, and the name never exists
//! without its text).
//! It holds its writer's name and the second, by the store's clock, of its last refresh
//! ([`UploadLock::refresh`], once for each second in which a block is written). A lock not
//! refreshed for [`LOCK_STALE`] seconds, or whose text is not a lock's, is stale: the next writer
//! removes it and makes its own. Two writers that both found one stale can both go on for a
//! moment; the first to refresh finds the lock is not its own and stops, and what would have
//! been written wrong cannot be stored, since the whole file's SHA-256 is checked before the
//! archive is.
//!
//! An upload whose last write is older than [`STALE_UPLOAD`] is removed, with its record and its lock, by
//! [`XcodeStore::lapse`]: the sweep of the store's start and of the daily job `xcode-feed`
//! ([`sweep`]). The same sweep removes `.upload-` files that old, which a request that is not a
//! part leaves when the coordinator stops in the middle of it.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::io::Write;
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use hostkeeper_release::grammar::is_sha256;
use serde::Deserialize;

use super::{
    STALE_UPLOAD, StoreError, UPLOAD, XcodeStore, create_private, lock, read_small, remove_quietly,
};
use crate::desired::{sync_dir, write_whole};

/// The start of the names of a partial upload's files.
pub(super) const PARTIAL: &str = ".partial-";

/// The ending of a partial upload's record.
const RECORD_END: &str = ".json";

/// The ending of a partial upload's lock.
const LOCK_END: &str = ".lock";

/// How many seconds a lock may go without a refresh before it is stale: three times the listener's
/// `CLIENT_PATIENCE`, past which a stalled body is already cut off.
pub(crate) const LOCK_STALE: u64 = 30;

/// The width of a number in a lock's text, so that a refresh is one write of the same length.
const WIDTH: usize = 20;

/// The clock of the store's locks.
pub(super) struct Clock(pub(super) crate::audit_file::UnixClock);

impl std::fmt::Debug for Clock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Clock")
    }
}

/// What `.partial-<sha256>.json` holds.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct UploadRecord {
    /// The Xcode version the upload names.
    pub(crate) version: String,
    /// The build the upload names.
    pub(crate) build: String,
    /// The archive's whole size in bytes.
    pub(crate) size: u64,
    /// The operator token's name, or the user's, whose request started it.
    pub(crate) by: String,
}

/// A partial upload the store keeps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Upload {
    /// The SHA-256 the whole archive must have.
    pub(crate) sha256: String,
    /// What the first request said.
    pub(crate) record: UploadRecord,
    /// The bytes received so far.
    pub(crate) received: u64,
    /// The last write to it, in seconds since the epoch.
    pub(crate) last_write_unix: u64,
}

/// Why a partial upload could not be locked or kept locked.
#[derive(Debug, thiserror::Error)]
pub(crate) enum LockError {
    /// Another writer holds the lock, and it is not stale; or the lock this writer made is gone
    /// or is another's now.
    #[error("another writer holds the partial upload")]
    Held,
    /// A file of the store could not be read or written.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// `text` as a lock's: the writer's name and the second of its last refresh.
fn parse_lock(text: &[u8]) -> Option<(&str, u64)> {
    let (owner, written) = std::str::from_utf8(text).ok()?.trim_end().split_once(' ')?;
    (!owner.is_empty()).then_some(())?;
    Some((owner, written.parse().ok()?))
}

/// The text of a lock of `owner` last refreshed at `now`.
fn lock_text(owner: &str, now: u64) -> String {
    format!("{owner} {now:0WIDTH$}\n")
}

/// `time` in seconds since the epoch; 0 for a time before it.
fn unix_of(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

impl XcodeStore {
    /// The file of the partial upload `sha256`.
    fn upload_path(&self, sha256: &str) -> PathBuf {
        self.dir.join(format!("{PARTIAL}{sha256}"))
    }

    /// The record of the partial upload `sha256`.
    fn upload_record_path(&self, sha256: &str) -> PathBuf {
        self.dir.join(format!("{PARTIAL}{sha256}{RECORD_END}"))
    }

    /// The lock of the partial upload `sha256`.
    fn lock_path(&self, sha256: &str) -> PathBuf {
        self.dir.join(format!("{PARTIAL}{sha256}{LOCK_END}"))
    }

    /// Seconds since the epoch by the store's clock.
    fn now(&self) -> u64 {
        (self.clock.0)()
    }

    /// The same store with `clock` as the clock of its locks.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_clock(mut self, clock: crate::audit_file::UnixClock) -> Self {
        self.clock = Clock(clock);
        self
    }

    /// The partial upload of `sha256`: none when there is no record of one. A record without its
    /// file is an upload of no bytes.
    ///
    /// # Errors
    /// [`StoreError::Io`]: the record or the file could not be read, or is not a record.
    pub(crate) fn upload(&self, sha256: &str) -> Result<Option<Upload>, StoreError> {
        if !is_sha256(sha256) {
            return Ok(None);
        }
        let path = self.upload_record_path(sha256);
        let text = match read_small(&path) {
            Ok(text) => text,
            Err(why) if why.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(why) => return Err(Self::io(&path, why)),
        };
        let record: UploadRecord = serde_json::from_slice(&text)
            .map_err(|why| Self::io(&path, io::Error::new(io::ErrorKind::InvalidData, why)))?;
        let data = self.upload_path(sha256);
        let (received, written) = match fs::metadata(&data) {
            Ok(meta) => (meta.len(), meta.modified()),
            Err(why) if why.kind() == io::ErrorKind::NotFound => {
                (0, fs::metadata(&path).and_then(|meta| meta.modified()))
            }
            Err(why) => return Err(Self::io(&data, why)),
        };
        Ok(Some(Upload {
            sha256: sha256.to_owned(),
            record,
            received,
            last_write_unix: unix_of(written.map_err(|why| Self::io(&data, why))?),
        }))
    }

    /// Each partial upload, the one written longest ago first. A record that cannot be read is
    /// left out and logged.
    ///
    /// # Errors
    /// [`StoreError::Io`]: the directory could not be listed.
    pub(crate) fn uploads(&self) -> Result<Vec<Upload>, StoreError> {
        let entries = fs::read_dir(&self.dir).map_err(|why| Self::io(&self.dir, why))?;
        let mut uploads = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|why| Self::io(&self.dir, why))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(sha256) = name
                .strip_prefix(PARTIAL)
                .and_then(|rest| rest.strip_suffix(RECORD_END))
                .filter(|sha| is_sha256(sha))
            else {
                continue;
            };
            match self.upload(sha256) {
                Ok(upload) => uploads.extend(upload),
                Err(why) => {
                    tracing::warn!(%why, "a partial Xcode upload's record could not be read")
                }
            }
        }
        uploads.sort_by(|a, b| {
            a.last_write_unix
                .cmp(&b.last_write_unix)
                .then_with(|| a.sha256.cmp(&b.sha256))
        });
        Ok(uploads)
    }

    /// Starts the partial upload of `sha256`: its record written (an earlier file without a record
    /// is removed, so that it holds no bytes). The caller holds the upload's lock.
    ///
    /// # Errors
    /// [`StoreError::Io`].
    pub(crate) fn begin_upload(
        &self,
        sha256: &str,
        record: &UploadRecord,
    ) -> Result<(), StoreError> {
        remove_quietly(&self.upload_path(sha256));
        let path = self.upload_record_path(sha256);
        let text = serde_json::json!({
            "version": record.version,
            "build": record.build,
            "size": record.size,
            "by": record.by,
        })
        .to_string();
        write_whole(&path, text.as_bytes())
            .map_err(|why| Self::io(&path, io::Error::other(why.to_string())))
    }

    /// The file of the partial upload `sha256`, opened to append (made, mode 0600, when it is not
    /// there). The caller holds the upload's lock.
    ///
    /// # Errors
    /// [`StoreError::Io`].
    pub(crate) fn append_upload(&self, sha256: &str) -> Result<fs::File, StoreError> {
        let path = self.upload_path(sha256);
        fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(&path)
            .map_err(|why| Self::io(&path, why))
    }

    /// The path of the file of the partial upload `sha256`, to read it whole or to rename it into
    /// place ([`Self::commit`]).
    pub(crate) fn upload_file_of(&self, sha256: &str) -> PathBuf {
        self.upload_path(sha256)
    }

    /// Removes the partial upload `sha256`: its file and its record, not its lock. The caller holds
    /// the upload's lock.
    ///
    /// # Errors
    /// [`StoreError::Io`]: a file could not be removed.
    pub(crate) fn remove_upload(&self, sha256: &str) -> Result<(), StoreError> {
        for path in [self.upload_path(sha256), self.upload_record_path(sha256)] {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(why) if why.kind() == io::ErrorKind::NotFound => {}
                Err(why) => return Err(Self::io(&path, why)),
            }
        }
        sync_dir(&self.dir).map_err(|why| Self::io(&self.dir, why))
    }

    /// Whether this store has a request writing the partial upload `sha256` now.
    pub(crate) fn is_writing(&self, sha256: &str) -> bool {
        lock(&self.writers).contains(sha256)
    }

    /// Takes the lock of the partial upload `sha256`. It ends when the answer is dropped.
    ///
    /// # Errors
    /// [`LockError::Held`]: another writer, of this store or of another over the same directory,
    /// holds it and has refreshed it within [`LOCK_STALE`] seconds. [`LockError::Store`]: a file
    /// could not be read, written or removed.
    pub(crate) fn lock_upload(&self, sha256: &str) -> Result<UploadLock<'_>, LockError> {
        if !lock(&self.writers).insert(sha256.to_owned()) {
            return Err(LockError::Held);
        }
        let owner = format!(
            "{}-{:0WIDTH$}",
            self.token,
            self.next.fetch_add(1, Ordering::Relaxed)
        );
        let taken = UploadLock {
            store: self,
            sha256: sha256.to_owned(),
            owner,
            refreshed: None,
        };
        // On a failure `taken` is dropped, which removes only a lock that is its own.
        taken.make()?;
        Ok(taken)
    }

    /// Removes what the sweep removes ([`sweep`]) of the directory, as of `now`: each upload
    /// file and each partial upload last written more than its allowance before. Each removal is
    /// logged. The names removed.
    ///
    /// # Errors
    /// [`StoreError::Io`]: the directory could not be listed, or a file could not be examined or
    /// removed.
    pub(crate) fn lapse(&self, now: SystemTime) -> Result<Vec<String>, StoreError> {
        let entries = fs::read_dir(&self.dir).map_err(|why| Self::io(&self.dir, why))?;
        sweep(&self.dir, entries, now)
    }
}

/// The lock of a partial upload, held by one request ([`XcodeStore::lock_upload`]).
#[derive(Debug)]
pub(crate) struct UploadLock<'a> {
    store: &'a XcodeStore,
    sha256: String,
    /// The name this writer's lock holds: the store's token and the number of the lock.
    owner: String,
    /// The second of the last refresh; none before the first.
    refreshed: Option<u64>,
}

impl UploadLock<'_> {
    /// Makes the lock file, or finds who has it: a lock that is fresh is [`LockError::Held`], one
    /// that is stale or not a lock's is removed and made again. The text is written to a file of
    /// its own (made with `O_CREAT|O_EXCL`) that is then linked to the lock's name, which fails
    /// if the name is taken, so that no other writer ever reads a lock that is made and not yet
    /// written.
    fn make(&self) -> Result<(), LockError> {
        let path = self.store.lock_path(&self.sha256);
        let mut temp = path.clone().into_os_string();
        temp.push(format!(".{}", self.owner));
        let temp = PathBuf::from(temp);
        let text = lock_text(&self.owner, self.store.now());
        create_private(&temp)
            .and_then(|mut file| {
                file.write_all(text.as_bytes())
                    .and_then(|()| file.sync_all())
            })
            .map_err(|why| XcodeStore::io(&temp, why))?;
        let made = self.link(&temp, &path);
        remove_quietly(&temp);
        made
    }

    /// Links the lock's text at `temp` to the lock's name `path`, taking over a stale lock there.
    fn link(&self, temp: &Path, path: &Path) -> Result<(), LockError> {
        loop {
            match fs::hard_link(temp, path) {
                Ok(()) => return Ok(()),
                Err(why) if why.kind() == io::ErrorKind::AlreadyExists => {}
                Err(why) => return Err(XcodeStore::io(path, why).into()),
            }
            match read_small(path) {
                Ok(held) => {
                    let fresh = parse_lock(&held).is_some_and(|(_, written)| {
                        self.store.now().saturating_sub(written) < LOCK_STALE
                    });
                    if fresh {
                        return Err(LockError::Held);
                    }
                    tracing::info!(sha256 = %self.sha256, "a stale lock of a partial Xcode upload was taken over");
                    fs::remove_file(path).or_else(|why| {
                        if why.kind() == io::ErrorKind::NotFound {
                            Ok(())
                        } else {
                            Err(XcodeStore::io(path, why))
                        }
                    })?;
                }
                // Removed since: make it.
                Err(why) if why.kind() == io::ErrorKind::NotFound => {}
                Err(why) => return Err(XcodeStore::io(path, why).into()),
            }
        }
    }

    /// Writes the second of now into the lock, once for each second: the writer is alive. It is
    /// [`LockError::Held`] when the lock is gone or is another's now, which is what a writer that
    /// took it over as stale leaves.
    ///
    /// # Errors
    /// [`LockError`].
    pub(crate) fn refresh(&mut self) -> Result<(), LockError> {
        let now = self.store.now();
        if self.refreshed == Some(now) {
            return Ok(());
        }
        let path = self.store.lock_path(&self.sha256);
        let held = match read_small(&path) {
            Ok(held) => held,
            Err(why) if why.kind() == io::ErrorKind::NotFound => return Err(LockError::Held),
            Err(why) => return Err(XcodeStore::io(&path, why).into()),
        };
        if parse_lock(&held).is_none_or(|(owner, _)| owner != self.owner) {
            return Err(LockError::Held);
        }
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .and_then(|file| file.write_all_at(lock_text(&self.owner, now).as_bytes(), 0))
            .map_err(|why| XcodeStore::io(&path, why))?;
        self.refreshed = Some(now);
        Ok(())
    }
}

impl Drop for UploadLock<'_> {
    fn drop(&mut self) {
        let path = self.store.lock_path(&self.sha256);
        let ours = read_small(&path)
            .is_ok_and(|held| parse_lock(&held).is_some_and(|(owner, _)| owner == self.owner));
        if ours {
            remove_quietly(&path);
        }
        lock(&self.store.writers).remove(&self.sha256);
    }
}

/// One name of the directory the sweep looks at.
struct Seen {
    path: PathBuf,
    written: SystemTime,
}

/// Removes from the directory `dir`, whose entries are `entries`, the upload files (`.upload-`)
/// and the partial uploads (`.partial-`) last written more than [`STALE_UPLOAD`] before `now`. The names
/// of one partial upload (its file, its record, its lock) go together, by the last write of its
/// file; a name of one that has no file goes by its own. Each removal is logged. The names
/// removed, in name order.
pub(super) fn sweep(
    dir: &Path,
    entries: fs::ReadDir,
    now: SystemTime,
) -> Result<Vec<String>, StoreError> {
    let mut groups: BTreeMap<String, BTreeMap<String, Seen>> = BTreeMap::new();
    for entry in entries {
        let entry = entry.map_err(|source| StoreError::Io {
            path: dir.to_owned(),
            source,
        })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(UPLOAD) && !name.starts_with(PARTIAL) {
            continue;
        }
        let path = entry.path();
        let written = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .map_err(|source| StoreError::Io {
                path: path.clone(),
                source,
            })?;
        // The names of one partial upload share its first `.partial-<sha256>` characters.
        let key = name
            .get(..PARTIAL.len() + 64)
            .filter(|head| head.strip_prefix(PARTIAL).is_some_and(is_sha256))
            .unwrap_or(&name)
            .to_owned();
        groups
            .entry(key)
            .or_default()
            .insert(name, Seen { path, written });
    }
    let lapsed = |written: SystemTime| {
        now.duration_since(written)
            .is_ok_and(|age| age > STALE_UPLOAD)
    };
    let mut removed = Vec::new();
    for (key, names) in groups {
        let basis = names.get(&key).map(|file| file.written);
        for (name, seen) in names {
            if lapsed(basis.unwrap_or(seen.written)) {
                fs::remove_file(&seen.path).map_err(|source| StoreError::Io {
                    path: seen.path.clone(),
                    source,
                })?;
                tracing::info!(%name, "a lapsed file of the Xcode store was removed");
                removed.push(name);
            }
        }
    }
    removed.sort();
    Ok(removed)
}

#[cfg(test)]
#[path = "xcode_partial_tests.rs"]
mod tests;
