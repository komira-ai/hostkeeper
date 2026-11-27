//! A Mac's internal disk's own SMART verdict, as `diskutil` reports it
//! (docs/design/host-health-alerts.md sections 3.2, 4.2 and 4.5): the source [`SOURCE`], read
//! every [`DISKUTIL_EVERY_SECS`] by [`DiskutilSource`] on a worker of its own
//! ([`crate::hardware_sources`]).
//!
//! **The program** is [`DISKUTIL`] with the fixed arguments [`DISKUTIL_ARGS`] (`info -plist
//! disk0`), run through the [`Launchctl`] port: an empty environment, its output capped, killed at
//! [`crate::mac::TOOL_DEADLINE`]. It reads the disk's description and writes nothing: no
//! `verifyVolume`, no repair. Its output is an XML property list ([`parse_info`]; `-plist` prints
//! no other form, so text of any other form is not one) whose `SMARTStatus`
//! is the disk's verdict, and whose `MediaName` names the disk in the record's `raw`.
//!
//! **What is judged** ([`judge`]): `Failing` gives an `asserted` record `disk_failing`, critical
//! (component `disk`, subject `disk0`); `Verified` after it a `deasserted` record about the
//! assertion, critical too, so a queue that folds `info` records never loses the end of the
//! condition (as Linux's SMART source does, [`crate::smart`]). The mark is the open assertion's
//! `event_id`, empty when none is open, so a disk that stays failing makes one record, and a
//! restart of the daemon none again. A record's `event_id` is `diskutil:disk0:failing:<second>`
//! or `diskutil:disk0:passing:<second>`; its `raw` the verdict and the disk's name, as one line.
//!
//! **The source's state**: `ok` at `Verified` and `Failing`; `unsupported` at `Not Supported` (the
//! disk reports no SMART verdict), with an open assertion kept as it is; `unreadable`, with its
//! reason and the mark kept, when `diskutil` does not run, does not exit 0, prints no property
//! list, or prints one with no `SMARTStatus` or with a verdict this release does not know.

use std::path::Path;

use hostkeeper_launchd::{Launchctl, LaunchctlProcess};
use hostkeeper_proto::health::{MAX_EVENT_ID, MAX_MESSAGE, MAX_RAW};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use plist::Value;

use crate::hardware_sources::{HardwareSource, SourceRead};
use crate::health_state::cut;
use crate::mac::TOOL_DEADLINE;
use crate::mac_health::run_bytes;
use crate::smart::CODE_FAILING;

/// The source's token.
pub const SOURCE: &str = "diskutil";

/// The program.
pub const DISKUTIL: &str = "/usr/sbin/diskutil";

/// The disk whose verdict is read: a Mac's internal disk.
pub const DISK: &str = "disk0";

/// The program's arguments: the description of [`DISK`] as a property list.
pub const DISKUTIL_ARGS: [&str; 3] = ["info", "-plist", DISK];

/// The seconds between two reads: 15 minutes.
pub const DISKUTIL_EVERY_SECS: u64 = 15 * 60;

/// The key of the verdict in `diskutil info -plist`.
pub const SMART_STATUS_KEY: &str = "SMARTStatus";

/// The key of the disk's name in `diskutil info -plist`.
pub const MEDIA_NAME_KEY: &str = "MediaName";

/// A disk's SMART verdict, as `diskutil` words it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SmartStatus {
    /// `Verified`: the disk says it is healthy.
    Verified,
    /// `Failing`: the disk says it is failing.
    Failing,
    /// `Not Supported`: the disk reports no verdict.
    NotSupported,
    /// A word this release does not know, as printed.
    Other(String),
}

impl SmartStatus {
    /// The verdict `word` names.
    #[must_use]
    pub fn from_word(word: &str) -> SmartStatus {
        match word {
            "Verified" => SmartStatus::Verified,
            "Failing" => SmartStatus::Failing,
            "Not Supported" => SmartStatus::NotSupported,
            other => SmartStatus::Other(other.to_owned()),
        }
    }
}

/// What `diskutil info -plist` says of the disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskInfo {
    /// Its SMART verdict.
    pub status: SmartStatus,
    /// The verdict as printed.
    pub word: String,
    /// Its `MediaName`; empty when it has none.
    pub media: String,
}

