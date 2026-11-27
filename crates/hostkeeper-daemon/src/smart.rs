//! SMART, each physical disk's own health record, read through smartmontools' `smartctl`
//! (docs/design/host-health-alerts.md sections 3.1, 4.2 and 4.5): the source `smart` of a Linux
//! host, read every [`SMART_EVERY_SECS`].
//!
//! **The disks** are the entries of [`SYS_BLOCK`] under the host's root that are physical: a
//! name of `[a-z0-9]`, 1 to 32 bytes (so never one that starts with `-`), not a virtual device
//! (a name starting with one of [`VIRTUAL_PREFIXES`]: `loop`, `ram`, `zram`, `dm-`, `md`, `nbd`,
//! `sr`, `fd`), with a `device` entry, and not removable (its `removable` is not `1`). At most
//! [`MAX_SMART_DISKS`] are read, in the order of their names.
//!
//! **The program** is [`SMARTCTL`] with the fixed arguments [`SMARTCTL_ARGS`] and the disk's
//! `/dev/<name>` ([`smartctl_args`]): compact JSON, the identity, the health and the attributes,
//! and `--nocheck=standby`, so a disk asleep is not spun up. It writes nothing to the disk: no
//! self-test, no option that sets anything. It runs with an empty environment, its output kept to
//! 64 KiB, killed at [`SMARTCTL_DEADLINE`]. Its exit status is read bit by bit, as `smartctl(8)`
//! documents: bit 0 (the command line did not parse) makes the disk unreadable; bit 1 (the device
//! did not open) is a disk asleep when smartctl says the device is in STANDBY or SLEEP mode (read
//! next time), a disk with no SMART when it says so (`unsupported`), and unreadable otherwise;
//! bit 2 (a SMART command failed) is unreadable when no health verdict came and smartctl does not
//! say the disk has no SMART (then `unsupported`); bit 3 (the disk says
//! it is failing) is failing whatever the JSON says.
//!
//! **What is judged**, per disk, keyed by the 8-byte SHA-256 digest of its serial (when it reports
//! none, the key of the disk last read under its name, else `name-<disk>`):
//!
//! - **failing**: the SMART health check failed (`smart_status.passed` false, or bit 3), an NVMe
//!   `critical_warning` is not 0, or an NVMe disk's available spare is under its threshold. A disk
//!   that becomes failing gives an `asserted` record `disk_failing`, critical; one that passes
//!   again, or is gone (judged once every disk is read: not read, and its name no longer listed
//!   or read with another serial; a disk asleep or unreadable under its own name is kept), a
//!   `deasserted` record about the assertion, critical too, so a queue that folds `info` records never loses
//!   the end of a condition.
//! - **media errors**: an NVMe disk's `media_errors`, an ATA disk's reallocated (attribute 5) plus
//!   pending (197) sectors, a SCSI disk's grown defects. A count larger than the last read gives
//!   an `event` record `disk_media_errors`, a warning; a disk's first read with a count above 0
//!   gives one as `info`, so old damage is kept in the log and raises nothing.
//!
//! The mark ([`SmartMark`], JSON) keeps each disk's name, its assertion's `event_id` and its last
//! count, so a record is made once per change and a restart of the daemon makes none again. A
//! record's `event_id` is `smart:<key>:failing:<second>`, `smart:<key>:passing:<second>` or
//! `smart:<key>:media:<count>`, `<key>` the disk's key above; its `subject` the disk's name; its
//! `raw` the facts it was judged on, as one line.
//!
//! **The source's state**: `unsupported` when [`SMARTCTL`] is not installed (smartmontools comes
//! with the host group's config package, section 3.3) or no physical disk is listed (a listing
//! that is empty still ends the assertions of the disks gone from it); `unreadable`
//! when [`SYS_BLOCK`] cannot be listed, when more than [`MAX_SMART_DISKS`] disks are listed, or
//! when any disk could not be read; else `unsupported` when any disk reports no SMART; else `ok`.
//! The reason names the first disk at fault. The disks that were read are judged whatever the
//! state.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hostkeeper_launchd::launchctl::OUTPUT_MAX;
use hostkeeper_launchd::{Launchctl, LaunchctlError};
use hostkeeper_proto::health::{MAX_DISK_NAME, MAX_RAW};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::hardware_sources::{HardwareSource, SourceRead};
use crate::health_state::cut;

/// The source's token.
pub const SOURCE: &str = "smart";

/// The program, as smartmontools installs it on Debian, Ubuntu, RHEL and Alpine.
pub const SMARTCTL: &str = "/usr/sbin/smartctl";

