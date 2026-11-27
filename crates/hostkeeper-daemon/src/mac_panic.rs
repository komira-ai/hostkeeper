//! A Mac's kernel panic reports, parsed and classified into records of the host's hardware event
//! log (docs/design/host-health-alerts.md sections 2.2 and 3.2): [`parse`] reads one report of
//! `/Library/Logs/DiagnosticReports`, [`classify`] judges its panic line, and
//! [`PanicReport::event`] gives its record. Nothing here reads a file or the clock: the reader of
//! the directory is `crate::mac_health`.
//!
//! **The forms a report takes.**
//!
//! - **JSON** (`.panic` and `.ips`, macOS 12 and later): a header of one line, a JSON object with
//!   `bug_type`, `timestamp` and `incident_id`, then a body, a JSON object with `panicString`
//!   (and `incident` and `date`). Only a header whose `bug_type` is [`PANIC_BUG_TYPE`] is a kernel
//!   panic; any other `.ips` report (an app's crash, a hang, a stackshot) is passed over.
//! - **Text** (`.panic` of an older release): the panic line `panic(cpu ...` among lines of plain
//!   text, with no incident id.
//! - **Cut short** (a report past the reader's cap, or written while the Mac went down): what the
//!   header says, and the first panic line found in the bytes there are.
//!
//! A `.ips` report whose header does not parse, or whose `bug_type` is not a panic, is not a panic
//! report. A `.panic` report is a panic whatever it holds: one with no panic line is a record with
//! no `raw` (class [`PanicClass::Unreadable`]).
//!
//! **Its id.** The record's `event_id` is `panic:` and the report's `incident_id` in lower case
//! (the header's, else the body's `incident`), so the `.panic` and `.ips` reports of one panic, and
//! a report read again, are one record. A report with no usable incident id (none, longer than
//! [`MAX_INCIDENT`], or holding more than hex digits and dashes) is known by its file's name
//! instead: `panic:file:` and the first 8 bytes of the name's SHA-256, as hex.
//!
//! **Its time.** The header's `timestamp`, else the body's `date` (`2026-10-14 03:12:07.00 +0000`:
//! a date, a time with optional fractions, and an offset from UTC); else when the file was last
//! modified, which the reader gives.
//!
//! **Its class** ([`classify`], on the panic line and the lines after it in `panicString`, case
//! not minded), in this order, the first that matches deciding:
//!
//! 1. an `SError` or an `AMCC` fault (the memory controller's): hardware, `panic_hardware`,
//!    critical;
//! 2. a userspace watchdog (`userspace watchdog`, or `no successful checkins from`, naming the
//!    process that missed its check-ins): software, `panic_software`, warning;
//! 3. any other watchdog (the SoC's): software, warning;
//! 4. a kernel extension named in the backtrace that is not Apple's (`Kernel Extensions in
//!    backtrace:`, the first line after it that is not `com.apple.`): software, warning;
//! 5. anything else: software, warning.
//!
//! A runaway build can trip a watchdog, so a watchdog is never read as bad hardware.

use std::fmt::Write as _;

use hostkeeper_proto::health::{MAX_EVENT_ID, MAX_MESSAGE, MAX_RAW};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use crate::health_state::cut;

/// The `bug_type` of a kernel panic report.
pub const PANIC_BUG_TYPE: &str = "210";

/// The extension of a panic report.
pub const PANIC_EXTENSION: &str = ".panic";

/// The extension of a diagnostic report, a panic only when its header says so.
pub const IPS_EXTENSION: &str = ".ips";

/// The source token of a panic record.
pub const PANIC_SOURCE: &str = "panic";

/// The `code` of a panic of suspected bad hardware.
pub const PANIC_HARDWARE: &str = "panic_hardware";

/// The `code` of a panic of software, or of load.
pub const PANIC_SOFTWARE: &str = "panic_software";

/// The subject of a panic record: the Mac's board.
pub const BOARD: &str = "board";

/// How a panic line starts.
pub const PANIC_LINE: &str = "panic(cpu";

/// The line before the kernel extensions of a backtrace.
pub const KEXT_HEADING: &str = "Kernel Extensions in backtrace:";

/// The start of Apple's own kernel extensions.
const APPLE_KEXT: &str = "com.apple.";

