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
//!
//! A page of the audit is read without reading the rest ([`page_in`], [`AuditLog::page`]):
//! [`CHUNK`] bytes at a time, keeping only the lines the page returns. The lines are in the
//! order of their numbers, within a file and from segment to segment (each is appended under the
//! lock that numbers it), so the first line after a `since_seq` is found by bisecting a file's
//! bytes, and the newest lines by reading back from the end of the newest file. A run of more
//! than [`MAX_LINE`] bytes with no newline is passed over as a line that does not parse; no line
//! this store writes is near that long. Every other read (the last number at start, every line
//! for the approvals check) also goes a chunk at a time.

use std::cell::Cell;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::num::{NonZeroU64, NonZeroUsize};
use std::ops::ControlFlow;
use std::os::unix::fs::{FileExt, OpenOptionsExt};
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

/// What reading the audit file found, for the tests.
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

/// The lines of the one file at `path`, read whole, for the tests; none if there is no file.
///
/// # Errors
/// [`OpsError::Io`]: the file exists and cannot be read.
#[cfg(test)]
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
#[derive(Clone, Copy)]
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

/// Visits every line of the one file at `path` that parses, in file order, a chunk at a time,
/// and answers how many lines did not parse (an empty line is not counted); none if there is no
/// file.
///
/// # Errors
/// [`OpsError::Io`]: the file exists and cannot be read.
fn each_in(path: &Path, visit: &mut dyn FnMut(AuditLine)) -> Result<usize, OpsError> {
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
#[cfg(test)]
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
        let mut first = None;
        each_in(&file, &mut |line| {
            first.get_or_insert(line.at_unix);
        })?;
        if first.is_some() {
            return Ok(first);
        }
    }
    Ok(None)
}

/// The page of the audit file at `active` and its sealed segments that `query` asks for: each
/// file's part read by [`page_in`], the oldest entries from the oldest file on, the newest from
/// the newest file back, until the page is full. A file with no line after `since_seq` costs
/// about a chunk for each halving of it (oldest) or one chunk (newest).
///
/// # Errors
/// [`OpsError::Io`]: the directory cannot be listed, or a file of it cannot be read.
fn page(active: &Path, query: &Query<'_>) -> Result<Vec<Logged>, OpsError> {
    let mut files = files_of(active)?;
    if query.newest {
        files.reverse();
    }
    let mut parts = Vec::new();
    let mut held = 0;
    for path in files {
        if held >= query.limit {
            break;
        }
        let Some(file) = open_read(&path)? else {
            continue;
        };
        let part = page_in(
            &file,
            &Query {
                limit: query.limit - held,
                ..*query
            },
        )
        .map_err(|source| StoreError::Io {
            path: path.clone(),
            source,
        })?;
        held += part.len();
        parts.push(part);
    }
    if query.newest {
        parts.reverse();
    }
    Ok(parts.into_iter().flatten().collect())
}

/// Visits every line of the audit file at `active` and its sealed segments that parses, oldest
/// first, a chunk at a time.
///
/// # Errors
/// [`OpsError::Io`]: the directory cannot be listed, or a file of it cannot be read.
fn each(active: &Path, visit: &mut dyn FnMut(AuditLine)) -> Result<(), OpsError> {
    for file in files_of(active)? {
        each_in(&file, visit)?;
    }
    Ok(())
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
        let mut last_seq = None;
        for file in files_of(&path)?.iter().rev() {
            let mut last = None;
            let skipped = each_in(file, &mut |line| {
                last = Some(last.map_or(line.seq, |seq: u64| seq.max(line.seq)));
            })?;
            if skipped > 0 {
                tracing::warn!(
                    path = %file.display(),
                    skipped,
                    "lines of the audit file this build cannot read were passed over"
                );
            }
            if last.is_some() {
                last_seq = last;
                break;
            }
        }
        let last_seq = last_seq.unwrap_or(0);
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
    #[cfg(test)]
    pub(crate) fn read_after(&self, since_seq: u64) -> Result<AuditRead, OpsError> {
        let _appends = self.lock();
        read_after(&self.path, since_seq)
    }

    /// The page `query` asks for of the files of this log ([`page`]), under the lock that keeps
    /// appends one at a time, so no rotation seals the active file between the listing of the
    /// files and their read.
    ///
    /// # Errors
    /// [`OpsError::Io`], as [`page`].
    pub(crate) fn page(&self, query: &Query<'_>) -> Result<Vec<Logged>, OpsError> {
        let _appends = self.lock();
        page(&self.path, query)
    }

    /// Visits every line of the files of this log ([`each`]), oldest first, under the lock that
    /// keeps appends one at a time.
    ///
    /// # Errors
    /// [`OpsError::Io`], as [`each`].
    pub(crate) fn each(&self, visit: &mut dyn FnMut(AuditLine)) -> Result<(), OpsError> {
        let _appends = self.lock();
        each(&self.path, visit)
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

#[cfg(test)]
#[path = "audit_read_tests.rs"]
mod tests;
