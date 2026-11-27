//! rasdaemon's database read into hardware event records (docs/design/host-health-alerts.md
//! sections 3.1 and 4.5): on a Linux host, the source [`RasdaemonSource`] of
//! [`crate::hardware_sources`] reads [`DATABASE`] every [`POLL_SECS`] seconds, turns each new row
//! of the tables it knows into a record of the host's hardware event log, and says whether the
//! database could be read; its worker hands both to the daemon's health state
//! ([`crate::host_health::HealthKeeper::read`]).
//!
//! **The tables** ([`TABLES`]): `mc_event` (memory errors, by DIMM label), `mce_record` (machine
//! checks, by CPU socket), `aer_event` (PCIe errors, by device), `mf_event` (memory failures: a
//! page the kernel took out of use) and `disk_errors` (failed block requests, by disk). A table the database lacks is passed over: rasdaemon makes
//! the tables of the events it was built to record. A table read with no row is read: an empty
//! memory table is a host with no memory error, or one whose firmware records them in the BMC's
//! event log only, and is never a source that is not read.
//!
//! **New rows only.** The mark kept in the state file ([`Mark`]) holds the last rowid read of each
//! table (its `id`, an `INTEGER PRIMARY KEY`). Each read takes the rows after it, at most
//! [`CHUNK`] at a time, each chunk a read of its own ([`sqlite_read::steady`]), and at most
//! [`MAX_ROWS`] of a table in one poll, but on the source's first read on this host, which takes
//! every row, so that what is older than the history window is history
//! ([`crate::host_health::HISTORY_SECS`]). A table whose last rowid is below the mark was made
//! again (rasdaemon reinstalled, its database removed): it is read from its first row, and the
//! read says so in its notes. A record's `event_id` is `rasdaemon:<table>:<rowid>@<time>`, the
//! row's time in Unix seconds, so a row read twice is one record, and a row of a database made
//! again is not taken for the row of the same id before it.
//!
//! **Severity** (section 3.1): an uncorrected or fatal memory error, a machine check whose status
//! has its uncorrected bit, an uncorrected or fatal PCIe error and a memory failure are
//! `critical`; a deferred memory error, a type rasdaemon did not write before and a machine check
//! with no status are `warning`; a memory event of type `Info` is `info`.
//!
//! **Corrected errors are counted, an hour at a time** (section 2.1): the corrected memory errors
//! of one DIMM (by their `err_count`), the corrected machine checks of one CPU and the corrected
//! PCIe errors of one device in one hour of their time are one `info` record, and the failed
//! requests of one disk in one hour one `warning` record (`io_error`), whose message carries the
//! count, whose `raw` is the hour's last row, and whose `event_id` is of the hour's first row. The
//! hour's rows are counted in the mark until the hour is over (or, when more than
//! [`MAX_PENDING`] subjects and hours are counted, at once), so a failing part makes at most 24
//! records a day.
//!
//! **Source states.** No database file is `unsupported` ([`ABSENT`]), and so is a database with
//! none of the tables ([`NO_TABLES`]); a database that cannot be read (WAL mode, written during
//! every attempt, malformed, not SQLite) is `unreadable` with the reason, and nothing is held
//! until a read succeeds. The database is never written ([`crate::sqlite_read`]).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use serde::{Deserialize, Serialize};

use crate::hardware_sources::{HardwareSource, SourceRead};
use crate::host_health::PROC_STAT;
use crate::maintenance_host::parse_boot_time;
use crate::sqlite_read::{self, Database, Row, SqliteError, Table, Value};

/// rasdaemon's database.
pub const DATABASE: &str = "/var/lib/rasdaemon/ras-mc_event.db";

/// The source's token: the `source` of its records and of its state.
pub const SOURCE: &str = "rasdaemon";

/// The seconds between two reads.
pub const POLL_SECS: u64 = 60;

/// The most rows of one read of a table.
pub const CHUNK: usize = 256;

/// The most rows of a table one poll reads, but the source's first on a host.
pub const MAX_ROWS: usize = 4096;

