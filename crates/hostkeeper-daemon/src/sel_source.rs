//! The BMC's event log as a source of the host's hardware health (docs/design/host-health-alerts.md
//! sections 3.1, 4.4 and 4.5): [`SelReader`] asks the BMC through [`BmcPort`] (on a host,
//! [`crate::ipmi::DevIpmi`] over `/dev/ipmi0`), normalizes each new SEL entry with
//! [`crate::sel::decode`], its sensor named by the SDR, and gives the records to the
//! [`HealthKeeper`], which holds them until the coordinator writes them. It reads only: the
//! requests it can send are the table of [`BmcRequest`].
//!
//! **Each read** ([`SelReader::poll`]) first asks `Get Device ID`. A host with no device, or one
//! whose BMC does not answer, is a host with no BMC; on the second such read in a row the reader
//! holds one `bmc_missing` record (`host`, `warning`, `asserted`), and when a BMC answers again
//! its `deasserted` record (`about` the assertion). A daemon that may not open the device (not
//! root) is no answer about the BMC, and raises nothing. Then `Get SEL Info`: when the count, the
//! last addition and the last erase are those of the read before, nothing else is asked. Else the
//! entries after the last one read are read with `Get SEL Entry` (reservation 0, whole records),
//! from the first when the SEL was erased since (a new erase time, or fewer entries), when the
//! last one read is gone, and at the source's first read on the host, whose entries older than
//! seven days the keeper keeps as history. At most [`MAX_WALK`] entries are read at once; the rest
//! at the next read. The SDR is read again (`Get SDR Repository Info`, then `Get SDR` of each
//! record) only when its last addition or erase changed; an SDR that cannot be read names the
//! sensors by type and number.
//!
//! **Records.** Each entry is one record, `source` `sel`, its `event_id` `sel:<record id>@<the
//! SEL's last erase time>` (the SEL's record ids start again after an erase), `of_previous_boot`
//! when its time is before the boot began. A condition's entry is `asserted`, and its deassertion
//! `deasserted`, `about` the assertion of the same sensor and offset when the reader saw it. The
//! corrected errors of one subject in one hour are one `info` record that counts them, held once
//! the hour is over (or at once, for entries whose time the BMC does not know), its `event_id`
//! that of the hour's first entry, its `raw` the hour's last.
//!
//! **Source state.** `ok` after a read; `unsupported` with no device; `unreadable` with its reason
//! when the BMC does not answer, refuses, answers short or the daemon may not open the device.
//! After a read that failed the next waits twice as long, up to [`MAX_WAIT_SECS`]; after one that
//! did not, [`POLL_SECS`].
//!
//! **Marks.** What the reader must remember across a restart of the daemon is the source's mark
//! in the state file, its own JSON ([`SelMark`]): the SEL's count, last addition and erase, the
//! last record id read, the conditions not yet deasserted, and the hours of corrected errors not
//! yet over; and, under [`BMC_MARK`], the open `bmc_missing` assertion.

use std::collections::BTreeMap;
use std::io;
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use hostkeeper_launchd::PlatformKind;
use hostkeeper_link::{ClockSleeper, Log, Sleeper, StopFlag, Tick, UnixClock};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_steps::Clock;
use serde::{Deserialize, Serialize};

use crate::host_health::HealthKeeper;
use crate::ipmi::{BmcPort, BmcRequest, DevIpmi, IPMI_DEVICE};
use crate::sdr::{self, SdrInfo, SensorNames};
use crate::sel::{self, FIRST_RECORD, LAST_RECORD, SelInfo, SelRecord};
use crate::sel_tables::Kind;
use crate::unix_clock::SystemUnixClock;

/// The source's token: the records' `source` and the source state's.
pub const SOURCE: &str = "sel";

/// The keeper's mark of the BMC's presence (the open `bmc_missing` assertion), apart from the
/// SEL's so that a host whose BMC is missing at its first read still reads its SEL's history as
/// history when the BMC answers.
pub const BMC_MARK: &str = "sel-bmc";

/// The wait after a read that did not fail.
pub const POLL_SECS: u64 = 60;

/// The longest wait, after reads that failed.
pub const MAX_WAIT_SECS: u64 = 300;

