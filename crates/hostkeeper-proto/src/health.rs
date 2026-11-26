//! The bounds of a host's hardware health (`hostkeeper.agent.v1.HostHealth`, the `health` of a
//! heartbeat) and of the health facts of a service (`ServiceBeat.health_reason`, `health_body` and
//! `restarts`), as `hostkeeper/agent/v1/host_health.proto` and `agent.proto` give them field by
//! field, and [`check_health`] and [`check_service`], which refuse a message that breaks one.
//!
//! A `HostHealth` is also at most [`MAX_HEALTH_BYTES`] encoded, whatever its fields hold: its
//! lists at their bounds would take more, so the sender keeps the whole within it (the memory
//! devices only in a boot's first heartbeats, fewer records), and [`check_health`] refuses one that
//! is not. Together with the maintenance report ([`crate::maintenance::MAX_REPORT_BYTES`]) and the
//! prerequisites report it keeps the reports a heartbeat never drops well inside its 16 KiB.

use prost::Message as _;

use crate::hostkeeper::agent::v1::{HostHealth, ServiceBeat};

/// The most bytes a `HostHealth` encodes to.
pub const MAX_HEALTH_BYTES: usize = 4096;

/// The most records of `events`.
pub const MAX_EVENTS: usize = 6;
/// The most entries of `sources`.
pub const MAX_SOURCES: usize = 12;
/// The most devices of `baseline.memory_devices`.
pub const MAX_MEMORY_DEVICES: usize = 32;
/// The most disks of `baseline.disks`.
pub const MAX_DISKS: usize = 16;
/// The most interfaces of `baseline.nics`.
pub const MAX_NICS: usize = 8;

/// The most bytes of a record's `event_id` and of its `about`; a record's `event_id` is at least
/// one byte.
pub const MAX_EVENT_ID: usize = 64;
/// The most bytes of a record's `subject`.
pub const MAX_SUBJECT: usize = 32;
/// The most bytes of a record's `source`, and of a source state's `source`.
pub const MAX_SOURCE: usize = 16;
/// The most bytes of a record's `code`.
pub const MAX_CODE: usize = 32;
/// The most bytes of a record's `message`.
pub const MAX_MESSAGE: usize = 120;
/// The most bytes of a record's `raw`.
pub const MAX_RAW: usize = 256;
/// The most bytes of `boot_id`.
pub const MAX_BOOT_ID: usize = 64;
/// The most bytes of a memory device's `locator`.
pub const MAX_LOCATOR: usize = 32;
/// The most bytes of `baseline.smt`.
pub const MAX_SMT: usize = 16;
/// The most bytes of a disk's `name`.
pub const MAX_DISK_NAME: usize = 32;
/// The most bytes of an interface's `name`.
pub const MAX_NIC_NAME: usize = 16;
/// The most bytes of a source state's `reason`.
pub const MAX_SOURCE_REASON: usize = 64;

/// The most bytes of a service's `health_reason`.
pub const MAX_HEALTH_REASON: usize = 64;
/// The most bytes of a service's `health_body`.
pub const MAX_HEALTH_BODY: usize = 160;
/// The most bytes of a service's `restarts.last_result`.
pub const MAX_LAST_RESULT: usize = 32;

/// The bound a `HostHealth` or a service's health facts break. A field is named by its path in
/// the message, a list's entries all by the list's name (`events.raw`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HealthError {
    /// A list holds more entries than its bound.
    #[error("{field} holds {count} entries, more than {max}")]
    TooMany {
        /// The list.
        field: &'static str,
        /// How many entries it holds.
        count: usize,
        /// Its bound.
        max: usize,
    },
    /// A text is longer than its bound.
    #[error("{field} is {len} bytes, more than {max}")]
    TooLong {
        /// The text.
        field: &'static str,
        /// Its length in bytes.
        len: usize,
        /// Its bound.
        max: usize,
    },
    /// A text that must be given is empty.
    #[error("{field} is empty")]
    Empty {
        /// The text.
        field: &'static str,
    },
    /// The whole message encodes to more bytes than [`MAX_HEALTH_BYTES`].
    #[error("it encodes to {len} bytes, more than {MAX_HEALTH_BYTES}")]
    TooLarge {
        /// Its encoded length.
        len: usize,
    },
}