/// The most subjects and hours of corrected errors counted in the mark before they are all
/// made records.
pub const MAX_PENDING: usize = 64;

/// The reason of a host with no database.
pub const ABSENT: &str = "no rasdaemon database";

/// The reason of a database with none of the tables.
pub const NO_TABLES: &str = "no rasdaemon table in its database";

/// The uncorrected bit of a machine check's status (`MCi_STATUS.UC`).
pub const MCI_STATUS_UC: u64 = 1 << 61;

/// The tables read, in the order they are read.
pub const TABLES: [&str; 5] = [
    "mc_event",
    "mce_record",
    "aer_event",
    "mf_event",
    "disk_errors",
];

const HOUR: u64 = 60 * 60;

/// What the source keeps in the state file between reads.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Mark {
    /// The last rowid read of each table, by its name.
    pub ids: BTreeMap<String, i64>,
    /// The corrected errors of an hour not over yet, counted.
    pub pending: Vec<Pending>,
}

/// The corrected errors of one subject of one table in one hour, counted.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Pending {
    pub table: String,
    pub subject: String,
    /// The hour: Unix seconds divided by 3600.
    pub hour: u64,
    /// The rowid and the time of the hour's first row.
    pub first_id: i64,
    pub first_unix: u64,
    /// How many errors.
    pub count: u64,
    /// The hour's last row, as its `raw`.
    pub raw: String,
}

/// What one read of the database came to.
#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    /// Whether it was read.
    pub status: pb::SourceStatus,
    /// Why not; empty when it was.
    pub reason: String,
    /// When it was read: the records and the new mark.
    pub found: Option<Found>,
    /// What the read found worth saying besides its records: a database made again, a mark that
    /// does not parse.
    pub notes: Vec<String>,
}

/// The records a read made, and the mark it leaves.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub events: Vec<pb::HardwareEvent>,
    /// The [`Mark`], as JSON.
    pub mark: String,
}

/// Why a read found nothing.
enum Shut {
    Unsupported(&'static str),
    Unreadable(String),
}

impl From<SqliteError> for Shut {
    fn from(e: SqliteError) -> Shut {
        Shut::Unreadable(e.to_string())
    }
}

/// What a row says, before the record's ids and time are added.
enum Normal {
    /// A record of its own.
    Event {
        severity: pb::HardwareSeverity,
        code: &'static str,
        subject: String,
        message: String,
        raw: String,
    },
    /// Errors counted in their hour: corrected ones, or a disk's failed requests.
    Corrected {
        subject: String,
        count: u64,
        raw: String,
    },
}

/// A table read, and how its rows are said.
struct Kind {
    table: &'static str,
    component: pb::HardwareComponent,
    /// The severity and `code` of the record of its errors counted in an hour, and what its
    /// message calls them.
    counted_severity: pb::HardwareSeverity,
    corrected_code: &'static str,
    corrected_what: &'static str,
    normalize: fn(&Cols) -> Normal,
}

static KINDS: [Kind; 5] = [
    Kind {
        table: TABLES[0],
        component: pb::HardwareComponent::Memory,
        counted_severity: pb::HardwareSeverity::Info,
        corrected_code: "memory_corrected",
        corrected_what: "corrected errors",
        normalize: memory,
    },
    Kind {
        table: TABLES[1],
        component: pb::HardwareComponent::Cpu,
        counted_severity: pb::HardwareSeverity::Info,
        corrected_code: "cpu_corrected",
        corrected_what: "corrected machine checks",
        normalize: machine_check,
    },
    Kind {
        table: TABLES[2],
        component: pb::HardwareComponent::Pcie,
        counted_severity: pb::HardwareSeverity::Info,
        corrected_code: "pcie_corrected",
        corrected_what: "corrected PCIe errors",
        normalize: pcie,
    },
    Kind {
        table: TABLES[3],
        component: pb::HardwareComponent::Memory,
        counted_severity: pb::HardwareSeverity::Info,
        corrected_code: "memory_corrected",
        corrected_what: "corrected errors",
        normalize: memory_failure,
    },
    Kind {
        table: TABLES[4],
        component: pb::HardwareComponent::Disk,
        counted_severity: pb::HardwareSeverity::Warning,
        corrected_code: "io_error",
        corrected_what: "I/O errors",
        normalize: disk_error,
    },
];

/// A row's values by their column's name.
struct Cols<'a> {
    table: &'a Table,
    row: &'a Row,
}

