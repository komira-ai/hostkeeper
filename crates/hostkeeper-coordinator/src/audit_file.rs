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
//!
//! The file is never rotated, so a page of it is read without reading the rest ([`page_in`]):
//! [`CHUNK`] bytes at a time, keeping only the lines the page returns. The lines are in the
//! order of their numbers (each is appended under the lock that numbers it), so the first line
//! after a `since_seq` is found by bisecting the file's bytes, and the newest lines by reading
//! back from its end. A run of more than [`MAX_LINE`] bytes with no newline is passed over as a
//! line that does not parse; no line this store writes is near that long.

use std::cell::Cell;
use std::fs::{File, OpenOptions};
use std::io::{self, Write as _};
use std::ops::ControlFlow;
use std::os::unix::fs::{FileExt, OpenOptionsExt};
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

    /// The entry the line records, with its time.
    pub(crate) fn into_logged(self) -> Logged {
        Logged {
            at_unix: Some(self.at_unix),
            entry: AuditEntry {
                seq: self.seq,
                by: self.by,
                action: self.action,
                service: self.detail.service,
                node_id: self.detail.node_id,
                reason: self.detail.reason,
            },
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

/// An entry of the audit log, with the time its line in the audit file gives it: `None` for an
/// entry of the ring of a store with no audit file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Logged {
    pub(crate) entry: AuditEntry,
    pub(crate) at_unix: Option<u64>,
}

/// Which entries a page of the audit log holds: those numbered above `since_seq` that `keep`
/// keeps, at most `limit` of them (at least 1), the oldest of them or, with `newest`, the newest;
/// oldest first either way.
pub(crate) struct Query<'a> {
    pub(crate) since_seq: u64,
    pub(crate) limit: usize,
    pub(crate) newest: bool,
    pub(crate) keep: &'a dyn Fn(&AuditEntry) -> bool,
}

/// How many bytes one read of the audit file takes.
pub(crate) const CHUNK: usize = 16 * 1024;

/// The most bytes of a line that is read: a longer run with no newline is passed over, as a line
/// that does not parse would be.
pub(crate) const MAX_LINE: usize = 64 * 1024;

/// The line `raw` holds, if it parses; `None` for one longer than [`MAX_LINE`] (`raw` is `None`).
fn parse(raw: Option<&[u8]>) -> Option<AuditLine> {
    serde_json::from_slice(raw?).ok()
}

/// The audit file open for reading: its length when it was opened (a line appended since is
/// not read), and how many bytes have been read from it.
pub(crate) struct Reader {
    file: File,
    len: u64,
    read: Cell<u64>,
}

impl Reader {
    /// A reader of `file`, as long as it is now.
    ///
    /// # Errors
    /// The file's length could not be read.
    pub(crate) fn new(file: File) -> io::Result<Self> {
        let len = file.metadata()?.len();
        Ok(Self {
            file,
            len,
            read: Cell::new(0),
        })
    }

    /// How many bytes have been read from the file: what the tests bound.
    #[cfg(test)]
    pub(crate) fn bytes_read(&self) -> u64 {
        self.read.get()
    }

    /// The `len` bytes of the file from `at`.
    fn chunk(&self, at: u64, len: usize) -> io::Result<Vec<u8>> {
        let mut bytes = vec![0; len];
        self.file.read_exact_at(&mut bytes, at)?;
        self.read.set(self.read.get() + len as u64);
        Ok(bytes)
    }
}

/// What [`forward`] hands each line to: its offset, and its bytes or `None` for one longer than
/// [`MAX_LINE`]. Breaking stops the read.
type Forward<'a> = &'a mut dyn FnMut(u64, Option<&[u8]>) -> ControlFlow<()>;

/// What [`backward`] hands each line to, as [`Forward`] without the offset.
type Backward<'a> = &'a mut dyn FnMut(Option<&[u8]>) -> ControlFlow<()>;

/// How many bytes to read next: [`CHUNK`], or the `left` bytes there are when fewer.
fn take(left: u64) -> usize {
    usize::try_from(left).map_or(CHUNK, |left| left.min(CHUNK))
}

/// A line being put together from the chunks it spans: its bytes, or `None` once it has grown
/// past [`MAX_LINE`].
struct Partial(Option<Vec<u8>>);