/// The seconds between two reads: an hour.
pub const SMART_EVERY_SECS: u64 = 60 * 60;

/// How long one disk's `smartctl` may take before it is killed.
pub const SMARTCTL_DEADLINE: Duration = Duration::from_secs(60);

/// The block devices, under the host's root.
pub const SYS_BLOCK: &str = "sys/block";

/// The most disks one read reads.
pub const MAX_SMART_DISKS: usize = 64;

/// The arguments before the disk: compact JSON; leave a disk in standby asleep; the identity
/// (serial), the health verdict and the attributes (an NVMe disk's health log).
pub const SMARTCTL_ARGS: [&str; 5] = [
    "--json=c",
    "--nocheck=standby",
    "--info",
    "--health",
    "--attributes",
];

/// The starts of the names of block devices that are not physical disks.
pub const VIRTUAL_PREFIXES: [&str; 8] = ["loop", "ram", "zram", "dm-", "md", "nbd", "sr", "fd"];

/// The code of a failing disk's records.
pub const CODE_FAILING: &str = "disk_failing";

/// The code of a disk's grown media errors.
pub const CODE_MEDIA: &str = "disk_media_errors";

/// The reason of a host with no physical disk listed.
pub const NO_DISK: &str = "no physical disk is listed";

/// `smartctl`'s exit status bits (`smartctl(8)`, "RETURN VALUES").
const BIT_PARSE: i32 = 1;
const BIT_OPEN: i32 = 1 << 1;
const BIT_COMMAND: i32 = 1 << 2;
const BIT_FAILING: i32 = 1 << 3;

/// The ATA attributes counted as media errors: reallocated and pending sectors.
const ATA_REALLOCATED: u64 = 5;
const ATA_PENDING: u64 = 197;

/// The full argument list of one disk's read: [`SMARTCTL_ARGS`] and `/dev/<disk>`.
#[must_use]
pub fn smartctl_args(disk: &str) -> Vec<String> {
    SMARTCTL_ARGS
        .iter()
        .map(|a| (*a).to_owned())
        .chain(std::iter::once(format!("/dev/{disk}")))
        .collect()
}

/// The first 8 bytes of the SHA-256 of `serial`, as a big-endian integer (the digest of
/// `BaselineDisk.serial_digest`).
#[must_use]
pub fn serial_digest(serial: &str) -> u64 {
    let digest = Sha256::digest(serial.as_bytes());
    let mut first = [0u8; 8];
    first.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(first)
}

/// What the source keeps between reads: each disk it has read, by its key.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SmartMark {
    pub disks: BTreeMap<String, DiskMark>,
}

/// What the source keeps of one disk.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiskMark {
    /// Its block device name when last read.
    pub name: String,
    /// The `event_id` of its `disk_failing` assertion while it is failing; empty otherwise.
    pub failing: String,
    /// Its media error count when last read; none when it reports none.
    pub media: Option<u64>,
}

/// What `smartctl` said of one disk, as judged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskFacts {
    /// Its serial; empty when it reports none.
    pub serial: String,
    /// Why it is failing, a message's start; none when it is not.
    pub failing: Option<String>,
    /// Its media error count; none when it reports none.
    pub media: Option<u64>,
    /// The facts as one line, the records' `raw`.
    pub raw: String,
}

/// What one disk's read came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiskRead {
    /// It was read.
    Read(DiskFacts),
    /// It is asleep, and was left so.
    Standby,
    /// It reports no SMART, for the reason given.
    Unsupported(String),
    /// Reading it failed, for the reason given.
    Unreadable(String),
}

/// The physical disks of [`SYS_BLOCK`] under `root`, by name (the module's doc).
///
/// # Errors
/// The directory cannot be listed.
pub fn list_disks(root: &Path) -> std::io::Result<Vec<String>> {
    let dir = root.join(SYS_BLOCK);
    let mut names: Vec<String> = fs::read_dir(&dir)?
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| is_physical(&dir, name))
        .collect();
    names.sort();
    Ok(names)
}

fn is_physical(dir: &Path, name: &str) -> bool {
    let plain = !name.is_empty()
        && name.len() <= MAX_DISK_NAME
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    plain
        && !VIRTUAL_PREFIXES.iter().any(|p| name.starts_with(p))
        && dir.join(name).join("device").exists()
        && !fs::read_to_string(dir.join(name).join("removable"))
            .is_ok_and(|text| text.trim() == "1")
}