impl Cols<'_> {
    fn value(&self, name: &str) -> &Value {
        self.table
            .column(name)
            .map_or(&Value::Null, |index| self.row.get(index))
    }

    /// The column as text, trimmed (a blob's bytes as UTF-8, a number in decimal); empty when
    /// it is null.
    fn text(&self, name: &str) -> String {
        match self.value(name) {
            Value::Text(text) => text.trim().to_owned(),
            Value::Blob(bytes) => String::from_utf8_lossy(bytes).trim().to_owned(),
            Value::Integer(n) => n.to_string(),
            Value::Real(x) => x.to_string(),
            Value::Null => String::new(),
        }
    }

    /// The column as an integer, a text one parsed; none when it is not one.
    fn int(&self, name: &str) -> Option<i64> {
        match self.value(name) {
            Value::Integer(n) => Some(*n),
            Value::Text(text) => text.trim().parse().ok(),
            Value::Null | Value::Real(_) | Value::Blob(_) => None,
        }
    }
}

/// `text`, or `fallback` when it is empty.
fn or(text: String, fallback: String) -> String {
    if text.is_empty() { fallback } else { text }
}

fn event(
    severity: pb::HardwareSeverity,
    code: &'static str,
    subject: String,
    message: String,
    raw: String,
) -> Normal {
    Normal::Event {
        severity,
        code,
        subject,
        message,
        raw,
    }
}

/// A row of `mc_event`, by its `err_type`.
fn memory(c: &Cols) -> Normal {
    use pb::HardwareSeverity::{Critical, Info, Warning};
    let label = c.text("label");
    let subject = or(label.clone(), format!("mc{}", c.int("mc").unwrap_or(0)));
    let count = c
        .int("err_count")
        .and_then(|n| u64::try_from(n).ok())
        .filter(|n| *n > 0)
        .unwrap_or(1);
    let kind = c.text("err_type");
    let raw = format!(
        "{} | {kind} | count {count} | {} | {label} | {}",
        c.text("timestamp"),
        c.text("err_msg"),
        c.text("driver_detail"),
    );
    match kind.to_lowercase().as_str() {
        "corrected" => Normal::Corrected {
            subject,
            count,
            raw,
        },
        "uncorrected" | "fatal" => event(
            Critical,
            "memory_uncorrectable",
            subject.clone(),
            format!("Uncorrectable memory error on {subject}: {count} reported"),
            raw,
        ),
        "deferred" => event(
            Warning,
            "memory_deferred",
            subject.clone(),
            format!("Deferred memory error on {subject}: {count} reported"),
            raw,
        ),
        "info" => event(
            Info,
            "memory_info",
            subject.clone(),
            format!("Memory event on {subject}: {}", c.text("err_msg")),
            raw,
        ),
        _ => event(
            Warning,
            "memory_error",
            subject.clone(),
            format!("Memory error ({kind}) on {subject}: {count} reported"),
            raw,
        ),
    }
}

/// A row of `mce_record`, by its status's uncorrected bit.
fn machine_check(c: &Cols) -> Normal {
    let subject = format!("CPU {}", c.int("socketid").unwrap_or(0));
    let bank = c.int("bank").unwrap_or(0);
    let status = c.int("status").map(i64::cast_unsigned);
    let raw = format!(
        "{} | cpu {} | bank {bank} {} | status {} | {} | {} | {}",
        c.text("timestamp"),
        c.int("cpu").unwrap_or(0),
        c.text("bank_name"),
        status.map_or_else(|| "none".to_owned(), |s| format!("{s:#018x}")),
        c.text("error_msg"),
        c.text("mcistatus_msg"),
        c.text("mc_location"),
    );
    match status {
        None => event(
            pb::HardwareSeverity::Warning,
            "cpu_machine_check",
            subject.clone(),
            format!("Machine check on {subject}, bank {bank}, with no status"),
            raw,
        ),
        Some(status) if status & MCI_STATUS_UC != 0 => event(
            pb::HardwareSeverity::Critical,
            "cpu_machine_check",
            subject.clone(),
            format!("Uncorrected machine check on {subject}, bank {bank}"),
            raw,
        ),
        Some(_) => Normal::Corrected {
            subject,
            count: 1,
            raw,
        },
    }
}

