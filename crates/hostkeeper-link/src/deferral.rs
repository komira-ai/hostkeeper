//! The deferral file: an update the host declined for a reason about its moment, and the unix
//! second before which the loop does not run that target again (design 5.2 step 6, 8.3).
//!
//! One file per service, [`DEFERRAL_FILE`] in the service's step-record directory
//! (`<state-dir>/<service>/`), beside the step records. The time is in unix seconds, because a
//! restart has no memory of the monotonic clock. The schema is the one kbf's agent gives its own
//! deferral file (`defer-<unit>` in its state directory), `{"target", "reason",
//! "retry_after_unix"}` and nothing else, so either program reads a file of the other's schema;
//! a key added later is ignored by both (formats change additively).
//!
//! The file is written beside, synced and renamed over, created new with mode 0600, and never
//! read or written through a link (`O_NOFOLLOW`).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use hostkeeper_config::names::{ServiceName, Version};
use serde::{Deserialize, Serialize};

use crate::names::Reason;
use crate::order::DeferredReport;

/// The file's name in the service's step-record directory.
pub const DEFERRAL_FILE: &str = "deferral.json";

/// The temporary file a write goes through.
const TEMP_FILE: &str = ".deferral.json.tmp";

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

    /// The deferral of `service` as read at `now` ([`Deferral::read_at`]); none when there is
    /// no file.
    ///
    /// # Errors
    /// The file is there and cannot be read, is a link, is longer than [`FILE_MAX`], or is no
    /// deferral.
    pub fn load(&self, service: &ServiceName, now: u64) -> Result<Option<Deferral>, DeferralError> {
        let path = self.path(service);
        let opened = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path);
        let file = match opened {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(DeferralError::Io { path, source }),
        };
        let mut bytes = Vec::new();
        if let Err(source) = file.take(FILE_MAX + 1).read_to_end(&mut bytes) {
            return Err(DeferralError::Io { path, source });
        }
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
                Some(
                    Deferral {
                        target,
                        reason,
                        retry_after_unix: text.retry_after_unix,
                    }
                    .read_at(now),
                )
            })
            .ok_or(DeferralError::Grammar { path })
    }

    /// Records `deferral` for `service`, durably: a new temporary file of mode 0600, synced,
    /// renamed over the file, and the directory synced. A temporary file left by a crash is
    /// removed first, never written through.
    ///
    /// # Errors
    /// The service's directory does not take it.
    pub fn save(&self, service: &ServiceName, deferral: &Deferral) -> io::Result<()> {
        let dir = self.dir.join(service.as_str());
        let temp = dir.join(TEMP_FILE);
        remove(&temp)?;
        let body = serde_json::to_vec(&OnDisk {
            target: deferral.target.as_str().to_owned(),
            reason: deferral.reason.as_str().to_owned(),
            retry_after_unix: deferral.retry_after_unix,
        })?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_NOFOLLOW)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(&body)?;
        file.sync_all()?;
        fs::rename(&temp, dir.join(DEFERRAL_FILE))?;
        File::open(&dir)?.sync_all()
    }

    /// Forgets the deferral of `service`; none is not an error.
    ///
    /// # Errors
    /// The file is there and cannot be removed.
    pub fn clear(&self, service: &ServiceName) -> io::Result<()> {
        remove(&self.path(service))
    }
}

/// Removes `path`; a path that is not there is fine.
fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}