/// The longest incident id used as the record's id: `panic:` and it fit [`MAX_EVENT_ID`].
pub const MAX_INCIDENT: usize = MAX_EVENT_ID - "panic:".len();

/// The start of the id of a report known by its file's name.
pub const FILE_ID_PREFIX: &str = "panic:file:";

/// What a panic is, by its panic line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PanicClass {
    /// An SError interrupt: the CPU was told of a fault it could not attribute to an instruction.
    SError,
    /// A fault the memory controller (AMCC) reported.
    Amcc,
    /// A userspace watchdog: the process named missed its check-ins (empty when not named).
    UserspaceWatchdog(String),
    /// The SoC's watchdog, or any other watchdog.
    SocWatchdog,
    /// A kernel extension that is not Apple's, named in the backtrace.
    Kext(String),
    /// Anything else.
    Software,
    /// A `.panic` report with no panic line to read.
    Unreadable,
}

impl PanicClass {
    /// Whether it suggests bad hardware.
    #[must_use]
    pub fn is_hardware(&self) -> bool {
        matches!(self, PanicClass::SError | PanicClass::Amcc)
    }

    /// The record's `code`.
    #[must_use]
    pub fn code(&self) -> &'static str {
        if self.is_hardware() {
            PANIC_HARDWARE
        } else {
            PANIC_SOFTWARE
        }
    }

    /// The record's severity: critical for hardware, a warning otherwise.
    #[must_use]
    pub fn severity(&self) -> pb::HardwareSeverity {
        if self.is_hardware() {
            pb::HardwareSeverity::Critical
        } else {
            pb::HardwareSeverity::Warning
        }
    }

    /// The record's message, at most `MAX_MESSAGE` bytes.
    #[must_use]
    pub fn message(&self) -> String {
        let text = match self {
            PanicClass::SError => "Kernel panic (SError), suspected hardware fault".to_owned(),
            PanicClass::Amcc => {
                "Kernel panic (AMCC memory controller fault), suspected hardware fault".to_owned()
            }
            PanicClass::UserspaceWatchdog(process) if process.is_empty() => {
                "Kernel panic (userspace watchdog), software or load".to_owned()
            }
            PanicClass::UserspaceWatchdog(process) => {
                format!("Kernel panic (userspace watchdog: {process}), software or load")
            }
            PanicClass::SocWatchdog => "Kernel panic (SoC watchdog), software or load".to_owned(),
            PanicClass::Kext(kext) => format!("Kernel panic in kernel extension {kext}"),
            PanicClass::Software => "Kernel panic (software)".to_owned(),
            PanicClass::Unreadable => "Kernel panic (its report has no panic line)".to_owned(),
        };
        cut(&text, MAX_MESSAGE)
    }

    /// The class in one word, as a log line names it.
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self {
            PanicClass::SError => "SError",
            PanicClass::Amcc => "AMCC",
            PanicClass::UserspaceWatchdog(_) => "userspace watchdog",
            PanicClass::SocWatchdog => "SoC watchdog",
            PanicClass::Kext(_) => "kernel extension",
            PanicClass::Software => "software",
            PanicClass::Unreadable => "unreadable",
        }
    }
}

/// One panic report, parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanicReport {
    /// The record's `event_id` (the module's doc).
    pub event_id: String,
    /// When it happened, Unix seconds (the module's doc).
    pub time_unix: u64,
    /// Its panic line, as the report printed it; empty when it has none.
    pub panic_line: String,
    /// Its class.
    pub class: PanicClass,
}

impl PanicReport {
    /// Its record, read in the boot that began at `boot_unix` (0 when not known): `board`,
    /// source [`PANIC_SOURCE`], by its class, `raw` its panic line cut to its bound, and of the
    /// boot before when it happened before this boot began.
    #[must_use]
    pub fn event(&self, boot_unix: u64) -> pb::HardwareEvent {
        pb::HardwareEvent {
            event_id: self.event_id.clone(),
            time_unix: self.time_unix,
            of_previous_boot: boot_unix != 0 && self.time_unix < boot_unix,
            component: pb::HardwareComponent::Board.into(),
            subject: BOARD.to_owned(),
            severity: self.class.severity().into(),
            state: pb::HardwareEventState::Event.into(),
            source: PANIC_SOURCE.to_owned(),
            code: self.class.code().to_owned(),
            message: self.class.message(),
            raw: cut(&self.panic_line, MAX_RAW),
            about: String::new(),
        }
    }
}