impl Partial {
    fn new() -> Self {
        Self(Some(Vec::new()))
    }

    /// Adds `bytes` after (`back` false) or before (`back` true) what it holds.
    fn add(&mut self, bytes: &[u8], back: bool) {
        if let Some(line) = &mut self.0 {
            if line.len() + bytes.len() > MAX_LINE {
                self.0 = None;
            } else if back {
                line.splice(0..0, bytes.iter().copied());
            } else {
                line.extend_from_slice(bytes);
            }
        }
    }

    /// The line, and a fresh start for the next one; `None` for an empty line, which is no line.
    fn finish(&mut self) -> Option<Option<Vec<u8>>> {
        let done = self.0.replace(Vec::new());
        match done {
            Some(line) if line.is_empty() => None,
            done => Some(done),
        }
    }
}

/// Visits, in order, the lines of `file` that start at or after byte `from`, which is a line's
/// start: each by its offset and its bytes without the newline, or `None` for one longer than
/// [`MAX_LINE`]. A last line with no newline is visited too. Empty lines are not visited. Stops
/// when `visit` breaks.
fn forward(file: &Reader, from: u64, visit: Forward<'_>) -> io::Result<()> {
    let len = file.len;
    let mut line = Partial::new();
    let mut start = from;
    let mut at = from;
    while at < len {
        let bytes = file.chunk(at, take(len - at))?;
        let mut rest = &bytes[..];
        let mut offset = at;
        while let Some(end) = rest.iter().position(|byte| *byte == b'\n') {
            line.add(&rest[..end], false);
            if let Some(done) = line.finish()
                && visit(start, done.as_deref()).is_break()
            {
                return Ok(());
            }
            offset += end as u64 + 1;
            start = offset;
            rest = &rest[end + 1..];
        }
        line.add(rest, false);
        at += bytes.len() as u64;
    }
    if let Some(done) = line.finish() {
        let _ = visit(start, done.as_deref());
    }
    Ok(())
}

/// Visits the lines of `file`, newest first, read back from the end a chunk at a time; as
/// [`forward`] does, without the offsets.
fn backward(file: &Reader, visit: Backward<'_>) -> io::Result<()> {
    let mut line = Partial::new();
    let mut at = file.len;
    while at > 0 {
        let size = take(at);
        at -= size as u64;
        let bytes = file.chunk(at, size)?;
        let mut rest = &bytes[..];
        while let Some(newline) = rest.iter().rposition(|byte| *byte == b'\n') {
            line.add(&rest[newline + 1..], true);
            if let Some(done) = line.finish()
                && visit(done.as_deref()).is_break()
            {
                return Ok(());
            }
            rest = &rest[..newline];
        }
        line.add(rest, true);
    }
    if let Some(done) = line.finish() {
        let _ = visit(done.as_deref());
    }
    Ok(())
}

/// The seq of the first line of `file` that starts at or after byte `at` (at least 1) and before
/// `before` and parses, with the offset just past its newline; `None` if there is none.
fn line_from(file: &Reader, at: u64, before: u64) -> io::Result<Option<(u64, u64)>> {
    let mut found = None;
    let mut first = true;
    // From the byte before `at`: the part of the line that runs into `at` is the first visited
    // (an empty one, not visited, when a line starts at `at`), and is not a line from `at`.
    forward(file, at - 1, &mut |start, raw| {
        if std::mem::take(&mut first) && start < at {
            return ControlFlow::Continue(());
        }
        if start >= before {
            return ControlFlow::Break(());
        }
        match parse(raw) {
            Some(line) => {
                found = Some((line.seq, start + raw.map_or(0, <[u8]>::len) as u64 + 1));
                ControlFlow::Break(())
            }
            None => ControlFlow::Continue(()),
        }
    })?;
    Ok(found)
}

/// A line's start in `file` before which every line that parses is numbered `since_seq` or
/// below: found by bisection, reading about a chunk at each step, until what is left to search
/// is one chunk.
fn start_after(file: &Reader, since_seq: u64) -> io::Result<u64> {
    let (mut low, mut high) = (0, file.len);
    while high.saturating_sub(low) > CHUNK as u64 {
        let middle = low + (high - low) / 2;
        match line_from(file, middle, high)? {
            Some((seq, next)) if seq <= since_seq => low = next,
            _ => high = middle,
        }
    }
    Ok(low)
}

