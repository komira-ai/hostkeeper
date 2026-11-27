//! The hardware event log over files ([`crate::hardware_log`]): one file beside the state file,
//! `<state-file>.hardware-events`, holding one record per line as JSON ([`Record`]), in the order
//! they were written. A write appends the lines of the records the file does not hold yet, by host
//! and `event_id`, in one write, and syncs the file before it answers; nothing is ever rewritten
//! but a torn last line. A file that does not exist is an empty log.
//!
//! The file is read once, at the first call, and kept in memory. A last line with no line break
//! is a write a crash cut short, never answered as written: it is cut off the file then. Any other
//! line that is not a record fails the call, naming the line, until the file is fixed; the
//! coordinator does not guess what a log held. A write that fails leaves the file to be read again
//! at the next call, so a line it left torn is cut off before another is appended.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use crate::hardware_log::{HardwareLog, Held, Query, Record};

/// The file of the log beside the state file at `state`: `<state>.hardware-events`.
pub(crate) fn hardware_events_path(state: &Path) -> PathBuf {
    let mut path = state.as_os_str().to_owned();
    path.push(".hardware-events");
    PathBuf::from(path)
}

/// The log in one file, and what it holds once read.
#[derive(Debug)]
pub(crate) struct FileLog {
    path: PathBuf,
    held: Mutex<Option<Held>>,
}

impl FileLog {
    /// The log kept in the file at `path`.
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            held: Mutex::new(None),
        }
    }

    /// `why`, naming the file.
    fn at(&self, why: impl std::fmt::Display) -> String {
        format!("{}: {why}", self.path.display())
    }

    /// What the file holds, its torn last line cut off.
    fn read(&self) -> Result<Held, String> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(why) if why.kind() == io::ErrorKind::NotFound => return Ok(Held::default()),
            Err(why) => return Err(self.at(why)),
        };
        let whole = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |n| n + 1);
        if whole < bytes.len() {
            self.cut(whole).map_err(|why| self.at(why))?;
        }
        let mut held = Held::default();
        for (n, line) in bytes[..whole].split_inclusive(|b| *b == b'\n').enumerate() {
            let record: Record = serde_json::from_slice(line)
                .map_err(|why| self.at(format!("line {} is not a record: {why}", n + 1)))?;
            held.hold(record);
        }
        Ok(held)
    }

    /// Cuts the file to its first `len` bytes, and syncs it.
    fn cut(&self, len: usize) -> io::Result<()> {
        let file = OpenOptions::new().write(true).open(&self.path)?;
        file.set_len(len as u64)?;
        file.sync_data()
    }

    /// Appends `text` to the file, made if it is not there, and syncs it.
    fn append(&self, text: &[u8]) -> io::Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.path)?;
        file.write_all(text)?;
        file.sync_data()
    }
}

impl HardwareLog for FileLog {
    fn write(&self, records: &[Record]) -> Result<(), String> {
        let mut kept = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let mut held = match kept.take() {
            Some(held) => held,
            None => self.read()?,
        };
        let new = held.new_of(records);
        let mut text = Vec::new();
        for record in &new {
            serde_json::to_writer(&mut text, record).map_err(|why| self.at(why))?;
            text.push(b'\n');
        }
        if !text.is_empty() {
            // A failed append leaves `kept` empty: the file is read again at the next call.
            self.append(&text).map_err(|why| self.at(why))?;
        }
        for record in new {
            held.hold(record);
        }
        *kept = Some(held);
        Ok(())
    }

    fn page(&self, query: &Query) -> Result<Vec<Record>, String> {
        let mut kept = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let held = match kept.take() {
            Some(held) => held,
            None => self.read()?,
        };
        let page = held.page(query);
        *kept = Some(held);
        Ok(page)
    }
}

#[cfg(test)]
#[path = "hardware_files_tests.rs"]
mod tests;