/// Judges what `smartctl` printed for `disk` (`stdout`), and its exit status `code` (none when
/// it was ended by a signal), by the module's rules.
#[must_use]
pub fn parse_smartctl(disk: &str, code: Option<i32>, stdout: &[u8]) -> DiskRead {
    if stdout.len() >= OUTPUT_MAX {
        return DiskRead::Unreadable(format!("/dev/{disk}: output past {OUTPUT_MAX} bytes"));
    }
    let Some(code) = code else {
        return DiskRead::Unreadable(format!("/dev/{disk}: smartctl ended by a signal"));
    };
    let Ok(json) = serde_json::from_slice::<Value>(stdout) else {
        return DiskRead::Unreadable(format!("/dev/{disk}: output is not JSON (exit {code})"));
    };
    let messages: Vec<&str> = json["smartctl"]["messages"]
        .as_array()
        .map(|list| list.iter().filter_map(|m| m["string"].as_str()).collect())
        .unwrap_or_default();
    let first = messages.first().copied().unwrap_or("no message");
    let says = |words: &[&str]| messages.iter().any(|m| words.iter().any(|w| m.contains(w)));
    let no_smart = says(&[
        "SMART support is: Unavailable",
        "Unable to detect device type",
        "Unknown USB bridge",
    ]);
    if code & BIT_PARSE != 0 {
        return DiskRead::Unreadable(format!("/dev/{disk}: smartctl refused: {first}"));
    }
    if code & BIT_OPEN != 0 {
        if says(&["STANDBY", "SLEEP"]) {
            return DiskRead::Standby;
        }
        if no_smart {
            return DiskRead::Unsupported(format!("/dev/{disk} reports no SMART"));
        }
        return DiskRead::Unreadable(format!("/dev/{disk} did not open: {first}"));
    }
    let passed = json["smart_status"]["passed"]
        .as_bool()
        .or((code & BIT_FAILING != 0).then_some(false));
    if passed.is_none() {
        if code & BIT_COMMAND != 0 && !no_smart {
            return DiskRead::Unreadable(format!("/dev/{disk}: a SMART command failed: {first}"));
        }
        return DiskRead::Unsupported(format!("/dev/{disk} reports no SMART"));
    }
    DiskRead::Read(facts(&json, code, passed == Some(true)))
}

fn facts(json: &Value, code: i32, passed: bool) -> DiskFacts {
    let nvme = &json["nvme_smart_health_information_log"];
    let warning = nvme["critical_warning"].as_u64().unwrap_or(0);
    let spare = nvme["available_spare"].as_u64();
    let threshold = nvme["available_spare_threshold"].as_u64();
    let low_spare = matches!((spare, threshold), (Some(s), Some(t)) if s < t);
    let failing = if warning != 0 {
        Some(format!("NVMe critical warning 0x{warning:02x}"))
    } else if low_spare {
        Some(format!(
            "available spare {}% under its threshold {}%",
            spare.unwrap_or(0),
            threshold.unwrap_or(0)
        ))
    } else if !passed || code & BIT_FAILING != 0 {
        Some("SMART health check failed".to_owned())
    } else {
        None
    };
    let ata = |id: u64| {
        json["ata_smart_attributes"]["table"]
            .as_array()
            .and_then(|table| table.iter().find(|a| a["id"].as_u64() == Some(id)))
            .and_then(|a| a["raw"]["value"].as_u64())
    };
    let (reallocated, pending) = (ata(ATA_REALLOCATED), ata(ATA_PENDING));
    let media_errors = nvme["media_errors"].as_u64();
    let grown = json["scsi_grown_defect_list"].as_u64();
    let media = media_errors.or(grown).or(match (reallocated, pending) {
        (None, None) => None,
        (r, p) => Some(r.unwrap_or(0).saturating_add(p.unwrap_or(0))),
    });
    let mut raw = format!("exit_status={code} passed={passed}");
    let fields = [
        ("critical_warning", nvme["critical_warning"].as_u64()),
        ("available_spare", spare),
        ("available_spare_threshold", threshold),
        ("media_errors", media_errors),
        ("reallocated_sectors", reallocated),
        ("pending_sectors", pending),
        ("grown_defects", grown),
    ];
    for (name, value) in fields {
        if let Some(value) = value {
            raw.push_str(&format!(" {name}={value}"));
        }
    }
    DiskFacts {
        serial: json["serial_number"]
            .as_str()
            .map(|s| s.trim().to_owned())
            .unwrap_or_default(),
        failing,
        media,
        raw: cut(&raw, MAX_RAW),
    }
}