/// Reads the XML property list `diskutil info -plist` printed.
///
/// # Errors
/// Why it gives no verdict: it is not an XML property list, its top is not a dictionary, or it
/// has no `SMARTStatus` string.
pub fn parse_info(bytes: &[u8]) -> Result<DiskInfo, String> {
    let dictionary = Value::from_reader_xml(bytes)
        .map_err(|_| "diskutil printed no property list".to_owned())?
        .into_dictionary()
        .ok_or_else(|| "diskutil printed no dictionary".to_owned())?;
    let word = dictionary
        .get(SMART_STATUS_KEY)
        .and_then(Value::as_string)
        .ok_or_else(|| format!("diskutil printed no {SMART_STATUS_KEY} of {DISK}"))?;
    let media = dictionary
        .get(MEDIA_NAME_KEY)
        .and_then(Value::as_string)
        .unwrap_or_default();
    Ok(DiskInfo {
        status: SmartStatus::from_word(word),
        word: word.to_owned(),
        media: media.to_owned(),
    })
}

/// Judges `info` at `now` against `open`, the open assertion's `event_id` (empty when none): the
/// new mark, and the record when the condition began or ended.
#[must_use]
pub fn judge(info: &DiskInfo, open: &str, now: u64) -> (String, Option<pb::HardwareEvent>) {
    let raw = cut(
        &format!(
            "{DISK} {SMART_STATUS_KEY} {} ({MEDIA_NAME_KEY} {})",
            info.word, info.media
        ),
        MAX_RAW,
    );
    match (&info.status, open.is_empty()) {
        (SmartStatus::Failing, true) => {
            let id = cut(&format!("{SOURCE}:{DISK}:failing:{now}"), MAX_EVENT_ID);
            let event = record(
                id.clone(),
                now,
                pb::HardwareEventState::Asserted,
                format!(
                    "Disk {DISK} says it is failing (SMART status {})",
                    info.word
                ),
                raw,
                String::new(),
            );
            (id, Some(event))
        }
        (SmartStatus::Verified, false) => {
            let event = record(
                cut(&format!("{SOURCE}:{DISK}:passing:{now}"), MAX_EVENT_ID),
                now,
                pb::HardwareEventState::Deasserted,
                format!(
                    "Disk {DISK} passes its SMART check again (SMART status {})",
                    info.word
                ),
                raw,
                open.to_owned(),
            );
            (String::new(), Some(event))
        }
        _ => (open.to_owned(), None),
    }
}

/// One record of the disk, critical.
fn record(
    event_id: String,
    now: u64,
    state: pb::HardwareEventState,
    message: String,
    raw: String,
    about: String,
) -> pb::HardwareEvent {
    pb::HardwareEvent {
        event_id,
        time_unix: now,
        of_previous_boot: false,
        component: pb::HardwareComponent::Disk.into(),
        subject: DISK.to_owned(),
        severity: pb::HardwareSeverity::Critical.into(),
        state: state.into(),
        source: SOURCE.to_owned(),
        code: CODE_FAILING.to_owned(),
        message: cut(&message, MAX_MESSAGE),
        raw,
        about,
    }
}

/// The internal disk's SMART verdict, through `diskutil`.
pub struct DiskutilSource {
    diskutil: Box<dyn Launchctl>,
}

impl DiskutilSource {
    /// The verdict `diskutil` (the program [`DISKUTIL_ARGS`] are given to) prints.
    #[must_use]
    pub fn new(diskutil: Box<dyn Launchctl>) -> DiskutilSource {
        DiskutilSource { diskutil }
    }

    /// The host's: [`DISKUTIL`] killed at [`TOOL_DEADLINE`].
    #[must_use]
    pub fn at_host() -> DiskutilSource {
        DiskutilSource::new(Box::new(LaunchctlProcess::new(
            Path::new(DISKUTIL),
            TOOL_DEADLINE,
        )))
    }
}

impl HardwareSource for DiskutilSource {
    fn name(&self) -> &'static str {
        SOURCE
    }

    fn every_secs(&self) -> u64 {
        DISKUTIL_EVERY_SECS
    }

    fn read(&mut self, mark: Option<&str>, now_unix: u64) -> SourceRead {
        let info = match run_bytes(self.diskutil.as_ref(), &DISKUTIL_ARGS, "diskutil")
            .and_then(|bytes| parse_info(&bytes))
        {
            Ok(info) => info,
            Err(why) => return SourceRead::unreadable(why),
        };
        match &info.status {
            SmartStatus::NotSupported => {
                return SourceRead::unsupported(format!("{DISK} reports no SMART status"));
            }
            SmartStatus::Other(word) => {
                return SourceRead::unreadable(format!("{DISK} has the SMART status {word:?}"));
            }
            SmartStatus::Verified | SmartStatus::Failing => {}
        }
        let open = mark.unwrap_or_default();
        let (kept, event) = judge(&info, open, now_unix);
        SourceRead {
            status: pb::SourceStatus::Ok,
            reason: String::new(),
            mark: (kept != open).then_some(kept),
            events: event.into_iter().collect(),
        }
    }
}