/// Whether a file named `name` may be a panic report: a `.panic` or a `.ips` report.
#[must_use]
pub fn is_candidate(name: &str) -> bool {
    name.ends_with(PANIC_EXTENSION) || name.ends_with(IPS_EXTENSION)
}

/// The header of a JSON report.
#[derive(Deserialize)]
struct Header {
    #[serde(default)]
    bug_type: Option<serde_json::Value>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    incident_id: Option<String>,
}

/// The body of a JSON panic report.
#[derive(Deserialize)]
struct Body {
    #[serde(default, rename = "panicString")]
    panic_string: Option<String>,
    #[serde(default)]
    incident: Option<String>,
    #[serde(default)]
    date: Option<String>,
}

/// The report named `name`, whose bytes (perhaps cut short) are `bytes` and whose file was last
/// modified at `modified_unix`; none when it is not a panic report (the module's doc).
#[must_use]
pub fn parse(name: &str, bytes: &[u8], modified_unix: u64) -> Option<PanicReport> {
    let lossy = String::from_utf8_lossy(bytes);
    let text: &str = &lossy;
    let (first, rest) = text.split_once('\n').unwrap_or((text, ""));
    let header = serde_json::from_str::<Header>(first.trim()).ok();
    if !name.ends_with(PANIC_EXTENSION) {
        // A `.ips` report: a panic only by its header.
        if !header
            .as_ref()?
            .bug_type
            .as_ref()
            .is_some_and(is_panic_bug_type)
        {
            return None;
        }
    }
    let body = header
        .as_ref()
        .and_then(|_| serde_json::from_str::<Body>(rest.trim()).ok());
    let header_incident = header.as_ref().and_then(|h| h.incident_id.clone());
    let body_incident = body.as_ref().and_then(|b| b.incident.clone());
    let event_id = header_incident
        .into_iter()
        .chain(body_incident)
        .find_map(|id| incident_event_id(&id))
        .unwrap_or_else(|| file_event_id(name));
    let header_time = header.as_ref().and_then(|h| h.timestamp.clone());
    let body_time = body.as_ref().and_then(|b| b.date.clone());
    let time_unix = header_time
        .into_iter()
        .chain(body_time)
        .find_map(|t| parse_timestamp(&t))
        .unwrap_or(modified_unix);
    let panic_text = body
        .and_then(|b| b.panic_string)
        .and_then(|s| s.find(PANIC_LINE).map(|at| s[at..].to_owned()));
    let (panic_line, class) = match panic_text {
        Some(panic) => {
            let line = panic.lines().next().unwrap_or_default().to_owned();
            let class = classify(&panic);
            (line, class)
        }
        None => match scan_panic_line(text) {
            Some(line) => {
                let class = classify(&line);
                (line, class)
            }
            None => (String::new(), PanicClass::Unreadable),
        },
    };
    Some(PanicReport {
        event_id,
        time_unix,
        panic_line,
        class,
    })
}

/// Whether a header's `bug_type` is [`PANIC_BUG_TYPE`], as a string or as a number.
fn is_panic_bug_type(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(text) => text == PANIC_BUG_TYPE,
        other => other.to_string() == PANIC_BUG_TYPE,
    }
}

/// The record id of the incident `id`: `panic:` and it in lower case; none when it is empty,
/// past [`MAX_INCIDENT`] or holds more than hex digits and dashes.
fn incident_event_id(id: &str) -> Option<String> {
    let id = id.trim();
    let usable = !id.is_empty()
        && id.len() <= MAX_INCIDENT
        && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    usable.then(|| format!("panic:{}", id.to_ascii_lowercase()))
}

/// The record id of the report named `name`: [`FILE_ID_PREFIX`] and the first 8 bytes of the
/// name's SHA-256, as hex.
#[must_use]
pub fn file_event_id(name: &str) -> String {
    let digest = Sha256::digest(name.as_bytes());
    let mut id = FILE_ID_PREFIX.to_owned();
    for byte in &digest[..8] {
        let _ = write!(id, "{byte:02x}");
    }
    id
}

