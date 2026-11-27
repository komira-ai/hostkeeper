//! The daemon's state file of its host's hardware health (docs/design/host-health-alerts.md
//! sections 4.1 and 4.4): [`HEALTH_FILE`] in the state directory, JSON, written whole (a
//! temporary file, fsync, rename, directory fsync: [`crate::files::write_atomic`]) after every
//! change. It holds the machine it was written on, the boot it was last written in, the
//! clean-shutdown marker of that boot, whether the boot before it ended with one, the records
//! not yet written to the host's log ([`Queue`]), and each source's high-water mark
//! ([`SourceMark`]).
//!
//! **Read.** A file that is not there is a host the daemon has not run on, or whose state
//! directory was wiped: nothing held, no marker. A file that does not parse is moved aside, to
//! [`HEALTH_FILE_ASIDE`] (one moved aside before is replaced), and read as one that is not there;
//! so is a file written on another machine (both machine ids known, and different), whose records
//! and marks are not this host's. A field this release does not know is passed over, and one it
//! knows that the file lacks takes its default, so a file of a release before or after this one
//! reads (B8: the format only grows).
//!
//! **Held records.** [`Queue::hold`] keeps a record by its `event_id`, once: a record held again
//! is passed over. Each text is cut to its bound of `hostkeeper_proto::health` (at a character
//! boundary); a record whose `event_id` is empty or longer than its bound is refused. Past
//! [`MAX_HELD`] records, the `info` records of one source, the source of the oldest `info` record
//! that has two or more, are folded into one `info` record that counts them (code [`FOLDED_CODE`]);
//! no `warning` or `critical` record is ever folded or dropped, so a queue of more than
//! [`MAX_HELD`] holds those alone past its bound. [`Queue::to_send`] gives the records of the next
//! heartbeat, at most `hostkeeper_proto::health::MAX_EVENTS`: the most severe first, then the
//! oldest, then the first held. [`Queue::written`] drops the records the coordinator wrote.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

use hostkeeper_proto::health::{
    MAX_CODE, MAX_EVENT_ID, MAX_EVENTS, MAX_MESSAGE, MAX_RAW, MAX_SOURCE, MAX_SUBJECT,
};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use serde::{Deserialize, Serialize};

use crate::files::{WriteStep, write_atomic};

/// The state file, in the state directory.
pub const HEALTH_FILE: &str = "health-state.json";

/// Where a state file that does not parse, or that another machine wrote, is moved.
pub const HEALTH_FILE_ASIDE: &str = "health-state.json.corrupt";

/// The most records the queue holds before it folds `info` records.
pub const MAX_HELD: usize = 256;

/// The `code` of a record that stands for `info` records folded into it.
pub const FOLDED_CODE: &str = "folded";

/// The clean-shutdown marker of the boot the file was last written in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Marker {
    /// The daemon runs, or ran and did not stop with the system: what it writes at its start.
    #[default]
    Running,
    /// The daemon stopped because the system was going down (an orderly shutdown or reboot).
    Clean,
    /// A marker this release does not know, which a later release wrote: read as not clean, so
    /// the rest of the file still reads.
    #[serde(other)]
    Unknown,
}

/// What a source last read, so that it reads only what is newer (a SEL's last record id and
/// add time, a journal's cursor): its own words, which only it reads.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SourceMark {
    /// When the daemon first read the source on this host, Unix seconds: the records of that
    /// first read older than the history window are history ([`crate::host_health`]).
    pub first_read_unix: u64,
    /// The source's high-water mark.
    pub mark: String,
}

/// One record held until the coordinator writes it: a `HardwareEvent`, with how many records it
/// stands for and the order it was held in.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Held {
    pub event_id: String,
    pub time_unix: u64,
    pub of_previous_boot: bool,
    pub component: i32,
    pub subject: String,
    pub severity: i32,
    pub state: i32,
    pub source: String,
    pub code: String,
    pub message: String,
    pub raw: String,
    pub about: String,
    /// How many records it stands for: 1, or more for a fold.
    pub count: u64,
    /// Its place in the order records were held in, the earliest lowest.
    pub seq: u64,
}