/// SMART through `smartctl`, over the disks of a host's root.
pub struct SmartSource {
    root: PathBuf,
    smartctl: PathBuf,
    runner: Box<dyn Launchctl>,
}

impl SmartSource {
    /// The source of the disks under `root`, run as `smartctl` (whose absence is `unsupported`)
    /// through `runner`.
    #[must_use]
    pub fn new(root: &Path, smartctl: &Path, runner: Box<dyn Launchctl>) -> SmartSource {
        SmartSource {
            root: root.to_path_buf(),
            smartctl: smartctl.to_path_buf(),
            runner,
        }
    }

    fn read_disk(&self, disk: &str) -> DiskRead {
        match self.runner.run(&smartctl_args(disk)) {
            Ok(output) => parse_smartctl(disk, output.code, &output.stdout),
            Err(LaunchctlError::TimedOut { .. }) => {
                DiskRead::Unreadable(format!("/dev/{disk}: smartctl timed out"))
            }
            Err(LaunchctlError::Spawn { why, .. }) => {
                DiskRead::Unreadable(format!("smartctl cannot run: {why}"))
            }
        }
    }
}

/// The records of a read are made here, with the mark they change.
struct Judge {
    mark: SmartMark,
    now: u64,
    events: Vec<pb::HardwareEvent>,
    /// The disks read this time: their names, by key.
    read: BTreeMap<String, String>,
}

impl Judge {
    /// A record of `disk` with `code`, `raw` and `message`, read now: an `event`, `info`, about
    /// nothing, with no `event_id` yet.
    fn base(&self, disk: &str, code: &str, message: String, raw: &str) -> pb::HardwareEvent {
        pb::HardwareEvent {
            event_id: String::new(),
            time_unix: self.now,
            of_previous_boot: false,
            component: pb::HardwareComponent::Disk.into(),
            subject: disk.to_owned(),
            severity: pb::HardwareSeverity::Info.into(),
            state: pb::HardwareEventState::Event.into(),
            source: SOURCE.to_owned(),
            code: code.to_owned(),
            message,
            raw: raw.to_owned(),
            about: String::new(),
        }
    }

    /// Ends the assertion of the disk of `key`, when it has one, with `message`.
    fn deassert(&mut self, key: &str, disk: &str, message: String, raw: &str) {
        let Some(assertion) = self
            .mark
            .disks
            .get_mut(key)
            .map(|d| std::mem::take(&mut d.failing))
            .filter(|id| !id.is_empty())
        else {
            return;
        };
        let event = pb::HardwareEvent {
            event_id: format!("{SOURCE}:{key}:passing:{}", self.now),
            severity: pb::HardwareSeverity::Critical.into(),
            state: pb::HardwareEventState::Deasserted.into(),
            about: assertion,
            ..self.base(disk, CODE_FAILING, message, raw)
        };
        self.events.push(event);
    }

    /// The key of `disk` with `serial`: the digest of its serial; with none, the key of the disk
    /// last read under its name, else `name-<disk>`.
    fn key(&self, disk: &str, serial: &str) -> String {
        if !serial.is_empty() {
            return format!("{:016x}", serial_digest(serial));
        }
        self.mark
            .disks
            .iter()
            .find(|(_, d)| d.name == disk)
            .map_or_else(|| format!("name-{disk}"), |(k, _)| k.clone())
    }

    /// Judges the facts read of `disk`.
    fn disk(&mut self, disk: &str, facts: &DiskFacts) {
        let key = self.key(disk, &facts.serial);
        self.read.insert(key.clone(), disk.to_owned());
        let before = self.mark.disks.get(&key).cloned();
        let entry = self.mark.disks.entry(key.clone()).or_default();
        entry.name = disk.to_owned();
        let failing_held = !entry.failing.is_empty();
        match (&facts.failing, failing_held) {
            (Some(why), false) => {
                let id = format!("{SOURCE}:{key}:failing:{}", self.now);
                entry.failing.clone_from(&id);
                let message = format!("Disk {disk} is failing: {why}");
                let event = pb::HardwareEvent {
                    event_id: id,
                    severity: pb::HardwareSeverity::Critical.into(),
                    state: pb::HardwareEventState::Asserted.into(),
                    ..self.base(disk, CODE_FAILING, message, &facts.raw)
                };
                self.events.push(event);
            }
            (None, true) => self.deassert(
                &key,
                disk,
                format!("Disk {disk} passes its SMART health check again"),
                &facts.raw,
            ),
            _ => {}
        }
        if let Some(count) = facts.media {
            let judged = match before.as_ref().map(|d| d.media) {
                None | Some(None) if count > 0 => Some((
                    pb::HardwareSeverity::Info,
                    format!("Disk {disk} has {count} media errors at its first read"),
                )),
                Some(Some(was)) if count > was => Some((
                    pb::HardwareSeverity::Warning,
                    format!("Media errors on disk {disk} grew from {was} to {count}"),
                )),
                _ => None,
            };
            if let Some((severity, message)) = judged {
                let event = pb::HardwareEvent {
                    event_id: format!("{SOURCE}:{key}:media:{count}"),
                    severity: severity.into(),
                    ..self.base(disk, CODE_MEDIA, message, &facts.raw)
                };
                self.events.push(event);
            }
        }
        self.mark.disks.entry(key).or_default().media = facts.media;
    }

