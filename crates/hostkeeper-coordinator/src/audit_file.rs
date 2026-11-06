//! The audit file: every change the ops store makes is also appended, as one JSON line, to
//! `<ops-file>.audit`.
//!
//! The ops file keeps the last [`crate::ops::AUDIT_RING`] changes, which `/v1/status` shows; this
//! file keeps all of them, with the time, up to its retention (below). A line is the five keys, in
//! this order, `{"seq":1,"at_unix":1800000000,"by":"alice","action":"exclude","detail":{"service":
//! "demo","node_id":"s2","reason":"bad disk"}}`, then a newline. `by` is who made the change (the
//! name of the operator's token, `coordinator` or `ci`), `at_unix` the seconds since the epoch,
//! and `detail` the change's `service`, `node_id` and `reason`, those it has, in that order (`{}`
//! when it has none).
//!
//! The file is opened for appending (`O_APPEND`, created with mode `0600`), each line is written
//! in one call and fsynced, and a file that was just created has its directory fsynced; nothing
//! rewrites it. A line the file's reader cannot parse, which a crash in the middle of an append
//! leaves as the last line, is passed over and counted. Before each append the file's last byte
//! is read: when it is not a newline (a crash, or an append that failed part-way while the store
//! runs), the next line starts with one, so it never joins the torn one.
//!
//! The file is rotated by size ([`Rotation`], `--audit-max-bytes` and
//! `--audit-retained-segments`). When a line would take a file that is not empty past the
//! maximum, the file is sealed: renamed to `<ops-file>.audit.<n>`, `n` one past the newest
//! segment's (1 for the first), and the directory fsynced; the line then starts a fresh
//! `<ops-file>.audit`. A line is never split across files and a sealed segment is never written
//! again. At each rotation the oldest segments beyond the retained count are deleted. The numbers
//! go on across files: the store reads, when it opens, the last number of the newest file that
//! holds a line, and numbers continue from the larger of the ring's last and that one, so they do
//! not repeat when the ops file is moved aside or a rotation has just left the active file empty.
//!
//! The file is the durable record of every change: a change that lowers protection or steers the
//! rollout is not made when its line cannot be appended ([`crate::ops::OpsStore`]).

use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::num::{NonZeroU64, NonZeroUsize};
use std::os::unix::fs::{FileExt as _, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::desired::{StoreError, dir_of};
use crate::ops::{AuditEntry, OpsError};

/// What appends a line to the audit: [`AuditLog::append`] in a store over a file. A test puts
/// one in its place that fails, to see what a change does when its line cannot be appended.
pub(crate) type Appender = Box<dyn Fn(&AuditLine) -> io::Result<()> + Send + Sync>;

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

/// How the audit file is rotated ([`DEFAULT_ROTATION`] unless the flags say otherwise).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Rotation {
    /// The most bytes the active file holds: a line that would take a non-empty file past it
    /// goes to a fresh file, and the full one is sealed. A line longer than this is written
    /// whole, alone in its file.
    pub(crate) max_bytes: NonZeroU64,
    /// How many sealed segments are kept: at a rotation, the oldest beyond it are deleted.
    pub(crate) retained: NonZeroUsize,
}

/// The default of `--audit-max-bytes`: 16 MiB, some 80 000 lines.
pub(crate) const DEFAULT_MAX_BYTES: NonZeroU64 = NonZeroU64::MIN.saturating_add((16 << 20) - 1);

/// The default of `--audit-retained-segments`: 8, so the audit holds at most 144 MiB.
pub(crate) const DEFAULT_RETAINED: NonZeroUsize = NonZeroUsize::MIN.saturating_add(7);

/// The rotation of a store whose flags say nothing of it.
pub(crate) const DEFAULT_ROTATION: Rotation = Rotation {
    max_bytes: DEFAULT_MAX_BYTES,
    retained: DEFAULT_RETAINED,
};

/// The sealed segment `number` of the audit file at `active`: its name with `.<number>` added.
#[must_use]
pub(crate) fn segment_path(active: &Path, number: u64) -> PathBuf {
    let mut path = active.as_os_str().to_owned();
    path.push(format!(".{number}"));
    PathBuf::from(path)
}