impl Held {
    /// The record as the heartbeat carries it.
    #[must_use]
    pub fn event(&self) -> pb::HardwareEvent {
        pb::HardwareEvent {
            event_id: self.event_id.clone(),
            time_unix: self.time_unix,
            of_previous_boot: self.of_previous_boot,
            component: self.component,
            subject: self.subject.clone(),
            severity: self.severity,
            state: self.state,
            source: self.source.clone(),
            code: self.code.clone(),
            message: self.message.clone(),
            raw: self.raw.clone(),
            about: self.about.clone(),
        }
    }

    fn is_info(&self) -> bool {
        self.severity == i32::from(pb::HardwareSeverity::Info)
    }
}

/// Why a record was not held.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refused {
    /// Its `event_id` is empty, or longer than its bound.
    #[error("its event_id is {len} bytes, not 1 to {MAX_EVENT_ID}")]
    EventId { len: usize },
}

/// The records held until the coordinator writes them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Queue {
    pub records: Vec<Held>,
    /// The `seq` the next record held takes.
    pub next_seq: u64,
}

impl Queue {
    /// Holds `event`, its texts cut to their bounds: `Ok(true)` when it is held now, `Ok(false)`
    /// when a record of its `event_id` was held already. Past [`MAX_HELD`] it folds `info`
    /// records (the module's doc).
    ///
    /// # Errors
    /// [`Refused`] when its `event_id` is empty or past its bound.
    pub fn hold(&mut self, event: pb::HardwareEvent) -> Result<bool, Refused> {
        let len = event.event_id.len();
        if len == 0 || len > MAX_EVENT_ID {
            return Err(Refused::EventId { len });
        }
        if self.records.iter().any(|r| r.event_id == event.event_id) {
            return Ok(false);
        }
        let seq = self.take_seq();
        self.records.push(Held {
            subject: cut(&event.subject, MAX_SUBJECT),
            source: cut(&event.source, MAX_SOURCE),
            code: cut(&event.code, MAX_CODE),
            message: cut(&event.message, MAX_MESSAGE),
            raw: cut(&event.raw, MAX_RAW),
            about: cut(&event.about, MAX_EVENT_ID),
            event_id: event.event_id,
            time_unix: event.time_unix,
            of_previous_boot: event.of_previous_boot,
            component: event.component,
            severity: event.severity,
            state: event.state,
            count: 1,
            seq,
        });
        while self.records.len() > MAX_HELD && self.fold() {}
        Ok(true)
    }

    /// The records of the next heartbeat: at most `MAX_EVENTS`, the most severe first, then the
    /// oldest by the source's time, then the first held.
    #[must_use]
    pub fn to_send(&self) -> Vec<pb::HardwareEvent> {
        let mut order: Vec<&Held> = self.records.iter().collect();
        order.sort_by_key(|r| (Reverse(r.severity), r.time_unix, r.seq));
        order.iter().take(MAX_EVENTS).map(|r| r.event()).collect()
    }

    /// Drops the records named in `event_ids`; how many it dropped.
    pub fn written(&mut self, event_ids: &[String]) -> usize {
        let before = self.records.len();
        self.records.retain(|r| !event_ids.contains(&r.event_id));
        before - self.records.len()
    }

    fn take_seq(&mut self) -> u64 {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        seq
    }