/// The page of `file` that `query` asks for. Memory holds the page, a chunk and a line; the
/// bytes read are about a chunk for each line of the page that spans one, plus, for the oldest
/// entries after a `since_seq`, about a chunk for each halving of the file. A filter that few
/// entries pass reads on until the page is full or the file ends.
///
/// # Errors
/// The file could not be read.
pub(crate) fn page_in(file: &Reader, query: &Query<'_>) -> io::Result<Vec<Logged>> {
    let mut page = Vec::new();
    let mut add = |logged: Logged| {
        if (query.keep)(&logged.entry) {
            page.push(logged);
        }
        if page.len() >= query.limit {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    if query.newest {
        backward(file, &mut |raw| match parse(raw) {
            Some(line) if line.seq <= query.since_seq => ControlFlow::Break(()),
            Some(line) => add(line.into_logged()),
            None => ControlFlow::Continue(()),
        })?;
        page.reverse();
    } else {
        let from = start_after(file, query.since_seq)?;
        forward(file, from, &mut |_, raw| match parse(raw) {
            Some(line) if line.seq > query.since_seq => add(line.into_logged()),
            _ => ControlFlow::Continue(()),
        })?;
    }
    Ok(page)
}

/// The audit file at `path`, open for reading; `None` if there is no file.
fn open_read(path: &Path) -> Result<Option<Reader>, OpsError> {
    let io_error = |source| StoreError::Io {
        path: path.to_owned(),
        source,
    };
    match File::open(path).and_then(Reader::new) {
        Ok(reader) => Ok(Some(reader)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(io_error(source).into()),
    }
}

/// The page of the audit file at `path` that `query` asks for ([`page_in`]); none if there is
/// no file.
///
/// # Errors
/// [`OpsError::Io`]: the file exists and cannot be read.
pub(crate) fn page(path: &Path, query: &Query<'_>) -> Result<Vec<Logged>, OpsError> {
    let Some(file) = open_read(path)? else {
        return Ok(Vec::new());
    };
    page_in(&file, query).map_err(|source| {
        StoreError::Io {
            path: path.to_owned(),
            source,
        }
        .into()
    })
}

/// Visits every line of the audit file at `path` that parses, in file order, a chunk at a time,
/// and answers how many lines did not parse (an empty line is not counted); none if there is no
/// file.
///
/// # Errors
/// [`OpsError::Io`]: the file exists and cannot be read.
pub(crate) fn each(path: &Path, visit: &mut dyn FnMut(AuditLine)) -> Result<usize, OpsError> {
    let Some(file) = open_read(path)? else {
        return Ok(0);
    };
    let mut skipped = 0;
    forward(&file, 0, &mut |_, raw| {
        match parse(raw) {
            Some(line) => visit(line),
            None => skipped += 1,
        }
        ControlFlow::Continue(())
    })
    .map_err(|source| StoreError::Io {
        path: path.to_owned(),
        source,
    })?;
    Ok(skipped)
}

/// What reading the whole audit file found, for the tests.
#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct AuditRead {
    /// The lines that parse, in file order.
    pub(crate) lines: Vec<AuditLine>,
    /// How many lines did not parse (an empty line is not counted).
    pub(crate) skipped: usize,
    /// Whether the file ends without a newline: its last line was cut short.
    pub(crate) torn: bool,
}

/// Every line of the audit file at `path` ([`each`]), for the tests; none if there is no file.
///
/// # Errors
/// [`OpsError::Io`]: the file exists and cannot be read.
#[cfg(test)]
pub(crate) fn read(path: &Path) -> Result<AuditRead, OpsError> {
    let mut lines = Vec::new();
    let skipped = each(path, &mut |line| lines.push(line))?;
    let torn = open_read(path)?.is_some_and(|file| ends_torn(&file.file).unwrap_or(false));
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
        let mut last_seq = 0;
        let skipped = each(&path, &mut |line| last_seq = last_seq.max(line.seq))?;
        if skipped > 0 {
            tracing::warn!(
                path = %path.display(),
                skipped,
                "lines of the audit file this build cannot read were passed over"
            );
        }
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

#[cfg(test)]
#[path = "audit_read_tests.rs"]
mod tests;