/// The number of the sealed segment named `name`, if it is one of the audit file named `active`:
/// `active`, a dot, and a number with no leading zero.
fn segment_number(active: &OsStr, name: &OsStr) -> Option<u64> {
    let digits = name
        .as_encoded_bytes()
        .strip_prefix(active.as_encoded_bytes())?
        .strip_prefix(b".")?;
    let canonical =
        digits.first().is_some_and(|first| *first != b'0') && digits.iter().all(u8::is_ascii_digit);
    if !canonical {
        return None;
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
}

/// The sealed segments of the audit file at `active`, oldest (lowest number) first; none when
/// its directory does not exist.
///
/// # Errors
/// The directory exists and cannot be listed.
pub(crate) fn segments(active: &Path) -> io::Result<Vec<(u64, PathBuf)>> {
    let entries = match fs::read_dir(dir_of(active)) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let name = active.file_name().unwrap_or_default();
    let mut found: Vec<(u64, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| segment_number(name, &entry.file_name()))
        .map(|number| (number, segment_path(active, number)))
        .collect();
    found.sort_unstable();
    Ok(found)
}

/// Every file of the audit file at `active`, oldest first: its sealed segments, then itself.
fn files_of(active: &Path) -> Result<Vec<PathBuf>, OpsError> {
    let listed = segments(active).map_err(|source| StoreError::Io {
        path: dir_of(active).to_owned(),
        source,
    })?;
    let mut files: Vec<PathBuf> = listed.into_iter().map(|(_, path)| path).collect();
    files.push(active.to_owned());
    Ok(files)
}

/// The lines of the audit file at `active` and of its sealed segments after `since_seq`, oldest
/// first, with the lines that did not parse counted. Files are read newest first, and a file
/// that holds a line at or before `since_seq` is the last read: the numbers rise from file to
/// file, so no older one holds a line after it. `torn` is whether the active file is.
///
/// # Errors
/// [`OpsError::Io`]: the directory cannot be listed, or a file of it cannot be read.
pub(crate) fn read_after(active: &Path, since_seq: u64) -> Result<AuditRead, OpsError> {
    let mut newest_first = Vec::new();
    let mut skipped = 0;
    let mut torn = None;
    for file in files_of(active)?.iter().rev() {
        let found = read(file)?;
        skipped += found.skipped;
        torn.get_or_insert(found.torn);
        let reached = found.lines.iter().any(|line| line.seq <= since_seq);
        newest_first.push(found.lines);
        if reached {
            break;
        }
    }
    let lines = newest_first
        .into_iter()
        .rev()
        .flatten()
        .filter(|line| line.seq > since_seq)
        .collect();
    Ok(AuditRead {
        lines,
        skipped,
        torn: torn.unwrap_or(false),
    })
}

/// When segments of the audit file at `active` were deleted (the oldest kept is not segment 1),
/// the time of the oldest line kept; `None` when nothing was deleted, or nothing kept is a line.
///
/// # Errors
/// [`OpsError::Io`]: the directory cannot be listed, or a file of it cannot be read.
pub(crate) fn pruned_before(active: &Path) -> Result<Option<u64>, OpsError> {
    let pruned = segments(active)
        .map_err(|source| StoreError::Io {
            path: dir_of(active).to_owned(),
            source,
        })?
        .first()
        .is_some_and(|(number, _)| *number > 1);
    if !pruned {
        return Ok(None);
    }
    for file in files_of(active)? {
        if let Some(first) = read(&file)?.lines.first() {
            return Ok(Some(first.at_unix));
        }
    }
    Ok(None)
}

/// The audit file of one ops file, with its sealed segments.
pub(crate) struct AuditLog {
    path: PathBuf,
    rotation: Rotation,
    /// The number of the last line the files hold; the lock also keeps appends one at a time.
    last_seq: Mutex<u64>,
}

/// Whether `file`, `len` bytes long, ends in a byte other than a newline: its last line was cut
/// short. The file itself is read, not what the store remembers, so an append that failed
/// part-way is seen.
fn ends_torn(file: &File, len: u64) -> io::Result<bool> {
    match len {
        0 => Ok(false),
        len => {
            let mut last = [0_u8];
            file.read_exact_at(&mut last, len - 1)
                .map(|()| last[0] != b'\n')
        }
    }
}

/// Syncs the directory of the file at `path`: a file just created or renamed is found after a
/// crash only once its directory entry is on disk.
fn sync_dir(path: &Path) -> io::Result<()> {
    File::open(dir_of(path)).and_then(|dir| dir.sync_all())
}

impl AuditLog {
    /// The log of the file at `path`, rotated by `rotation`. Its last number is read now, from
    /// the newest of its files that holds a line: the active file, else the newest sealed
    /// segment that does (the active file is empty, or not there, right after a rotation).
    /// Nothing is created until the first line is appended.
    ///
    /// # Errors
    /// [`OpsError::Io`]: the directory cannot be listed, or a file of it cannot be read.
    pub(crate) fn open(path: PathBuf, rotation: Rotation) -> Result<Self, OpsError> {
        let mut last_seq = 0;
        for file in files_of(&path)?.iter().rev() {
            let found = read(file)?;
            if found.skipped > 0 {
                tracing::warn!(
                    path = %file.display(),
                    skipped = found.skipped,
                    "lines of the audit file this build cannot read were passed over"
                );
            }
            if let Some(last) = found.lines.iter().map(|line| line.seq).max() {
                last_seq = last;
                break;
            }
        }
        Ok(Self {
            path,
            rotation,
            last_seq: Mutex::new(last_seq),
        })
    }

    fn lock(&self) -> MutexGuard<'_, u64> {
        self.last_seq.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The active file.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// [`read_after`] of the files of this log, under the lock that keeps appends one at a
    /// time: no rotation seals the active file between the listing of the files and their read,
    /// which would leave the segment it made out of the answer.
    ///
    /// # Errors
    /// [`OpsError::Io`], as [`read_after`].
    pub(crate) fn read_after(&self, since_seq: u64) -> Result<AuditRead, OpsError> {
        let _appends = self.lock();
        read_after(&self.path, since_seq)
    }

    /// The number of the last line the files hold: 0 when they hold none.
    pub(crate) fn last_seq(&self) -> u64 {
        *self.lock()
    }

    /// Appends `line`, fsynced, to the active file. The file is opened for appending each time,
    /// created with mode `0600` the first time (and its directory synced); when its last byte
    /// is not a newline ([`ends_torn`]) the line starts with one. When the file is not empty and
    /// the line would take it past the rotation's `max_bytes`, the file is sealed first
    /// ([`Self::rotate`]) and the line starts a fresh one: a line is never split across files.
    ///
    /// # Errors
    /// A file could not be opened, read, renamed, written or synced. The line may be in the
    /// file in part; the next append starts on a line of its own.
    pub(crate) fn append(&self, line: &AuditLine) -> io::Result<()> {
        let mut last_seq = self.lock();
        let appended = self.append_locked(line);
        if appended.is_ok() {
            *last_seq = (*last_seq).max(line.seq);
        }
        appended
    }

    /// [`Self::append`], under its lock.
    fn append_locked(&self, line: &AuditLine) -> io::Result<()> {
        let fresh = !self.path.exists();
        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .mode(0o600)
            .open(&self.path)?;
        let len = file.metadata()?.len();
        let text = line.text_after(ends_torn(&file, len)?);
        let past = len.saturating_add(text.len() as u64) > self.rotation.max_bytes.get();
        if len > 0 && past {
            drop(file);
            self.rotate()?;
            return self.append_locked(line);
        }
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        if fresh { sync_dir(&self.path) } else { Ok(()) }
    }

    /// Seals the active file: renames it to the segment numbered one past the newest
    /// ([`segment_path`]) and syncs the directory, then deletes the oldest segments beyond the
    /// rotation's `retained`. A segment that cannot be deleted is logged and left; the next
    /// rotation tries again.
    ///
    /// # Errors
    /// The directory could not be listed or synced, or the file could not be renamed.
    fn rotate(&self) -> io::Result<()> {
        let mut kept = segments(&self.path)?;
        let number = kept.last().map_or(1, |(newest, _)| newest + 1);
        let sealed = segment_path(&self.path, number);
        fs::rename(&self.path, &sealed)?;
        sync_dir(&self.path)?;
        kept.push((number, sealed));
        let surplus = kept.len().saturating_sub(self.rotation.retained.get());
        for (_, old) in kept.drain(..surplus) {
            if let Err(why) = fs::remove_file(&old) {
                tracing::warn!(
                    target: "hostkeeper_coordinator::audit",
                    path = %old.display(),
                    %why,
                    "a sealed segment of the audit file past the retained count could not be deleted"
                );
            }
        }
        Ok(())
    }
}