    /// Folds the `info` records of the source of the oldest `info` record whose source holds
    /// two or more into one record, held where the oldest of them was; false when no source
    /// holds two.
    fn fold(&mut self) -> bool {
        let mut infos: BTreeMap<&str, usize> = BTreeMap::new();
        for r in self.records.iter().filter(|r| r.is_info()) {
            *infos.entry(r.source.as_str()).or_default() += 1;
        }
        let Some(source) = self
            .records
            .iter()
            .filter(|r| r.is_info() && infos.get(r.source.as_str()).is_some_and(|n| *n > 1))
            .min_by_key(|r| (r.time_unix, r.seq))
            .map(|r| r.source.clone())
        else {
            return false;
        };
        let (folded, kept): (Vec<Held>, Vec<Held>) = std::mem::take(&mut self.records)
            .into_iter()
            .partition(|r| r.is_info() && r.source == source);
        self.records = kept;
        let count = folded.iter().map(|r| r.count).sum::<u64>();
        let oldest = folded
            .iter()
            .min_by_key(|r| (r.time_unix, r.seq))
            .cloned()
            .unwrap_or_default();
        let newest_raw = folded
            .iter()
            .max_by_key(|r| (r.time_unix, r.seq))
            .map(|r| r.raw.clone())
            .unwrap_or_default();
        let fresh = self.take_seq();
        self.records.push(Held {
            event_id: cut(
                &format!("fold:{source}:{}:{fresh}", oldest.time_unix),
                MAX_EVENT_ID,
            ),
            code: FOLDED_CODE.to_owned(),
            message: cut(
                &format!("{count} info records of {source} folded into one while held"),
                MAX_MESSAGE,
            ),
            raw: newest_raw,
            state: i32::from(pb::HardwareEventState::Event),
            about: String::new(),
            count,
            ..oldest
        });
        true
    }
}

/// The state file's contents.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HealthFile {
    /// The machine it was written on (`/etc/machine-id`); empty where the host has none to read.
    pub machine_id: String,
    /// The boot it was last written in; empty when the daemon could not read it.
    pub boot_id: String,
    /// When that boot began, Unix seconds; 0 when not read.
    pub boot_unix: u64,
    /// The clean-shutdown marker of that boot.
    pub marker: Marker,
    /// Whether the boot before that one ended with its clean-shutdown marker, as the daemon found
    /// at the first start of that boot.
    pub previous_clean: bool,
    /// The records not yet written to the host's log.
    pub held: Queue,
    /// Each source's mark, by the source's token.
    pub sources: BTreeMap<String, SourceMark>,
}

/// What reading the state file came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Read {
    /// There is no state file.
    Absent,
    /// The state file, read.
    Read(HealthFile),
    /// The state file could not be read, did not parse, or another machine wrote it: it was
    /// moved aside to [`HEALTH_FILE_ASIDE`] (`moved`, or why it could not be) and is read as
    /// absent. `why` says what was wrong with it.
    Aside {
        why: String,
        moved: Result<(), String>,
    },
}

/// Reads the state file of `state_dir`, written on the machine `machine_id` (empty when the
/// host has none to read, which any file matches).
#[must_use]
pub fn read(state_dir: &Path, machine_id: &str) -> Read {
    let path = state_dir.join(HEALTH_FILE);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Read::Absent,
        Err(e) => return aside(state_dir, format!("it cannot be read: {e}")),
    };
    match serde_json::from_slice::<HealthFile>(&bytes) {
        Err(e) => aside(state_dir, format!("it does not parse: {e}")),
        Ok(file)
            if !file.machine_id.is_empty()
                && !machine_id.is_empty()
                && file.machine_id != machine_id =>
        {
            aside(
                state_dir,
                format!("machine {} wrote it, not this one", file.machine_id),
            )
        }
        Ok(file) => Read::Read(file),
    }
}

fn aside(state_dir: &Path, why: String) -> Read {
    let moved = fs::rename(
        state_dir.join(HEALTH_FILE),
        state_dir.join(HEALTH_FILE_ASIDE),
    )
    .map_err(|e| e.to_string());
    Read::Aside { why, moved }
}

/// Writes `file` whole to the state file of `state_dir`; `stop` is asked at each [`WriteStep`]
/// ([`write_atomic`]).
///
/// # Errors
/// The write's own.
pub fn write(
    state_dir: &Path,
    file: &HealthFile,
    stop: &dyn Fn(WriteStep) -> bool,
) -> io::Result<()> {
    let bytes = serde_json::to_vec(file).map_err(io::Error::other)?;
    write_atomic(state_dir, HEALTH_FILE, &bytes, stop)
}

/// `text` cut to at most `max` bytes, at a character boundary.
#[must_use]
pub fn cut(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}
