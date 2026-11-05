//! The audit file: every change the ops store makes is also appended, as one JSON line, to
//! `<ops-file>.audit`.
//!
//! The ops file keeps the last [`crate::ops::AUDIT_RING`] changes, which `/v1/status` shows; this
//! file keeps all of them, with the time. A line is the five keys, in this order,
//! `{"seq":1,"at_unix":1800000000,"by":"alice","action":"exclude","detail":{"service":"demo",
//! "node_id":"s2","reason":"bad disk"}}`, then a newline. `by` is who made the change (the name
//! of the operator's token, `coordinator` or `ci`), `at_unix` the seconds since the epoch, and
//! `detail` the change's `service`, `node_id` and `reason`, those it has, in that order (`{}` when
//! it has none).
//!
//! The file is opened for appending (`O_APPEND`, created with mode `0600`), each line is written
//! in one call and fsynced, and a file that was just created has its directory fsynced; nothing
//! rewrites it. A line the file's reader cannot parse, which a crash in the middle of an append
//! leaves as the last line, is passed over and counted. Before each append the file's last byte
//! is read: when it is not a newline (a crash, or an append that failed part-way while the store
//! runs), the next line starts with one, so it never joins the torn one. The store reads the file when
//! it opens, for the number of the last line: numbers continue from the larger of the ring's last
//! and this file's last, so they do not repeat when the ops file is moved aside.
//!
//! The file is a second copy of what the ring holds, not a gate: a line that cannot be appended
//! is logged as an error and the change stands ([`crate::ops::OpsStore`]).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::{FileExt as _, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::desired::{StoreError, dir_of};
use crate::ops::{AuditEntry, OpsError};

/// Seconds since the epoch, for the `at_unix` of the audit file's lines.
pub(crate) type UnixClock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// The wall clock, for the `at_unix` of a line: when a change was made, for the people who read
/// the file. No decision reads it; the coordinator decides by its own monotonic clock.
#[must_use]
pub(crate) fn wall_clock() -> UnixClock {
    Arc::new(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs())
    })
}

/// The audit file of the ops file at `ops`: its name with `.audit` added.
#[must_use]
pub(crate) fn path_of(ops: &Path) -> PathBuf {
    let mut path = ops.as_os_str().to_owned();
    path.push(".audit");
    PathBuf::from(path)
}

/// What a change concerned: the keys it has.
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Detail {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) service: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
}

/// One line of the audit file.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AuditLine {
    pub(crate) seq: u64,
    pub(crate) at_unix: u64,
    pub(crate) by: String,
    pub(crate) action: String,
    pub(crate) detail: Detail,
}

impl AuditLine {
    /// The line for `entry`, made at `at_unix`.
    pub(crate) fn of(entry: &AuditEntry, at_unix: u64) -> Self {
        Self {
            seq: entry.seq,
            at_unix,
            by: entry.by.clone(),
            action: entry.action.clone(),
            detail: Detail {
                service: entry.service.clone(),
                node_id: entry.node_id.clone(),
                reason: entry.reason.clone(),
            },
        }
    }

    /// The entry the line records, without its time.
    pub(crate) fn into_entry(self) -> AuditEntry {
        AuditEntry {
            seq: self.seq,
            by: self.by,
            action: self.action,
            service: self.detail.service,
            node_id: self.detail.node_id,
            reason: self.detail.reason,
        }
    }

    /// The line as the file holds it, with its newline, and a newline before it when the file's
    /// last line is `torn`. (A line of strings and numbers always serialises, so the empty text
    /// is never written.)
    fn text_after(&self, torn: bool) -> String {
        let mut text = String::from(if torn { "\n" } else { "" });
        text.push_str(&serde_json::to_string(self).unwrap_or_default());
        text.push('\n');
        text
    }
}