/// A row of `aer_event`, by its `err_type`.
fn pcie(c: &Cols) -> Normal {
    use pb::HardwareSeverity::{Critical, Warning};
    let subject = or(c.text("dev_name"), "pcie".to_owned());
    let kind = c.text("err_type");
    let raw = format!(
        "{} | {subject} | {kind} | {}",
        c.text("timestamp"),
        c.text("err_msg"),
    );
    match kind.to_lowercase().as_str() {
        "corrected" => Normal::Corrected {
            subject,
            count: 1,
            raw,
        },
        "uncorrected (fatal)" => event(
            Critical,
            "pcie_fatal",
            subject.clone(),
            format!("Fatal PCIe error on {subject}"),
            raw,
        ),
        "uncorrected (non-fatal)" => event(
            Critical,
            "pcie_uncorrected",
            subject.clone(),
            format!("Uncorrected PCIe error on {subject}"),
            raw,
        ),
        _ => event(
            Warning,
            "pcie_error",
            subject.clone(),
            format!("PCIe error ({kind}) on {subject}"),
            raw,
        ),
    }
}

/// A row of `mf_event`: a page of memory the kernel took out of use.
fn memory_failure(c: &Cols) -> Normal {
    let (pfn, page_type, result) = (c.text("pfn"), c.text("page_type"), c.text("action_result"));
    event(
        pb::HardwareSeverity::Critical,
        "memory_failure",
        "memory".to_owned(),
        format!("Memory failure at page {pfn} ({page_type}): {result}"),
        format!(
            "{} | pfn {pfn} | {page_type} | {result}",
            c.text("timestamp")
        ),
    )
}

/// A row of `disk_errors`: a block request that failed, counted by its disk.
fn disk_error(c: &Cols) -> Normal {
    let subject = or(c.text("dev"), "disk".to_owned());
    Normal::Corrected {
        raw: format!(
            "{} | {subject} | sector {} | {} sectors | {} | {} | {}",
            c.text("timestamp"),
            c.text("sector"),
            c.text("nr_sector"),
            c.text("error"),
            c.text("rwbs"),
            c.text("cmd"),
        ),
        subject,
        count: 1,
    }
}

/// rasdaemon's time, `YYYY-MM-DD HH:MM:SS +ZZZZ` (its zone optional), in Unix seconds; none when
/// it is not one, or is before 1970.
#[must_use]
pub fn parse_time(text: &str) -> Option<u64> {
    let mut words = text.split_whitespace();
    let (date, time, zone) = (
        words.next()?,
        words.next()?,
        words.next().unwrap_or("+0000"),
    );
    let numbers = |text: &str, sep: char| -> Option<[i64; 3]> {
        let mut parts = text.split(sep).map(|part| part.parse::<i64>().ok());
        let found = [parts.next()??, parts.next()??, parts.next()??];
        parts.next().is_none().then_some(found)
    };
    let [year, month, day] = numbers(date, '-')?;
    let [hour, minute, second] = numbers(time, ':')?;
    let bounds = [
        (month, 1, 12),
        (day, 1, 31),
        (hour, 0, 23),
        (minute, 0, 59),
        (second, 0, 60),
    ];
    bounds
        .iter()
        .all(|(value, low, high)| (*low..=*high).contains(value))
        .then_some(())?;
    let sign = match zone.as_bytes().first() {
        Some(b'+') => 1,
        Some(b'-') => -1,
        _ => return None,
    };
    let digits = zone
        .get(1..)
        .filter(|d| d.len() == 4 && d.bytes().all(|b| b.is_ascii_digit()))?;
    let offset = digits.parse::<i64>().ok()?;
    let offset = sign * ((offset / 100) * 3600 + (offset % 100) * 60);
    // Days from the Unix epoch of the civil date (the proleptic Gregorian calendar, March-based).
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let of_era = year - era * 400;
    let of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let of_era_day = of_era * 365 + of_era / 4 - of_era / 100 + of_year;
    let days = era * 146_097 + of_era_day - 719_468;
    u64::try_from(days * 86_400 + hour * 3600 + minute * 60 + second - offset).ok()
}