/// The reads in a row with no BMC that make one `bmc_missing` record.
pub const MISSES_TO_MISSING: u32 = 2;

/// The most SEL entries one read asks for.
pub const MAX_WALK: usize = 4096;

/// The most SDR records one read of the SDR asks for.
pub const MAX_SDR: usize = 1024;

/// The most conditions the mark keeps open; past it the first by key is forgotten, so its
/// deassertion names no assertion.
pub const MAX_OPEN: usize = 128;

/// The completion code "requested sensor, data, or record not present".
const NOT_PRESENT: u8 = 0xCB;

const HOUR: u64 = 3600;

/// The SEL source's mark in the state file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SelMark {
    /// The SEL's count, last addition and last erase when it was last read whole.
    pub entries: u16,
    pub added: u32,
    pub erased: u32,
    /// The last record id read.
    pub last_id: Option<u16>,
    /// The `event_id` of each condition not yet deasserted, by [`sel::Decoded::condition`].
    pub open: BTreeMap<String, String>,
    /// The corrected errors of an hour not yet over, by code and subject.
    pub corrected: BTreeMap<String, Bucket>,
}

/// The corrected errors of one subject in one hour.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Bucket {
    /// The hour's first second; 0 for entries whose time the BMC does not know.
    pub hour: u64,
    pub erased: u32,
    pub first_id: u16,
    pub count: u64,
    pub time_unix: u64,
    pub component: i32,
    pub code: String,
    pub subject: String,
    /// The plural its count is told in.
    pub counted: String,
    pub raw: String,
}

/// The mark of the BMC's presence.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BmcMark {
    /// The `event_id` of the `bmc_missing` assertion not yet deasserted; empty when none.
    pub missing: String,
}

/// What a request came to: its data, or the completion code it was refused with.
enum Answer {
    Data(Vec<u8>),
    Refused(u8),
}

/// Why a read stopped: the source's state and reason.
struct Failed {
    status: pb::SourceStatus,
    reason: String,
}

impl Failed {
    fn unreadable(reason: String) -> Failed {
        Failed {
            status: pb::SourceStatus::Unreadable,
            reason,
        }
    }
}

/// The reader of the BMC's event log (the module's doc).
pub struct SelReader {
    port: Arc<dyn BmcPort>,
    keeper: Arc<HealthKeeper>,
    log: Arc<dyn Log>,
    /// The reads in a row that found no BMC.
    misses: u32,
    /// The reads in a row that failed.
    failures: u32,
    names: SensorNames,
    /// The SDR's state when `names` was read whole.
    sdr: Option<SdrInfo>,
    /// What the source last said, so that a change is logged once.
    said: Option<(pb::SourceStatus, String)>,
}

impl SelReader {
    /// The reader of the BMC behind `port`, whose records go to `keeper` and lines to `log`.
    #[must_use]
    pub fn new(port: Arc<dyn BmcPort>, keeper: Arc<HealthKeeper>, log: Arc<dyn Log>) -> SelReader {
        SelReader {
            port,
            keeper,
            log,
            misses: 0,
            failures: 0,
            names: SensorNames::default(),
            sdr: None,
            said: None,
        }
    }

    /// One read at the Unix second `now` (the module's doc); the seconds to wait before the next.
    pub fn poll(&mut self, now: u64) -> u64 {
        let read = self.present(now).and_then(|()| self.read_sel(now));
        let (status, reason) = match read {
            Ok(()) => {
                self.failures = 0;
                (pb::SourceStatus::Ok, String::new())
            }
            Err(failed) => {
                self.failures = self.failures.saturating_add(1);
                (failed.status, failed.reason)
            }
        };
        self.say(status, reason);
        (POLL_SECS << self.failures.min(3)).min(MAX_WAIT_SECS)
    }

    /// Reads at every wait `sleeper` gives, the time from `clock`, until it says stop.
    pub fn run(mut self, clock: &dyn UnixClock, sleeper: &dyn Sleeper) {
        loop {
            let wait = self.poll(clock.unix_secs());
            if sleeper.sleep(wait) == Tick::Stop {
                return;
            }
        }
    }

