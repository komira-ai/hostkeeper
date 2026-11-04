//! The deferral: an update the host declined for a reason about its moment, and the unix second
//! before which the loop does not run that target again (design 5.2 step 6, 8.3).
//!
//! **On disk.** One file per service, [`DEFERRAL_FILE`] in the service's step-record directory
//! (`<state-dir>/<service>/`), beside the step records. The time is in unix seconds, because a
//! restart has no memory of the monotonic clock. The schema is the one kbf's agent gives its own
//! deferral file (`defer-<unit>` in its state directory), `{"target", "reason",
//! "retry_after_unix"}` and nothing else, so either program reads a file of the other's schema; a
//! key added later is ignored by both (formats change additively).
//!
//! Every access goes through a handle on the service's directory, opened without following a
//! link and refused unless it is a directory owned by the daemon's user and closed to writes by
//! group and others; the file is opened under that handle, again without following a link and
//! without waiting on a special file, and only a regular file is read. A write goes to a new
//! temporary file of mode 0600, synced, renamed over the file, and the directory synced; a
//! removal syncs the directory too, so neither comes back after a crash.
//!
//! **In memory.** [`Book`] is the loop's view: it reads a service's file once, keeps the
//! deferral in memory from then on, caps it at [`MAX_RETRY_SECS`] from now at every read, and
//! writes every change. A write that fails is kept in memory and tried again at every heartbeat,
//! so a disk error never lets a target run before its retry time.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use hostkeeper_config::names::{ServiceName, Version};
use rustix::fs::{AtFlags, CWD, Mode, OFlags};
use rustix::io::Errno;
use serde::{Deserialize, Serialize};

use crate::names::Reason;
use crate::order::DeferredReport;
use crate::ports::Log;

/// The file's name in the service's step-record directory.
pub const DEFERRAL_FILE: &str = "deferral.json";

/// The temporary file a write goes through.
pub const TEMP_FILE: &str = ".deferral.json.tmp";

/// The longest a deferral lasts, in seconds, whatever the verdict or an older file says: kbf's
/// cap, and the coordinator's backoff cap, so a bad retry hint cannot hold a host for long.
pub const MAX_RETRY_SECS: u64 = 3600;

/// The most bytes of the file that are read.
pub const FILE_MAX: u64 = 4096;

/// A target the host declined, and until when.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Deferral {
    pub target: Version,
    pub reason: Reason,
    /// The unix second from which the target may run again.
    pub retry_after_unix: u64,
}

impl Deferral {
    /// The deferral of a verdict at `now`: `retry_after_secs` from now, at most
    /// [`MAX_RETRY_SECS`].
    pub fn starting(target: Version, reason: Reason, now: u64, retry_after_secs: u64) -> Deferral {
        Deferral {
            target,
            reason,
            retry_after_unix: now.saturating_add(retry_after_secs.min(MAX_RETRY_SECS)),
        }
    }

    /// The deferral as read at `now`: its retry time no further than [`MAX_RETRY_SECS`] ahead.
    pub fn read_at(mut self, now: u64) -> Deferral {
        self.retry_after_unix = self
            .retry_after_unix
            .min(now.saturating_add(MAX_RETRY_SECS));
        self
    }

    /// The report a heartbeat at `now` carries: none once the retry time has come.
    pub fn report(&self, now: u64) -> Option<DeferredReport> {
        (now < self.retry_after_unix).then(|| DeferredReport {
            target: self.target.clone(),
            reason: self.reason.clone(),
            retry_after_secs: self.retry_after_unix - now,
        })
    }

    /// Whether an update to `target` at `now` is one this deferral declines.
    pub fn declines(&self, target: &Version, now: u64) -> bool {
        (self.target == *target) & (now < self.retry_after_unix)
    }
}

/// The file's text.
#[derive(Serialize, Deserialize)]
struct OnDisk {
    target: String,
    reason: String,
    retry_after_unix: u64,
}

/// Why a deferral file is not used.
#[derive(Debug, thiserror::Error)]
pub enum DeferralError {
    #[error("cannot read {}: {source}", .path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("{} is not a regular file", .path.display())]
    NotRegular { path: PathBuf },
    #[error("{} is longer than {FILE_MAX} bytes", .path.display())]
    TooLong { path: PathBuf },
    #[error("{} is not a deferral: {source}", .path.display())]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("{} holds a target or a reason outside its grammar", .path.display())]
    Grammar { path: PathBuf },
}