fn event_id(table: &str, rowid: i64, time_unix: u64) -> String {
    format!("{SOURCE}:{table}:{rowid}@{time_unix}")
}

/// Reads the database at `database` after `mark` (none on the source's first read on this
/// host), in the boot that began at `boot_unix`, at `now_unix` (the module's doc).
#[must_use]
pub fn read(database: &Path, mark: Option<&str>, boot_unix: u64, now_unix: u64) -> Reading {
    let mut notes = Vec::new();
    let scanned = scan(database, mark, boot_unix, now_unix, &mut notes);
    let (status, reason, found) = match scanned {
        Ok(found) => (pb::SourceStatus::Ok, String::new(), Some(found)),
        Err(Shut::Unsupported(reason)) => (pb::SourceStatus::Unsupported, reason.to_owned(), None),
        Err(Shut::Unreadable(reason)) => (pb::SourceStatus::Unreadable, reason, None),
    };
    Reading {
        status,
        reason,
        found,
        notes,
    }
}

fn scan(
    database: &Path,
    mark: Option<&str>,
    boot_unix: u64,
    now_unix: u64,
    notes: &mut Vec<String>,
) -> Result<Found, Shut> {
    if !sqlite_read::exists(database)? {
        return Err(Shut::Unsupported(ABSENT));
    }
    let db = Database::open(database)?;
    let tables = sqlite_read::steady(&db, Database::tables)?;
    let present: Vec<(&Kind, &Table)> = KINDS
        .iter()
        .filter_map(|kind| tables.get(kind.table).map(|table| (kind, table)))
        .collect();
    if present.is_empty() {
        return Err(Shut::Unsupported(NO_TABLES));
    }
    let mut cursor = mark.map_or_else(Mark::default, |text| {
        serde_json::from_str(text).unwrap_or_else(|e| {
            notes.push(format!(
                "rasdaemon: its mark does not parse ({e}); read from the first rows"
            ));
            Mark::default()
        })
    });
    let budget = mark.map_or(usize::MAX, |_| MAX_ROWS);
    let of_previous = |time: u64| (1..boot_unix).contains(&time);
    let mut events = Vec::new();
    for (kind, table) in present {
        let mut after = cursor.ids.get(kind.table).copied().unwrap_or(0);
        let last = sqlite_read::steady(&db, |db| db.max_rowid(table.root))?.unwrap_or(0);
        if last < after {
            notes.push(format!(
                "rasdaemon: {} has no row after {after}, its last is {last}: the database was \
                 made again; read from its first row",
                kind.table
            ));
            after = 0;
        }
        let mut read = 0;
        loop {
            let rows = sqlite_read::steady(&db, |db| db.rows_after(table.root, after, CHUNK))?;
            for row in &rows {
                let cols = Cols { table, row };
                let time = parse_time(&cols.text("timestamp")).unwrap_or(0);
                match (kind.normalize)(&cols) {
                    Normal::Event {
                        severity,
                        code,
                        subject,
                        message,
                        raw,
                    } => events.push(pb::HardwareEvent {
                        event_id: event_id(kind.table, row.rowid, time),
                        time_unix: time,
                        of_previous_boot: of_previous(time),
                        component: kind.component.into(),
                        subject,
                        severity: severity.into(),
                        state: pb::HardwareEventState::Event.into(),
                        source: SOURCE.to_owned(),
                        code: code.to_owned(),
                        message,
                        raw,
                        about: String::new(),
                    }),
                    Normal::Corrected {
                        subject,
                        count,
                        raw,
                    } => count_in(
                        &mut cursor.pending,
                        Pending {
                            table: kind.table.to_owned(),
                            subject,
                            hour: time / HOUR,
                            first_id: row.rowid,
                            first_unix: time,
                            count,
                            raw,
                        },
                    ),
                }
            }
            after = rows.last().map_or(after, |row| row.rowid);
            read += rows.len();
            if rows.len() < CHUNK {
                break;
            }
            if read >= budget {
                break;
            }
        }
        cursor.ids.insert(kind.table.to_owned(), after);
    }
    let hour = now_unix / HOUR;
    let all = cursor.pending.len() > MAX_PENDING;
    let (due, kept): (Vec<Pending>, Vec<Pending>) = std::mem::take(&mut cursor.pending)
        .into_iter()
        .partition(|pending| all || pending.hour != hour);
    cursor.pending = kept;
    events.extend(due.iter().filter_map(|pending| {
        let kind = KINDS.iter().find(|kind| kind.table == pending.table)?;
        Some(pb::HardwareEvent {
            event_id: event_id(kind.table, pending.first_id, pending.first_unix),
            time_unix: pending.first_unix,
            of_previous_boot: of_previous(pending.first_unix),
            component: kind.component.into(),
            subject: pending.subject.clone(),
            severity: kind.counted_severity.into(),
            state: pb::HardwareEventState::Event.into(),
            source: SOURCE.to_owned(),
            code: kind.corrected_code.to_owned(),
            message: format!(
                "{} {} on {} in the hour",
                pending.count, kind.corrected_what, pending.subject
            ),
            raw: pending.raw.clone(),
            about: String::new(),
        })
    }));
    Ok(Found {
        events,
        mark: serde_json::to_string(&cursor).unwrap_or_default(),
    })
}