    fn say(&mut self, status: pb::SourceStatus, reason: String) {
        let now = (status, reason);
        if self.said.as_ref() != Some(&now) {
            self.log.line(&match now.0 {
                pb::SourceStatus::Ok => "the BMC's event log is read".to_owned(),
                _ => format!(
                    "error: the BMC's event log is {}: {}",
                    now.0
                        .as_str_name()
                        .trim_start_matches("SOURCE_STATUS_")
                        .to_lowercase(),
                    now.1
                ),
            });
        }
        self.keeper.source_state(pb::SourceState {
            source: SOURCE.to_owned(),
            status: now.0.into(),
            reason: now.1.clone(),
        });
        self.said = Some(now);
    }

    /// `request`, its completion code split off.
    fn ask(&self, request: BmcRequest) -> io::Result<Answer> {
        let bytes = self.port.ask(request)?;
        match bytes.split_first() {
            None => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "an empty answer",
            )),
            Some((&0, data)) => Ok(Answer::Data(data.to_vec())),
            Some((&code, _)) => Ok(Answer::Refused(code)),
        }
    }

    /// Whether a BMC answers `Get Device ID`, holding `bmc_missing` or its end (the module's doc).
    fn present(&mut self, now: u64) -> Result<(), Failed> {
        let gone = match self.ask(BmcRequest::DeviceId) {
            Ok(Answer::Data(_)) => None,
            Ok(Answer::Refused(code)) => {
                self.found(now);
                return Err(Failed::unreadable(format!(
                    "the BMC refused Get Device ID (0x{code:02x})"
                )));
            }
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                return Err(Failed::unreadable(format!(
                    "the daemon may not open {IPMI_DEVICE}"
                )));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Some((
                Failed {
                    status: pb::SourceStatus::Unsupported,
                    reason: format!("{IPMI_DEVICE} is absent"),
                },
                format!("No BMC device ({IPMI_DEVICE} is absent); hardware events cannot be read"),
                format!("{IPMI_DEVICE}: {e}"),
            )),
            Err(e) => Some((
                Failed::unreadable("the BMC does not answer Get Device ID".to_owned()),
                "No BMC answers; hardware events cannot be read".to_owned(),
                format!("Get Device ID: {e}"),
            )),
        };
        match gone {
            None => {
                self.found(now);
                Ok(())
            }
            Some((failed, message, raw)) => {
                self.misses = self.misses.saturating_add(1);
                let mark = self.bmc_mark();
                if self.misses >= MISSES_TO_MISSING && mark.missing.is_empty() {
                    let event_id = format!("bmc_missing@{now}");
                    let event = pb::HardwareEvent {
                        event_id: event_id.clone(),
                        time_unix: now,
                        component: pb::HardwareComponent::Host.into(),
                        subject: "BMC".to_owned(),
                        severity: pb::HardwareSeverity::Warning.into(),
                        state: pb::HardwareEventState::Asserted.into(),
                        source: SOURCE.to_owned(),
                        code: "bmc_missing".to_owned(),
                        message,
                        raw,
                        ..pb::HardwareEvent::default()
                    };
                    self.keep_bmc(&BmcMark { missing: event_id }, vec![event], now);
                }
                Err(failed)
            }
        }
    }

    /// A BMC answered: the misses end, and an open `bmc_missing` is deasserted.
    fn found(&mut self, now: u64) {
        self.misses = 0;
        let mark = self.bmc_mark();
        if !mark.missing.is_empty() {
            let event = pb::HardwareEvent {
                event_id: format!("{}:end", mark.missing),
                time_unix: now,
                component: pb::HardwareComponent::Host.into(),
                subject: "BMC".to_owned(),
                severity: pb::HardwareSeverity::Info.into(),
                state: pb::HardwareEventState::Deasserted.into(),
                source: SOURCE.to_owned(),
                code: "bmc_missing".to_owned(),
                message: "A BMC answers again".to_owned(),
                raw: "Get Device ID answered".to_owned(),
                about: mark.missing,
                ..pb::HardwareEvent::default()
            };
            self.keep_bmc(&BmcMark::default(), vec![event], now);
        }
    }

    fn bmc_mark(&self) -> BmcMark {
        self.keeper
            .mark(BMC_MARK)
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn keep_bmc(&self, mark: &BmcMark, events: Vec<pb::HardwareEvent>, now: u64) {
        let text = serde_json::to_string(mark).unwrap_or_default();
        self.keeper.record(BMC_MARK, &text, events, now);
    }

    /// The data of `request`, named `what` in a failure.
    fn data(&self, request: BmcRequest, what: &str) -> Result<Vec<u8>, Failed> {
        match self.ask(request) {
            Ok(Answer::Data(data)) => Ok(data),
            Ok(Answer::Refused(code)) => Err(Failed::unreadable(format!(
                "the BMC refused {what} (0x{code:02x})"
            ))),
            Err(e) => Err(Failed::unreadable(format!("{what}: {e}"))),
        }
    }

    /// The SEL entry `id` and the id after it; `Ok(None)` when the BMC says it is not there.
    fn entry(&self, id: u16) -> Result<Option<(u16, Vec<u8>)>, Failed> {
        let what = format!("Get SEL Entry 0x{id:04x}");
        let data = match self.ask(BmcRequest::SelEntry(id)) {
            Ok(Answer::Data(data)) => data,
            Ok(Answer::Refused(NOT_PRESENT)) => return Ok(None),
            Ok(Answer::Refused(code)) => {
                return Err(Failed::unreadable(format!(
                    "the BMC refused {what} (0x{code:02x})"
                )));
            }
            Err(e) => return Err(Failed::unreadable(format!("{what}: {e}"))),
        };
        sel::split_next(&data)
            .map(|(next, record)| Some((next, record.to_vec())))
            .ok_or_else(|| Failed::unreadable(format!("a short answer to {what}")))
    }

    /// Reads the SEL entries not read before (the module's doc).
    fn read_sel(&mut self, now: u64) -> Result<(), Failed> {
        let data = self.data(BmcRequest::SelInfo, "Get SEL Info")?;
        let info = sel::parse_info(&data)
            .ok_or_else(|| Failed::unreadable("a short answer to Get SEL Info".to_owned()))?;
        let kept = self.keeper.mark(SOURCE);
        let first = kept.is_none();
        let mut mark: SelMark = kept
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        let mut events = Vec::new();
        let same =
            mark.entries == info.entries && mark.added == info.added && mark.erased == info.erased;
        if !first && same {
            self.flush(&mut mark, &mut events, now, false);
            if !events.is_empty() {
                self.keep(&mark, events, now);
            }
            return Ok(());
        }
        if !first && (mark.erased != info.erased || info.entries < mark.entries) {
            self.log
                .line("the BMC's event log was erased; it is read again from its first entry");
            self.flush(&mut mark, &mut events, now, true);
            mark.last_id = None;
            mark.entries = 0;
            mark.open.clear();
        }
        // The erase is judged once: a read cut short below goes on from its last entry at the
        // next read, not from the first.
        mark.erased = info.erased;
        let start = match mark.last_id {
            _ if info.entries == 0 => None,
            None => Some(FIRST_RECORD),
            Some(last) => match self.entry(last) {
                Ok(Some((next, _))) => (next != LAST_RECORD).then_some(next),
                Ok(None) => Some(FIRST_RECORD),
                Err(failed) => return Err(failed),
            },
        };
        if start.is_some() {
            self.read_names();
        }
        let walked = self.walk(start, &info, &mut mark, &mut events);
        let complete = matches!(walked, Ok(true));
        if complete {
            mark.entries = info.entries;
            mark.added = info.added;
            mark.erased = info.erased;
        }
        self.flush(&mut mark, &mut events, now, false);
        self.keep(&mark, events, now);
        walked.map(|_| ())
    }

    /// Reads the entries from `start` on into `events`; whether it read to the last.
    fn walk(
        &self,
        start: Option<u16>,
        info: &SelInfo,
        mark: &mut SelMark,
        events: &mut Vec<pb::HardwareEvent>,
    ) -> Result<bool, Failed> {
        let Some(mut id) = start else {
            return Ok(true);
        };
        let boot_unix = self.keeper.boot_unix();
        for _ in 0..MAX_WALK {
            let Some((next, bytes)) = self.entry(id)? else {
                return Ok(false);
            };
            match SelRecord::parse(&bytes) {
                Some(record) => take(&record, &self.names, info.erased, boot_unix, mark, events),
                None => self.log.line(&format!(
                    "error: the BMC's event log entry 0x{id:04x} is {} bytes, not {}; it is passed over",
                    bytes.len(),
                    sel::SEL_RECORD_LEN
                )),
            }
            mark.last_id = Some(own_id(&bytes, id));
            if next == LAST_RECORD || next == id {
                return Ok(true);
            }
            id = next;
        }
        Ok(false)
    }

    /// Reads the SDR's sensor names again when it changed since they were read.
    fn read_names(&mut self) {
        let info = match self.data(BmcRequest::SdrInfo, "Get SDR Repository Info") {
            Ok(data) => sdr::parse_info(&data),
            Err(failed) => {
                self.log.line(&format!(
                    "error: the BMC's sensor records are not read: {}",
                    failed.reason
                ));
                return;
            }
        };
        if info.is_some() && info == self.sdr {
            return;
        }
        let mut names = SensorNames::default();
        let mut id = FIRST_RECORD;
        let mut complete = false;
        for _ in 0..MAX_SDR {
            let data = match self.data(BmcRequest::Sdr(id), "Get SDR") {
                Ok(data) => data,
                Err(failed) => {
                    self.log.line(&format!(
                        "error: the BMC's sensor record 0x{id:04x} is not read: {}",
                        failed.reason
                    ));
                    break;
                }
            };
            let Some((next, record)) = sel::split_next(&data) else {
                break;
            };
            names.add(record);
            if next == LAST_RECORD || next == id {
                complete = true;
                break;
            }
            id = next;
        }
        if complete || self.names == SensorNames::default() {
            self.names = names;
        }
        self.sdr = if complete { info } else { None };
    }

    /// Holds the corrected errors of hours over by `now` (all of them with `all`) as records.
    fn flush(&self, mark: &mut SelMark, events: &mut Vec<pb::HardwareEvent>, now: u64, all: bool) {
        let boot_unix = self.keeper.boot_unix();
        let (due, open): (BTreeMap<String, Bucket>, BTreeMap<String, Bucket>) =
            std::mem::take(&mut mark.corrected)
                .into_iter()
                .partition(|(_, b)| all || b.hour == 0 || b.hour.saturating_add(HOUR) <= now);
        mark.corrected = open;
        events.extend(due.values().map(|bucket| counted(bucket, boot_unix)));
    }

    fn keep(&self, mark: &SelMark, events: Vec<pb::HardwareEvent>, now: u64) {
        let text = serde_json::to_string(mark).unwrap_or_default();
        self.keeper.record(SOURCE, &text, events, now);
    }
}