/// The deferral files under a state directory.
#[derive(Clone, Debug)]
pub struct Deferrals {
    dir: PathBuf,
}

impl Deferrals {
    /// The files under `state_dir`, the directory of the services' step-record directories.
    pub fn new(state_dir: &Path) -> Deferrals {
        Deferrals {
            dir: state_dir.to_path_buf(),
        }
    }

    /// The file of `service`.
    pub fn path(&self, service: &ServiceName) -> PathBuf {
        self.dir.join(service.as_str()).join(DEFERRAL_FILE)
    }

    /// A handle on the step-record directory of `service`: not a link, a directory, owned by
    /// this process's user and not writable by group or others.
    fn directory(&self, service: &ServiceName) -> io::Result<File> {
        let path = self.dir.join(service.as_str());
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let dir = File::from(rustix::fs::openat(CWD, &path, flags, Mode::empty())?);
        let meta = dir.metadata()?;
        let mine = meta.uid() == rustix::process::geteuid().as_raw();
        if mine & ((meta.mode() & 0o022) == 0) {
            Ok(dir)
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} is not a directory of this user alone", path.display()),
            ))
        }
    }

    /// The file of `service`, opened under a handle on its directory: not through a link, and
    /// without waiting on a special file.
    fn open(&self, service: &ServiceName) -> io::Result<File> {
        let dir = self.directory(service)?;
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        Ok(File::from(rustix::fs::openat(
            &dir,
            DEFERRAL_FILE,
            flags,
            Mode::empty(),
        )?))
    }

    /// The deferral of `service` as the file holds it; none when there is no file or no
    /// step-record directory.
    ///
    /// # Errors
    /// The directory or the file is there and cannot be used: a link, a directory not the
    /// daemon's alone, not a regular file, longer than [`FILE_MAX`], or no deferral.
    pub fn load(&self, service: &ServiceName) -> Result<Option<Deferral>, DeferralError> {
        let path = self.path(service);
        let unreadable = |source| DeferralError::Io {
            path: path.clone(),
            source,
        };
        let file = match self.open(service) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(unreadable(source)),
        };
        if !file.metadata().map_err(unreadable)?.is_file() {
            return Err(DeferralError::NotRegular { path: path.clone() });
        }
        let mut bytes = Vec::new();
        file.take(FILE_MAX + 1)
            .read_to_end(&mut bytes)
            .map_err(unreadable)?;
        if bytes.len() as u64 > FILE_MAX {
            return Err(DeferralError::TooLong { path });
        }
        let text: OnDisk =
            serde_json::from_slice(&bytes).map_err(|source| DeferralError::Json {
                path: path.clone(),
                source,
            })?;
        let target = Version::new(&text.target).ok();
        let reason = Reason::new(&text.reason).ok();
        target
            .zip(reason)
            .map(|(target, reason)| {
                Some(Deferral {
                    target,
                    reason,
                    retry_after_unix: text.retry_after_unix,
                })
            })
            .ok_or(DeferralError::Grammar { path })
    }

    /// Records `deferral` for `service`, durably. A temporary file left by a crash is removed
    /// first, never written through.
    ///
    /// # Errors
    /// The service's directory cannot be used or does not take it; the file it had is left as
    /// it was.
    pub fn save(&self, service: &ServiceName, deferral: &Deferral) -> io::Result<()> {
        let dir = self.directory(service)?;
        remove(&dir, TEMP_FILE)?;
        let body = serde_json::to_vec(&OnDisk {
            target: deferral.target.as_str().to_owned(),
            reason: deferral.reason.as_str().to_owned(),
            retry_after_unix: deferral.retry_after_unix,
        })?;
        let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW;
        let mode = Mode::RUSR | Mode::WUSR;
        let mut file = File::from(rustix::fs::openat(
            &dir,
            TEMP_FILE,
            flags | OFlags::CLOEXEC,
            mode,
        )?);
        file.write_all(&body)?;
        file.sync_all()?;
        rustix::fs::renameat(&dir, TEMP_FILE, &dir, DEFERRAL_FILE)?;
        Ok(rustix::fs::fsync(&dir)?)
    }

    /// Forgets the deferral of `service`, durably; none, or no step-record directory, is not an
    /// error.
    ///
    /// # Errors
    /// The directory cannot be used, or the file is there and cannot be removed.
    pub fn clear(&self, service: &ServiceName) -> io::Result<()> {
        let dir = match self.directory(service) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            opened => opened?,
        };
        remove(&dir, DEFERRAL_FILE)?;
        Ok(rustix::fs::fsync(&dir)?)
    }
}