/// Whether `health` keeps every bound: first each list's count, in the order of the fields, then
/// each text, in the same order, then whether every record has an `event_id`, then its encoded
/// length.
///
/// # Errors
/// [`HealthError`]: the first bound it breaks.
pub fn check_health(health: &HostHealth) -> Result<(), HealthError> {
    let baseline = health.baseline.as_ref();
    let counts = [
        ("events", health.events.len(), MAX_EVENTS),
        (
            "baseline.memory_devices",
            baseline.map_or(0, |b| b.memory_devices.len()),
            MAX_MEMORY_DEVICES,
        ),
        (
            "baseline.disks",
            baseline.map_or(0, |b| b.disks.len()),
            MAX_DISKS,
        ),
        (
            "baseline.nics",
            baseline.map_or(0, |b| b.nics.len()),
            MAX_NICS,
        ),
        ("sources", health.sources.len(), MAX_SOURCES),
    ];
    if let Some((field, count, max)) = counts.into_iter().find(|&(_, count, max)| count > max) {
        return Err(HealthError::TooMany { field, count, max });
    }
    let events = health.events.iter().flat_map(|e| {
        [
            ("events.event_id", e.event_id.as_str(), MAX_EVENT_ID),
            ("events.subject", e.subject.as_str(), MAX_SUBJECT),
            ("events.source", e.source.as_str(), MAX_SOURCE),
            ("events.code", e.code.as_str(), MAX_CODE),
            ("events.message", e.message.as_str(), MAX_MESSAGE),
            ("events.raw", e.raw.as_str(), MAX_RAW),
            ("events.about", e.about.as_str(), MAX_EVENT_ID),
        ]
    });
    let baseline_texts = baseline.into_iter().flat_map(|b| {
        let devices = b.memory_devices.iter().map(|d| {
            (
                "baseline.memory_devices.locator",
                d.locator.as_str(),
                MAX_LOCATOR,
            )
        });
        let disks = b
            .disks
            .iter()
            .map(|d| ("baseline.disks.name", d.name.as_str(), MAX_DISK_NAME));
        let nics = b
            .nics
            .iter()
            .map(|n| ("baseline.nics.name", n.name.as_str(), MAX_NIC_NAME));
        devices
            .chain(std::iter::once(("baseline.smt", b.smt.as_str(), MAX_SMT)))
            .chain(disks)
            .chain(nics)
    });
    let boot_id = [("boot_id", health.boot_id.as_str(), MAX_BOOT_ID)];
    let sources = health.sources.iter().flat_map(|s| {
        [
            ("sources.source", s.source.as_str(), MAX_SOURCE),
            ("sources.reason", s.reason.as_str(), MAX_SOURCE_REASON),
        ]
    });
    if let Some((field, text, max)) = events
        .chain(baseline_texts)
        .chain(boot_id)
        .chain(sources)
        .find(|&(_, text, max)| text.len() > max)
    {
        return Err(HealthError::TooLong {
            field,
            len: text.len(),
            max,
        });
    }
    if health.events.iter().any(|e| e.event_id.is_empty()) {
        return Err(HealthError::Empty {
            field: "events.event_id",
        });
    }
    let len = health.encoded_len();
    if len > MAX_HEALTH_BYTES {
        return Err(HealthError::TooLarge { len });
    }
    Ok(())
}

/// Whether the health facts of `service` keep their bounds: `health_reason`, `health_body`, then
/// `restarts.last_result`. The rest of the entry is not looked at.
///
/// # Errors
/// [`HealthError::TooLong`]: the first text over its bound.
pub fn check_service(service: &ServiceBeat) -> Result<(), HealthError> {
    let texts = [
        (
            "health_reason",
            service.health_reason.as_deref().unwrap_or_default(),
            MAX_HEALTH_REASON,
        ),
        (
            "health_body",
            service.health_body.as_deref().unwrap_or_default(),
            MAX_HEALTH_BODY,
        ),
        (
            "restarts.last_result",
            service
                .restarts
                .as_ref()
                .map_or("", |r| r.last_result.as_str()),
            MAX_LAST_RESULT,
        ),
    ];
    match texts.into_iter().find(|&(_, text, max)| text.len() > max) {
        Some((field, text, max)) => Err(HealthError::TooLong {
            field,
            len: text.len(),
            max,
        }),
        None => Ok(()),
    }
}
