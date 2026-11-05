//! What is kept of a hook's output: the last [`RING_BYTES`] of each stream, and from the kept
//! standard output the last line, which is the hook's report, when it is at most
//! [`LAST_LINE_MAX`] bytes long.
//!
//! A hook may print without limit; the launcher reads both of its streams to their end, so
//! neither pipe fills and blocks the hook, and keeps a bounded tail of each
//! (docs/design/hostkeeper-design.md, section 11, item 11).

use std::collections::VecDeque;
use std::io;

/// How much of each stream is kept: its last 1 MiB.
pub const RING_BYTES: usize = 1024 * 1024;
/// The longest last line that is read as the report: 64 KiB, its line feed (and a carriage return
/// before it) not counted. A longer one is not read, and the step's report is `{}`.
pub const LAST_LINE_MAX: usize = 64 * 1024;

/// The last bytes written to it, at most its capacity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ring {
    kept: VecDeque<u8>,
    capacity: usize,
    /// Bytes were dropped from the front: the first kept line may be the end of a longer one.
    cut: bool,
}

impl Default for Ring {
    fn default() -> Ring {
        Ring::new()
    }
}

impl Ring {
    /// A ring of [`RING_BYTES`].
    pub fn new() -> Ring {
        Ring::with_capacity(RING_BYTES)
    }

    /// A ring that keeps the last `capacity` bytes.
    pub fn with_capacity(capacity: usize) -> Ring {
        Ring {
            kept: VecDeque::with_capacity(capacity),
            capacity,
            cut: false,
        }
    }

    /// Adds `bytes` at the end, dropping from the front what no longer fits.
    pub fn push(&mut self, bytes: &[u8]) {
        let tail = &bytes[bytes.len().saturating_sub(self.capacity)..];
        let over = (self.kept.len() + tail.len()).saturating_sub(self.capacity);
        self.cut |= over > 0 || tail.len() < bytes.len();
        self.kept.drain(..over);
        self.kept.extend(tail);
    }

    /// The kept bytes, oldest first.
    pub fn bytes(&self) -> Vec<u8> {
        self.kept.iter().copied().collect()
    }

    /// Whether bytes were dropped from the front.
    pub fn cut(&self) -> bool {
        self.cut
    }

    /// The last line that is not blank, as `hostkeeper_hook::Report::read` picks it from the whole
    /// output (lines end at a line feed, a carriage return before it is dropped, a line is blank
    /// when it is only white space, bytes that are not UTF-8 are replaced). `None` when there is
    /// none, when it is longer than [`LAST_LINE_MAX`] bytes, or when it is the first kept line of
    /// a ring that dropped bytes, so that its beginning is lost.
    pub fn last_line(&self) -> Option<String> {
        let bytes = self.bytes();
        let mut end = bytes.len();
        loop {
            let start = bytes[..end]
                .iter()
                .rposition(|b| *b == b'\n')
                .map(|at| at + 1);
            let first = start.is_none();
            let begin = start.unwrap_or(0);
            let raw = &bytes[begin..end];
            // As `str::lines`: a carriage return is dropped only before a line feed.
            let ended = end < bytes.len();
            let line = match raw.strip_suffix(b"\r") {
                Some(line) if ended => line,
                _ => raw,
            };
            let text = String::from_utf8_lossy(line);
            if !text.trim().is_empty() {
                let whole = !(first && self.cut);
                return (whole && line.len() <= LAST_LINE_MAX).then(|| text.into_owned());
            }
            if first {
                return None;
            }
            end = begin - 1;
        }
    }
}

/// A ring is a sink for `io::copy`, which reads a pipe into it to its end.
impl io::Write for Ring {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.push(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