/// Removes `name` from `dir`; a name that is not there is fine.
fn remove(dir: &File, name: &str) -> io::Result<()> {
    match rustix::fs::unlinkat(dir, name, AtFlags::empty()) {
        Ok(()) | Err(Errno::NOENT) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// One service's deferral as the loop knows it: none, or the active one.
struct Slot {
    service: ServiceName,
    deferral: Option<Deferral>,
}

/// The loop's deferrals: each service's file read once, then kept in memory and written at
/// every change; a write that failed is tried again by [`Book::flush`].
pub(crate) struct Book {
    files: Deferrals,
    known: BTreeMap<String, Slot>,
    /// The services whose file does not yet hold what memory holds.
    unwritten: BTreeSet<String>,
}

impl Book {
    pub(crate) fn new(state_dir: &Path) -> Book {
        Book {
            files: Deferrals::new(state_dir),
            known: BTreeMap::new(),
            unwritten: BTreeSet::new(),
        }
    }

    /// The deferral of `service` at `now`, capped at [`MAX_RETRY_SECS`] from now; a cap that
    /// lowers it is kept and written, so a far retry time cannot be read again as far. A file
    /// that cannot be used is logged, and removed by the next write.
    pub(crate) fn get(
        &mut self,
        service: &ServiceName,
        now: u64,
        log: &dyn Log,
    ) -> Option<Deferral> {
        let slot = match self.known.entry(service.as_str().to_owned()) {
            Entry::Occupied(slot) => slot.into_mut(),
            Entry::Vacant(slot) => {
                let deferral = match self.files.load(service) {
                    Ok(found) => found,
                    Err(e) => {
                        log.line(&format!(
                            "error: the deferral of {service} is not used and is removed: {e}"
                        ));
                        self.unwritten.insert(service.as_str().to_owned());
                        None
                    }
                };
                slot.insert(Slot {
                    service: service.clone(),
                    deferral,
                })
            }
        };
        let capped = slot.deferral.clone().map(|deferral| deferral.read_at(now));
        if capped != slot.deferral {
            slot.deferral = capped.clone();
            self.unwritten.insert(service.as_str().to_owned());
        }
        capped
    }

    /// Makes `deferral` the deferral of `service`, in place of any other.
    pub(crate) fn set(&mut self, service: &ServiceName, deferral: Deferral, log: &dyn Log) {
        self.put(service, Some(deferral), log);
    }

    /// Ends the deferral of `service` when it is one of `target`: a verdict on another target
    /// leaves it as it is.
    pub(crate) fn end(&mut self, service: &ServiceName, target: &Version, now: u64, log: &dyn Log) {
        let active = self.get(service, now, log);
        if active.is_some_and(|deferral| deferral.target == *target) {
            self.put(service, None, log);
        }
    }

    fn put(&mut self, service: &ServiceName, deferral: Option<Deferral>, log: &dyn Log) {
        self.known.insert(
            service.as_str().to_owned(),
            Slot {
                service: service.clone(),
                deferral,
            },
        );
        self.unwritten.insert(service.as_str().to_owned());
        self.flush(log);
    }

    /// Writes what memory holds and the files do not; a write that fails is logged and stays to
    /// be tried again.
    pub(crate) fn flush(&mut self, log: &dyn Log) {
        let mut written = Vec::new();
        for (key, slot) in &self.known {
            if !self.unwritten.contains(key) {
                continue;
            }
            let done = match &slot.deferral {
                Some(deferral) => self.files.save(&slot.service, deferral),
                None => self.files.clear(&slot.service),
            };
            match done {
                Ok(()) => written.push(key.clone()),
                Err(e) => log.line(&format!(
                    "error: the deferral of {} is not written: {e}",
                    slot.service
                )),
            }
        }
        for key in written {
            self.unwritten.remove(&key);
        }
    }
}