/// The first panic line of `text`, read as it lies in the file: from [`PANIC_LINE`] to the end of
/// its line, or to a JSON-escaped line end (`\n`) when the line lies inside a JSON string, whose
/// escaped quotes are read as quotes.
fn scan_panic_line(text: &str) -> Option<String> {
    let at = text.find(PANIC_LINE)?;
    let line = text[at..].lines().next().unwrap_or_default();
    let line = line.split("\\n").next().unwrap_or_default();
    Some(line.replace("\\\"", "\""))
}

/// The class of the panic whose text (its panic line, and the lines after it when the report
/// has them) is `panic` (the module's doc).
#[must_use]
pub fn classify(panic: &str) -> PanicClass {
    let first = panic.lines().next().unwrap_or_default();
    let lower = first.to_ascii_lowercase();
    if lower.contains("serror") {
        return PanicClass::SError;
    }
    if lower.contains("amcc") {
        return PanicClass::Amcc;
    }
    const CHECKINS: &str = "no successful checkins from ";
    if lower.contains("userspace watchdog") || lower.contains(CHECKINS) {
        let process = lower
            .find(CHECKINS)
            .map(|at| {
                first[at + CHECKINS.len()..]
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_owned()
            })
            .unwrap_or_default();
        return PanicClass::UserspaceWatchdog(cut(&process, 48));
    }
    if lower.contains("watchdog") {
        return PanicClass::SocWatchdog;
    }
    if let Some(kext) = foreign_kext(panic) {
        return PanicClass::Kext(kext);
    }
    PanicClass::Software
}

/// The first kernel extension of the backtrace of `panic` that is not Apple's: the first word
/// of a line after [`KEXT_HEADING`], up to its version's `(`; the list ends at its first blank
/// line.
fn foreign_kext(panic: &str) -> Option<String> {
    let mut lines = panic.lines();
    lines.find(|line| line.trim() == KEXT_HEADING)?;
    lines
        .map(str::trim)
        .take_while(|line| !line.is_empty())
        .map(|line| line.split('(').next().unwrap_or_default().trim())
        .find(|kext| !kext.is_empty() && !kext.starts_with(APPLE_KEXT))
        .map(|kext| cut(kext, 64))
}

/// `2026-10-14 03:12:07.00 +0000` (a report's `timestamp` or `date`) as Unix seconds: a date, a
/// time with optional fractions (dropped), and an offset from UTC as `+HHMM` or `-HHMM`; none
/// when it reads otherwise, or is before the Unix epoch.
#[must_use]
pub fn parse_timestamp(text: &str) -> Option<u64> {
    let mut words = text.split_whitespace();
    let date = words.next()?;
    let time = words.next()?;
    let offset = words.next()?;
    if words.next().is_some() {
        return None;
    }
    let mut ymd = date.split('-');
    let year: i64 = number(ymd.next()?, 4)?;
    let month: i64 = number(ymd.next()?, 2)?;
    let day: i64 = number(ymd.next()?, 2)?;
    if ymd.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let whole = time.split('.').next()?;
    let mut hms = whole.split(':');
    let hour: i64 = number(hms.next()?, 2)?;
    let minute: i64 = number(hms.next()?, 2)?;
    let second: i64 = number(hms.next()?, 2)?;
    if hms.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let (sign, digits) = match offset.split_at_checked(1)? {
        ("+", digits) => (1, digits),
        ("-", digits) => (-1, digits),
        _ => return None,
    };
    if digits.len() != 4 {
        return None;
    }
    let off_hours: i64 = number(&digits[..2], 2)?;
    let off_minutes: i64 = number(&digits[2..], 2)?;
    let local = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second;
    let unix = local - sign * (off_hours * 3600 + off_minutes * 60);
    u64::try_from(unix).ok()
}

/// `text` as a number of exactly `width` ASCII digits.
fn number(text: &str, width: usize) -> Option<i64> {
    (text.len() == width && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

/// The days from the Unix epoch to the proleptic Gregorian date `year`-`month`-`day`.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_from_march = (month + 9) % 12;
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}