    /// After every disk is read: a disk not read this time is gone when its name is no longer in
    /// `listed`, or when another disk was read under its name; one asleep or unreadable under its
    /// name is kept.
    fn sweep(&mut self, listed: &BTreeSet<&str>) {
        let names: BTreeSet<String> = self.read.values().cloned().collect();
        let gone: Vec<(String, String)> = self
            .mark
            .disks
            .iter()
            .filter(|(k, _)| !self.read.contains_key(*k))
            .filter_map(|(k, d)| {
                if !listed.contains(d.name.as_str()) {
                    Some((k.clone(), "it is no longer listed".to_owned()))
                } else if names.contains(&d.name) {
                    Some((k.clone(), format!("{} now holds another disk", d.name)))
                } else {
                    None
                }
            })
            .collect();
        for (key, why) in gone {
            self.gone(&key, &why);
        }
    }

    /// The disk of `key` is gone (`why`): its assertion ends, and it is forgotten.
    fn gone(&mut self, key: &str, why: &str) {
        let name = self
            .mark
            .disks
            .get(key)
            .map(|d| d.name.clone())
            .unwrap_or_default();
        self.deassert(
            key,
            &name,
            format!("Disk {name} is no longer failing: {why}"),
            why,
        );
        self.mark.disks.remove(key);
    }
}

impl HardwareSource for SmartSource {
    fn name(&self) -> &'static str {
        SOURCE
    }

    fn every_secs(&self) -> u64 {
        SMART_EVERY_SECS
    }

    fn read(&mut self, mark: Option<&str>, now_unix: u64) -> SourceRead {
        if !self.smartctl.exists() {
            return SourceRead::unsupported(format!(
                "{} is not installed",
                self.smartctl.display()
            ));
        }
        let disks = match list_disks(&self.root) {
            Ok(disks) => disks,
            Err(e) => return SourceRead::unreadable(format!("{SYS_BLOCK} is not listed: {e}")),
        };
        let mut judge = Judge {
            mark: mark
                .and_then(|text| serde_json::from_str(text).ok())
                .unwrap_or_default(),
            now: now_unix,
            events: Vec::new(),
            read: BTreeMap::new(),
        };
        let mut unreadable = None;
        let mut unsupported = None;
        if disks.len() > MAX_SMART_DISKS {
            unreadable = Some(format!(
                "{} disks listed, the first {MAX_SMART_DISKS} read",
                disks.len()
            ));
        }
        for disk in disks.iter().take(MAX_SMART_DISKS) {
            match self.read_disk(disk) {
                DiskRead::Read(facts) => judge.disk(disk, &facts),
                DiskRead::Standby => {}
                DiskRead::Unsupported(why) => {
                    unsupported.get_or_insert(why);
                }
                DiskRead::Unreadable(why) => {
                    unreadable.get_or_insert(why);
                }
            }
        }
        judge.sweep(&disks.iter().map(String::as_str).collect());
        if disks.is_empty() {
            if judge.events.is_empty() {
                return SourceRead::unsupported(NO_DISK.to_owned());
            }
            unsupported = Some(NO_DISK.to_owned());
        }
        let text = serde_json::to_string(&judge.mark).unwrap_or_default();
        let (status, reason) = match (unreadable, unsupported) {
            (Some(why), _) => (pb::SourceStatus::Unreadable, why),
            (None, Some(why)) => (pb::SourceStatus::Unsupported, why),
            (None, None) => (pb::SourceStatus::Ok, String::new()),
        };
        SourceRead {
            status,
            reason,
            mark: Some(text),
            events: judge.events,
        }
    }
}