/// The record id `bytes` give themselves, which is `asked` but for the first entry (asked as
/// [`FIRST_RECORD`]); `asked` for an entry too short to give one.
fn own_id(bytes: &[u8], asked: u16) -> u16 {
    bytes
        .get(..2)
        .map_or(asked, |id| u16::from_le_bytes([id[0], id[1]]))
}

/// The record of `bucket`'s corrected errors.
fn counted(bucket: &Bucket, boot_unix: u64) -> pb::HardwareEvent {
    let told = if bucket.count == 1 {
        bucket
            .counted
            .strip_suffix('s')
            .unwrap_or(&bucket.counted)
            .to_owned()
    } else {
        bucket.counted.clone()
    };
    let when = if bucket.hour == 0 { "" } else { " in the hour" };
    pb::HardwareEvent {
        event_id: format!("sel:{:04x}@{}", bucket.first_id, bucket.erased),
        time_unix: bucket.time_unix,
        of_previous_boot: before_boot(bucket.time_unix, boot_unix),
        component: bucket.component,
        subject: bucket.subject.clone(),
        severity: pb::HardwareSeverity::Info.into(),
        state: pb::HardwareEventState::Event.into(),
        source: SOURCE.to_owned(),
        code: bucket.code.clone(),
        message: format!("{} {told} on {}{when}", bucket.count, bucket.subject),
        raw: bucket.raw.clone(),
        about: String::new(),
    }
}

