//! The BMC's system event log (IPMI 2.0 section 31), decoded: the answer of `Get SEL Info`
//! ([`SelInfo`]), one 16-byte entry of `Get SEL Entry` ([`SelRecord`]), and the entry normalized
//! into the parts of a hardware event record ([`Decoded`], docs/design/host-health-alerts.md
//! sections 2.1 and 3.1) by the tables of [`crate::sel_tables`] and the sensor names of the SDR
//! ([`crate::sdr::SensorNames`]). Pure: bytes in, values out; [`crate::sel_source`] asks the BMC.
//!
//! **Normalized.** A system event entry (record type 0x02) of a sensor type and offset the tables
//! know takes that offset's part of the machine, code, severity and words; a threshold entry
//! (event type 0x01) of a temperature is a `warning` (`thermal_critical` past a critical or
//! non-recoverable threshold, `thermal_threshold` past a non-critical one), a fan past a lower
//! critical or non-recoverable threshold is `critical` (`fan_failed`), any other threshold
//! `warning` for a fan and `info` for any other sensor; a generic discrete state, an offset or
//! a sensor type the tables do not know, and an OEM entry (record types 0xC0 to 0xFF), is `info`,
//! kept, never dropped. A deassertion (the direction bit of byte 13) is `info`, the end of the
//! condition its sensor and offset began ([`Decoded::condition`]).
//!
//! **Subject.** The SDR's name of the sensor, its underscores read as spaces (`DIMM_D1` is
//! `DIMM D1`); with no name, the sensor type and number (`Memory #0x53`); `board` for an OEM
//! entry.
//!
//! **Raw.** The entry as `ipmitool sel list` lays it out, the SDR's name after the sensor:
//! `01a4 | 11/26/2026 | 03:00:12 | Memory #0x53 DIMM_D1 | Uncorrectable ECC | Asserted`, its
//! time in UTC; a time before the BMC's clock was set ("pre-init", at most 0x20000000) is
//! `Pre-Init | <seconds since the BMC's start>`, and an unspecified time (0xFFFFFFFF)
//! `Unspecified | Unspecified`.

use std::fmt::Write as _;

use hostkeeper_proto::hostkeeper::agent::v1 as pb;

use crate::sdr::SensorNames;
use crate::sel_tables::{self, Kind, Level, Offset, SensorType, THRESHOLDS};

/// The length of one SEL entry.
pub const SEL_RECORD_LEN: usize = 16;

/// The largest SEL timestamp that counts from the BMC's start rather than 1970 (IPMI 2.0
/// section 37.1).
pub const PRE_INIT_MAX: u32 = 0x2000_0000;

/// The record id `Get SEL Entry` and `Get SDR` take for the first record.
pub const FIRST_RECORD: u16 = 0x0000;

/// The "next record id" of the last record.
pub const LAST_RECORD: u16 = 0xFFFF;

/// What `Get SEL Info` answered (its completion code taken off).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SelInfo {
    /// How many entries the SEL holds.
    pub entries: u16,
    /// The timestamp of the most recent addition.
    pub added: u32,
    /// The timestamp of the most recent erase.
    pub erased: u32,
}

/// The response data of `Get SEL Info` (version, entries, free space, the most recent addition
/// and erase, the operations supported) read; none when shorter than its 14 bytes.
#[must_use]
pub fn parse_info(data: &[u8]) -> Option<SelInfo> {
    let data = data.get(..14)?;
    Some(SelInfo {
        entries: u16::from_le_bytes([data[1], data[2]]),
        added: u32::from_le_bytes([data[5], data[6], data[7], data[8]]),
        erased: u32::from_le_bytes([data[9], data[10], data[11], data[12]]),
    })
}

/// The response data of `Get SEL Entry` or `Get SDR`: the next record id, then the record; none
/// when shorter than the next id.
#[must_use]
pub fn split_next(data: &[u8]) -> Option<(u16, &[u8])> {
    let (next, record) = data.split_at_checked(2)?;
    Some((u16::from_le_bytes([next[0], next[1]]), record))
}

/// When a SEL entry says it happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelTime {
    /// Seconds since 1970, UTC.
    At(u32),
    /// Seconds since the BMC started, before its clock was set.
    PreInit(u32),
    /// The BMC gives none.
    Unspecified,
}

impl SelTime {
    /// The time of the timestamp `raw`.
    #[must_use]
    pub fn of(raw: u32) -> SelTime {
        match raw {
            u32::MAX => SelTime::Unspecified,
            raw if raw <= PRE_INIT_MAX => SelTime::PreInit(raw),
            raw => SelTime::At(raw),
        }
    }

    /// Unix seconds; 0 when not known.
    #[must_use]
    pub fn unix(self) -> u64 {
        match self {
            SelTime::At(t) => u64::from(t),
            SelTime::PreInit(_) | SelTime::Unspecified => 0,
        }
    }

    /// The date and time columns of the raw line.
    fn columns(self) -> String {
        match self {
            SelTime::At(t) => utc(u64::from(t)),
            SelTime::PreInit(t) => format!("Pre-Init | {t:010}"),
            SelTime::Unspecified => "Unspecified | Unspecified".to_owned(),
        }
    }
}