/// What reading the audit file found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct AuditRead {
    /// The lines that parse, in file order.
    pub(crate) lines: Vec<AuditLine>,
    /// How many lines did not parse (an empty line is not counted).
    pub(crate) skipped: usize,
    /// Whether the file ends without a newline: its last line was cut short.
    pub(crate) torn: bool,
}

/// The lines of the audit file at `path`; none if there is no file.
///
/// # Errors
/// [`OpsError::Io`]: the file exists and cannot be read.
pub(crate) fn read(path: &Path) -> Result<AuditRead, OpsError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(source) => {
            return Err(StoreError::Io {
                path: path.to_owned(),
                source,
            }
            .into());
        }
    };
    let torn = bytes.last().is_some_and(|last| *last != b'\n');
    let mut lines = Vec::new();
    let mut skipped = 0;
    for raw in bytes
        .split(|byte| *byte == b'\n')
        .filter(|raw| !raw.is_empty())
    {
        match serde_json::from_slice::<AuditLine>(raw) {
            Ok(line) => lines.push(line),
            Err(_) => skipped += 1,
        }
    }
    Ok(AuditRead {
        lines,
        skipped,
        torn,
    })
}

/// The audit file of one ops file.
pub(crate) struct AuditLog {
    path: PathBuf,
    /// The number of the last line the file holds; the lock also keeps appends one at a time.
    last_seq: Mutex<u64>,
}

/// Whether `file` ends in a byte other than a newline: its last line was cut short. The file
/// itself is read, not what the store remembers, so an append that failed part-way is seen.
fn ends_torn(file: &File) -> io::Result<bool> {
    file.metadata().and_then(|meta| match meta.len() {
        0 => Ok(false),
        len => {
            let mut last = [0_u8];
            file.read_exact_at(&mut last, len - 1)
                .map(|()| last[0] != b'\n')
        }
    })
}

/// Syncs the directory of the file at `path`: a file just created is found after a crash only
/// once its directory entry is on disk.
fn sync_dir(path: &Path) -> io::Result<()> {
    File::open(dir_of(path)).and_then(|dir| dir.sync_all())
}

impl AuditLog {
    /// The log of the file at `path`: its last number is read now. Nothing is created until the
    /// first line is appended.
    ///
    /// # Errors
    /// [`OpsError::Io`]: the file exists and cannot be read.
    pub(crate) fn open(path: PathBuf) -> Result<Self, OpsError> {
        let found = read(&path)?;
        if found.skipped > 0 {
            tracing::warn!(
                path = %path.display(),
                skipped = found.skipped,
                "lines of the audit file this build cannot read were passed over"
            );
        }
        let last_seq = found.lines.iter().map(|line| line.seq).max().unwrap_or(0);
        Ok(Self {
            path,
            last_seq: Mutex::new(last_seq),
        })
    }

    fn lock(&self) -> MutexGuard<'_, u64> {
        self.last_seq.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The file.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// The number of the last line the file holds: 0 when it holds none.
    pub(crate) fn last_seq(&self) -> u64 {
        *self.lock()
    }

    /// Appends `line`, fsynced. The file is opened for appending each time, created with mode
    /// `0600` the first time (and its directory synced); when its last byte is not a newline
    /// ([`ends_torn`]) the line starts with one.
    ///
    /// # Errors
    /// The file could not be opened, read, written or synced. The line may be in the file in
    /// part; the next append starts on a line of its own.
    pub(crate) fn append(&self, line: &AuditLine) -> io::Result<()> {
        let mut last_seq = self.lock();
        let fresh = !self.path.exists();
        let appended = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .mode(0o600)
            .open(&self.path)
            .and_then(|mut file| {
                ends_torn(&file)
                    .map(|torn| line.text_after(torn))
                    .and_then(|text| file.write_all(text.as_bytes()))
                    .and_then(|()| file.sync_all())
            })
            .and_then(|()| if fresh { sync_dir(&self.path) } else { Ok(()) });
        if appended.is_ok() {
            *last_seq = (*last_seq).max(line.seq);
        }
        appended
    }
}