fn before_boot(time_unix: u64, boot_unix: u64) -> bool {
    time_unix != 0 && boot_unix != 0 && time_unix < boot_unix
}

/// Takes the entry `record` into `events` or into the hours of corrected errors of `mark`, its
/// sensor named by `names`, the SEL last erased at `erased`, the boot begun at `boot_unix`.
fn take(
    record: &SelRecord,
    names: &SensorNames,
    erased: u32,
    boot_unix: u64,
    mark: &mut SelMark,
    events: &mut Vec<pb::HardwareEvent>,
) {
    let decoded = sel::decode(record, names);
    let time_unix = record.time.unix();
    let event_id = format!("sel:{:04x}@{erased}", record.id);
    let mut state = pb::HardwareEventState::Event;
    let mut about = String::new();
    if decoded.deassertion {
        state = pb::HardwareEventState::Deasserted;
        about = mark.open.remove(&decoded.condition).unwrap_or_default();
    } else if decoded.kind == Kind::Corrected {
        let hour = if time_unix == 0 {
            0
        } else {
            time_unix - time_unix % HOUR
        };
        let key = format!("{}/{}", decoded.code, decoded.subject);
        if mark
            .corrected
            .get(&key)
            .is_some_and(|open| open.hour != hour)
        {
            events.extend(
                mark.corrected
                    .remove(&key)
                    .map(|done| counted(&done, boot_unix)),
            );
        }
        let bucket = mark.corrected.entry(key).or_insert_with(|| Bucket {
            hour,
            erased,
            first_id: record.id,
            count: 0,
            time_unix,
            component: decoded.component.into(),
            code: decoded.code.clone(),
            subject: decoded.subject.clone(),
            counted: decoded.counted.unwrap_or("corrected errors").to_owned(),
            raw: String::new(),
        });
        bucket.count += 1;
        bucket.raw.clone_from(&decoded.raw);
        return;
    } else if decoded.kind == Kind::Condition {
        state = pb::HardwareEventState::Asserted;
        if mark.open.len() >= MAX_OPEN && !mark.open.contains_key(&decoded.condition) {
            mark.open.pop_first();
        }
        mark.open
            .insert(decoded.condition.clone(), event_id.clone());
    }
    events.push(pb::HardwareEvent {
        event_id,
        time_unix,
        of_previous_boot: before_boot(time_unix, boot_unix),
        component: decoded.component.into(),
        subject: decoded.subject,
        severity: decoded.severity.into(),
        state: state.into(),
        source: SOURCE.to_owned(),
        code: decoded.code,
        message: decoded.message,
        raw: decoded.raw,
        about,
    });
}