/// `MM/DD/YYYY | HH:MM:SS` of the Unix second `t`, UTC.
fn utc(t: u64) -> String {
    let days = t / 86_400;
    let secs = t % 86_400;
    let (year, month, day) = civil(days);
    format!(
        "{month:02}/{day:02}/{year:04} | {:02}:{:02}:{:02}",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

/// The proleptic Gregorian date (year, month, day) `days` after the Unix epoch (H. Hinnant's
/// `civil_from_days`).
fn civil(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    (year, month, day)
}

/// One SEL entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelRecord {
    pub id: u16,
    /// The record type: 0x02 a system event, 0xC0 to 0xDF OEM with a timestamp, 0xE0 to 0xFF OEM
    /// without one.
    pub record_type: u8,
    pub time: SelTime,
    /// The generator id's first byte: the IPMB slave address or software id, as the SDR's
    /// sensor owner id names it.
    pub generator: u8,
    /// The generator's logical unit (bits 1:0 of the generator id's second byte).
    pub lun: u8,
    pub sensor_type: u8,
    pub sensor_number: u8,
    /// The direction bit: the condition ended.
    pub deassertion: bool,
    /// The event/reading type (0x01 threshold, 0x02 to 0x0C generic, 0x6F sensor-specific).
    pub event_type: u8,
    /// Event data 1 to 3.
    pub data: [u8; 3],
    /// The entry's own bytes.
    pub bytes: [u8; SEL_RECORD_LEN],
}

impl SelRecord {
    /// The entry of `bytes`; none when they are not 16.
    #[must_use]
    pub fn parse(bytes: &[u8]) -> Option<SelRecord> {
        let bytes: [u8; SEL_RECORD_LEN] = bytes.try_into().ok()?;
        let record_type = bytes[2];
        let time = if record_type >= 0xE0 {
            SelTime::Unspecified
        } else {
            SelTime::of(u32::from_le_bytes([bytes[3], bytes[4], bytes[5], bytes[6]]))
        };
        Some(SelRecord {
            id: u16::from_le_bytes([bytes[0], bytes[1]]),
            record_type,
            time,
            generator: bytes[7],
            lun: bytes[8] & 0x03,
            sensor_type: bytes[10],
            sensor_number: bytes[11],
            deassertion: bytes[12] & 0x80 != 0,
            event_type: bytes[12] & 0x7F,
            data: [bytes[13], bytes[14], bytes[15]],
            bytes,
        })
    }

    /// Whether it is an OEM entry (record types 0xC0 to 0xFF), whose bytes past the timestamp
    /// are the manufacturer's.
    #[must_use]
    pub fn is_oem(&self) -> bool {
        self.record_type >= 0xC0
    }

    /// The offset of its event (the low nibble of event data 1).
    #[must_use]
    pub fn offset(&self) -> u8 {
        self.data[0] & 0x0F
    }
}

/// A SEL entry normalized: the parts of its hardware event record that come from the entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decoded {
    pub component: pb::HardwareComponent,
    pub severity: pb::HardwareSeverity,
    pub kind: Kind,
    /// The condition ended (a deassertion).
    pub deassertion: bool,
    pub code: String,
    pub subject: String,
    pub message: String,
    pub raw: String,
    /// What a condition and its deassertion share: the generator, the sensor, the event type
    /// and the offset (`20.0:53:6f:4`); empty for an OEM entry, which is no condition.
    pub condition: String,
    /// For a corrected error, the plural its hour's count is told in.
    pub counted: Option<&'static str>,
}

/// What the tables say an offset is, before the entry's subject is known.
struct Meaning {
    text: String,
    phrase: String,
    code: &'static str,
    severity: pb::HardwareSeverity,
    kind: Kind,
}

impl Meaning {
    fn of(offset: &Offset) -> Meaning {
        Meaning {
            text: offset.text.to_owned(),
            phrase: offset.phrase.to_owned(),
            code: offset.code,
            severity: offset.severity,
            kind: offset.kind,
        }
    }

    fn info(text: String, phrase: String, code: &'static str, kind: Kind) -> Meaning {
        Meaning {
            text,
            phrase,
            code,
            severity: pb::HardwareSeverity::Info,
            kind,
        }
    }
}