/// Counts `new` in the count of its table, subject and hour, or as a count of its own.
fn count_in(pending: &mut Vec<Pending>, new: Pending) {
    let same = pending
        .iter_mut()
        .find(|p| (&p.table, &p.subject, p.hour) == (&new.table, &new.subject, new.hour));
    match same {
        Some(counted) => {
            counted.count = counted.count.saturating_add(new.count);
            counted.raw = new.raw;
        }
        None => pending.push(new),
    }
}

/// rasdaemon's database as a source of the host's hardware ([`HardwareSource`]), read with
/// [`read`] in the boot that began at the `btime` of the host's [`PROC_STAT`], which its first
/// read reads (0 when it cannot be read, which marks no record of the boot before).
pub struct RasdaemonSource {
    root: PathBuf,
    boot_unix: Option<u64>,
}

impl RasdaemonSource {
    /// The source of the host whose root is `root`: its [`DATABASE`] under it. Nothing is read
    /// before the first read.
    #[must_use]
    pub fn new(root: &Path) -> RasdaemonSource {
        RasdaemonSource {
            root: root.to_path_buf(),
            boot_unix: None,
        }
    }
}

impl HardwareSource for RasdaemonSource {
    fn name(&self) -> &'static str {
        SOURCE
    }

    fn every_secs(&self) -> u64 {
        POLL_SECS
    }

    /// One [`read`]; a read that found the mark it was given gives no mark, so the state file is
    /// not written again for a database with nothing new.
    fn read(&mut self, mark: Option<&str>, now_unix: u64) -> SourceRead {
        let root = &self.root;
        let boot_unix = *self.boot_unix.get_or_insert_with(|| {
            fs::read_to_string(root.join(PROC_STAT)).map_or(0, |text| parse_boot_time(&text))
        });
        let database = self.root.join(DATABASE.trim_start_matches('/'));
        let reading = read(&database, mark, boot_unix, now_unix);
        SourceRead {
            status: reading.status,
            reason: reading.reason,
            mark: reading
                .found
                .as_ref()
                .map(|found| found.mark.clone())
                .filter(|found| mark != Some(found.as_str())),
            events: reading.found.map(|found| found.events).unwrap_or_default(),
        }
    }
}