/// On a Linux host (`kind`, under systemd or OpenRC) with a keeper, starts a [`SelReader`] of
/// `port` on a thread of its own, its records to `keeper`, its time from `clock` and its waits
/// from `sleeper`; on a Mac, which has no BMC, or with no keeper, starts nothing.
pub fn start(
    kind: PlatformKind,
    port: Arc<dyn BmcPort>,
    keeper: Option<Arc<HealthKeeper>>,
    clock: Arc<dyn UnixClock>,
    sleeper: Box<dyn Sleeper + Send>,
    log: Arc<dyn Log>,
) -> Option<JoinHandle<()>> {
    let keeper = keeper?;
    match kind {
        PlatformKind::Linux | PlatformKind::Openrc => Some(thread::spawn(move || {
            SelReader::new(port, keeper, log).run(clock.as_ref(), sleeper.as_ref());
        })),
        PlatformKind::Macos => None,
    }
}

/// [`start`] over the host's BMC ([`DevIpmi::host`]), the wall clock, and waits on the daemon's
/// `clock` that end when `stop` is set.
pub fn at_host_start(
    kind: PlatformKind,
    keeper: Option<Arc<HealthKeeper>>,
    clock: Arc<dyn Clock>,
    stop: StopFlag,
    log: Arc<dyn Log>,
) -> Option<JoinHandle<()>> {
    start(
        kind,
        Arc::new(DevIpmi::host()),
        keeper,
        Arc::new(SystemUnixClock),
        Box::new(ClockSleeper::new(clock, stop)),
        log,
    )
}

#[cfg(test)]
#[path = "sel_source_tests.rs"]
mod tests;