/// The entry `record` normalized, its sensor named by `names` (the module's doc).
#[must_use]
pub fn decode(record: &SelRecord, names: &SensorNames) -> Decoded {
    if record.is_oem() {
        return oem(record);
    }
    let kind = sel_tables::sensor_type(record.sensor_type);
    let type_name = kind.map_or_else(
        || format!("Sensor type 0x{:02x}", record.sensor_type),
        |t| t.name.to_owned(),
    );
    let name = names.name(record.generator, record.lun, record.sensor_number);
    let subject = name.map_or_else(
        || format!("{type_name} #0x{:02x}", record.sensor_number),
        |n| n.replace('_', " ").trim().to_owned(),
    );
    let meaning = meaning(record, kind, &type_name);
    let raw = format!(
        "{:04x} | {} | {type_name} #0x{:02x}{} | {} | {}",
        record.id,
        record.time.columns(),
        record.sensor_number,
        name.map_or_else(String::new, |n| format!(" {n}")),
        meaning.text,
        if record.deassertion {
            "Deasserted"
        } else {
            "Asserted"
        },
    );
    let said = format!("{} on {subject}", meaning.phrase);
    let (severity, message) = if record.deassertion {
        (pb::HardwareSeverity::Info, format!("Ended: {said}"))
    } else {
        (meaning.severity, said)
    };
    Decoded {
        component: kind.map_or(pb::HardwareComponent::Board, |t| t.component),
        severity,
        kind: meaning.kind,
        deassertion: record.deassertion,
        code: meaning.code.to_owned(),
        message,
        raw,
        condition: format!(
            "{:02x}.{}:{:02x}:{:02x}:{:x}",
            record.generator,
            record.lun,
            record.sensor_number,
            record.event_type,
            record.offset()
        ),
        counted: (meaning.kind == Kind::Corrected).then_some(
            kind.and_then(|t| t.offset(record.offset()))
                .map_or("corrected errors", |o| o.phrase),
        ),
        subject,
    }
}

/// What the tables say the event of `record`, of the sensor type `kind` named `type_name`, is.
fn meaning(record: &SelRecord, kind: Option<&SensorType>, type_name: &str) -> Meaning {
    let offset = record.offset();
    match record.event_type {
        0x01 => threshold(record.sensor_type, offset, type_name),
        0x6F => match kind.and_then(|t| t.offset(offset)) {
            Some(known) => Meaning::of(known),
            None => Meaning::info(
                format!("Sensor-specific offset 0x{offset:02x}"),
                format!("{type_name} event (offset 0x{offset:02x})"),
                "bmc_event",
                Kind::Event,
            ),
        },
        et @ 0x02..=0x0C => {
            let text = sel_tables::generic(et, offset).map_or_else(
                || format!("Generic event 0x{et:02x} offset 0x{offset:02x}"),
                str::to_owned,
            );
            Meaning::info(
                text.clone(),
                format!("{type_name}: {text}"),
                "bmc_event",
                Kind::Event,
            )
        }
        et => Meaning::info(
            format!("Event type 0x{et:02x} offset 0x{offset:02x}"),
            format!("{type_name} event (type 0x{et:02x}, offset 0x{offset:02x})"),
            "bmc_event",
            Kind::Event,
        ),
    }
}

/// A threshold event of a sensor of `sensor_type` at `offset`.
fn threshold(sensor_type: u8, offset: u8, type_name: &str) -> Meaning {
    let Some(th) = THRESHOLDS.get(usize::from(offset)) else {
        return Meaning::info(
            format!("Threshold offset 0x{offset:02x}"),
            format!("{type_name} threshold event (offset 0x{offset:02x})"),
            "bmc_threshold",
            Kind::Condition,
        );
    };
    let level = match th.level {
        Level::NonCritical => "non-critical",
        Level::Critical => "critical",
        Level::NonRecoverable => "non-recoverable",
    };
    let side = if th.lower { "lower" } else { "upper" };
    let phrase = if th.outward {
        let way = if th.lower { "below" } else { "above" };
        format!("{type_name} {way} its {side} {level} threshold")
    } else {
        format!("{type_name} crossed its {side} {level} threshold back")
    };
    let serious = th.level != Level::NonCritical;
    let (code, severity) = match sensor_type {
        0x01 if serious => ("thermal_critical", pb::HardwareSeverity::Warning),
        0x01 => ("thermal_threshold", pb::HardwareSeverity::Warning),
        0x04 if serious && th.lower => ("fan_failed", pb::HardwareSeverity::Critical),
        0x04 => ("fan_threshold", pb::HardwareSeverity::Warning),
        _ => ("bmc_threshold", pb::HardwareSeverity::Info),
    };
    Meaning {
        text: th.text.to_owned(),
        phrase,
        code,
        severity,
        kind: Kind::Condition,
    }
}

/// An OEM entry: `info`, of the board, its bytes past the record type in hex.
fn oem(record: &SelRecord) -> Decoded {
    let mut hex = String::new();
    let first = if record.record_type >= 0xE0 { 3 } else { 7 };
    for byte in &record.bytes[first..] {
        let _ = write!(hex, "{byte:02x}");
    }
    let time = if record.record_type >= 0xE0 {
        String::new()
    } else {
        format!("{} | ", record.time.columns())
    };
    Decoded {
        component: pb::HardwareComponent::Board,
        severity: pb::HardwareSeverity::Info,
        kind: Kind::Event,
        deassertion: false,
        code: "bmc_oem".to_owned(),
        subject: "board".to_owned(),
        message: format!(
            "OEM record of type 0x{:02x} in the BMC's log",
            record.record_type
        ),
        raw: format!(
            "{:04x} | {time}OEM record 0x{:02x} | {hex}",
            record.id, record.record_type
        ),
        condition: String::new(),
        counted: None,
    }
}

#[cfg(test)]
#[path = "sel_tests.rs"]
mod tests;
