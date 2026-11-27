//! The kernel's own files turned into hardware records (docs/design/host-health-alerts.md section
//! 3.1, the source `sysfs`): pure functions over what [`crate::kernel_sources`] read of `/proc`
//! and `/sys`, with no file, program or clock of their own.
//!
//! - **Taint** ([`taint_events`]): the flags of `/proc/sys/kernel/tainted` set in this boot that
//!   were not set at the last read, each once a boot: M (a machine check) `cpu`, critical
//!   `taint_machine_check`; B (a bad page) `memory`, warning `taint_bad_page`; D (the kernel died:
//!   an oops or a BUG) `cpu`, warning `taint_die`; L (a soft lockup) `cpu`, warning
//!   `taint_soft_lockup`. The other flags are not hardware and give nothing.
//! - **`HardwareCorrupted`** of `/proc/meminfo` ([`hardware_corrupted_kib`],
//!   [`corrupted_event`]): memory the kernel took out of use after hardware errors; `memory`,
//!   warning `memory_hardware_corrupted`, again each time it grows in a boot.
//! - **ext4 errors** ([`ext4_event`]): `/sys/fs/ext4/<device>/errors_count`, which the filesystem
//!   keeps on disk across boots; `disk` by the device, warning `filesystem_errors`, each time it
//!   grows. A filesystem on a loop, RAM or network block device is not read.
//! - **Read-only remounts** ([`remount_events`]): a filesystem of a block device (ext2, ext3,
//!   ext4, xfs, btrfs or f2fs) that was mounted read-write in this boot and is mounted read-only
//!   now is `disk` by the device, critical `filesystem_read_only`, asserted; it is deasserted when
//!   the mount is read-write again, is gone, or its boot ended ([`remounts_ended`]). A filesystem
//!   mounted read-only from the start is never asserted.
//! - **pstore** ([`pstore_event`]): a dump of the kernel's log that pstore kept across a reboot;
//!   `board`, critical `panic_hardware` when it is a panic or an oops and names an uncorrected
//!   hardware error ([`HARDWARE_WORDS`]), else warning `kernel_panic` for a panic and
//!   `kernel_oops` for an oops, else `info` `kernel_dump`.
//!   It is `of_previous_boot` when written before this boot began. Its `raw` is the panic's line,
//!   else the line naming the hardware, else the oops's, else the dump's first line.

use std::collections::{BTreeMap, BTreeSet};

use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use serde::{Deserialize, Serialize};

use crate::kernel_log::digest8;

/// The source token of every record of the kernel's files.
pub const SOURCE: &str = "sysfs";

/// The filesystem types whose read-only remounts are judged.
pub const REMOUNT_KINDS: [&str; 6] = ["ext2", "ext3", "ext4", "xfs", "btrfs", "f2fs"];

/// The words of a pstore dump that make its panic or oops a suspected hardware fault: an
/// uncorrected machine check, an uncorrected memory error, a fatal firmware error record or an
/// I/O check NMI. A corrected error's line (`Machine check events logged`, `event severity:
/// corrected`) is not one of them, so an old corrected error in a dump's log never makes a crash
/// a hardware fault.
pub const HARDWARE_WORDS: [&str; 6] = [
    "Machine Check Exception",
    "Fatal machine check",
    "Fatal local machine check",
    "Uncorrected hardware memory error",
    "event severity: fatal",
    "NMI: IOCK error",
];

/// One taint flag the health look reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaintFlag {
    /// Its bit in `/proc/sys/kernel/tainted`.
    pub bit: u32,
    /// Its letter, as the kernel prints it in an oops.
    pub letter: char,
    pub component: pb::HardwareComponent,
    pub severity: pb::HardwareSeverity,
    pub code: &'static str,
    pub message: &'static str,
}

/// The taint flags that are hardware's, or may be: M, B, D and L.
pub const TAINT_FLAGS: [TaintFlag; 4] = [
    TaintFlag {
        bit: 4,
        letter: 'M',
        component: pb::HardwareComponent::Cpu,
        severity: pb::HardwareSeverity::Critical,
        code: "taint_machine_check",
        message: "Kernel tainted M: the CPU raised a machine check",
    },
    TaintFlag {
        bit: 5,
        letter: 'B',
        component: pb::HardwareComponent::Memory,
        severity: pb::HardwareSeverity::Warning,
        code: "taint_bad_page",
        message: "Kernel tainted B: a bad page was found in memory",
    },
    TaintFlag {
        bit: 7,
        letter: 'D',
        component: pb::HardwareComponent::Cpu,
        severity: pb::HardwareSeverity::Warning,
        code: "taint_die",
        message: "Kernel tainted D: the kernel oopsed or hit a BUG",
    },
    TaintFlag {
        bit: 14,
        letter: 'L',
        component: pb::HardwareComponent::Cpu,
        severity: pb::HardwareSeverity::Warning,
        code: "taint_soft_lockup",
        message: "Kernel tainted L: a CPU was stuck in a soft lockup",
    },
];

/// The value of `/proc/sys/kernel/tainted`; none when it is not a number.
#[must_use]
pub fn parse_tainted(text: &str) -> Option<u64> {
    text.trim().parse().ok()
}

/// The bits of [`TAINT_FLAGS`] set in `tainted`.
#[must_use]
pub fn taint_bits(tainted: u64) -> u64 {
    TAINT_FLAGS
        .iter()
        .map(|flag| 1u64 << flag.bit)
        .filter(|bit| tainted & bit != 0)
        .fold(0, |bits, bit| bits | bit)
}

/// The records of the flags of [`TAINT_FLAGS`] set in `tainted` and not in `seen`, read at
/// `now_unix` in the boot `boot` ([`crate::kernel_log::boot_key`]).
#[must_use]
pub fn taint_events(tainted: u64, seen: u64, boot: &str, now_unix: u64) -> Vec<pb::HardwareEvent> {
    TAINT_FLAGS
        .iter()
        .filter(|flag| tainted & (1 << flag.bit) != 0 && seen & (1 << flag.bit) == 0)
        .map(|flag| pb::HardwareEvent {
            event_id: format!("{SOURCE}:taint:{boot}:{}", flag.letter),
            time_unix: now_unix,
            of_previous_boot: false,
            component: flag.component.into(),
            subject: if flag.component == pb::HardwareComponent::Memory {
                "memory".to_owned()
            } else {
                "CPU".to_owned()
            },
            severity: flag.severity.into(),
            state: pb::HardwareEventState::Event.into(),
            source: SOURCE.to_owned(),
            code: flag.code.to_owned(),
            message: flag.message.to_owned(),
            raw: format!("/proc/sys/kernel/tainted: {tainted}"),
            about: String::new(),
        })
        .collect()
}

/// The `HardwareCorrupted` of `/proc/meminfo`, in kB; none when the line is absent (a kernel
/// built without memory failure handling) or does not parse.
#[must_use]
pub fn hardware_corrupted_kib(meminfo: &str) -> Option<u64> {
    meminfo
        .lines()
        .find_map(|line| line.strip_prefix("HardwareCorrupted:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|kib| kib.parse().ok())
}

/// The record of `kib` kB of `HardwareCorrupted`, read at `now_unix` in the boot `boot`.
#[must_use]
pub fn corrupted_event(kib: u64, boot: &str, now_unix: u64) -> pb::HardwareEvent {
    pb::HardwareEvent {
        event_id: format!("{SOURCE}:hwcorrupted:{boot}:{kib}"),
        time_unix: now_unix,
        of_previous_boot: false,
        component: pb::HardwareComponent::Memory.into(),
        subject: "memory".to_owned(),
        severity: pb::HardwareSeverity::Warning.into(),
        state: pb::HardwareEventState::Event.into(),
        source: SOURCE.to_owned(),
        code: "memory_hardware_corrupted".to_owned(),
        message: format!("{kib} kB of memory taken out of use after hardware errors"),
        raw: format!("HardwareCorrupted: {kib} kB"),
        about: String::new(),
    }
}

/// The error counter of one ext4 filesystem.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ext4Errors {
    /// Its device, as `/sys/fs/ext4` names it (`sda1`, `nvme0n1p2`, `dm-0`).
    pub device: String,
    /// `errors_count`.
    pub count: u64,
    /// `last_error_time`, Unix seconds; 0 when not read.
    pub last_error_unix: u64,
}

/// The record of `errors` when its count is above `before`, the count of the last read, read at
/// `now_unix`; none when it is not. Its time is the filesystem's last error, else `now_unix`.
#[must_use]
pub fn ext4_event(errors: &Ext4Errors, before: u64, now_unix: u64) -> Option<pb::HardwareEvent> {
    if errors.count <= before {
        return None;
    }
    let device = &errors.device;
    Some(pb::HardwareEvent {
        event_id: format!("{SOURCE}:ext4:{device}:{}", errors.count),
        time_unix: if errors.last_error_unix == 0 {
            now_unix
        } else {
            errors.last_error_unix
        },
        of_previous_boot: false,
        component: pb::HardwareComponent::Disk.into(),
        subject: device.clone(),
        severity: pb::HardwareSeverity::Warning.into(),
        state: pb::HardwareEventState::Event.into(),
        source: SOURCE.to_owned(),
        code: "filesystem_errors".to_owned(),
        message: format!(
            "ext4 on {device}: {} errors recorded, {} new",
            errors.count,
            errors.count - before
        ),
        raw: format!("/sys/fs/ext4/{device}/errors_count: {}", errors.count),
        about: String::new(),
    })
}

/// The device names of `/sys/fs/ext4` that are not read: loop, RAM and network block devices.
pub const EXT4_NOT_READ: [&str; 4] = ["loop", "ram", "zram", "nbd"];

/// Whether `device`, a name of `/sys/fs/ext4`, is read: a disk's partition, a RAID array or a
/// device mapper volume, none of [`EXT4_NOT_READ`].
#[must_use]
pub fn ext4_device_read(device: &str) -> bool {
    !device.is_empty()
        && !EXT4_NOT_READ
            .iter()
            .any(|prefix| device.starts_with(prefix))
}

/// One mount of a block device whose read-only remount is judged.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Mount {
    /// The device, as the mount table names it (`/dev/sda1`).
    pub device: String,
    /// Where it is mounted, as the mount table spells it.
    pub point: String,
    pub read_only: bool,
    /// Its line of the mount table.
    pub line: String,
}

/// The mounts of `/proc/self/mounts` (`<device> <point> <type> <options> <dump> <pass>`) of a
/// device under `/dev/` with a type of [`REMOUNT_KINDS`]; read-only when the first option is
/// `ro`. A line that does not parse is passed over.
#[must_use]
pub fn parse_mounts(text: &str) -> Vec<Mount> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let device = fields.next()?;
            let point = fields.next()?;
            let kind = fields.next()?;
            let options = fields.next()?;
            (device.starts_with("/dev/") && REMOUNT_KINDS.contains(&kind)).then(|| Mount {
                device: device.to_owned(),
                point: point.to_owned(),
                read_only: options.split(',').next() == Some("ro"),
                line: line.to_owned(),
            })
        })
        .collect()
}

/// What the read-only remounts of a boot keep between reads.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RemountMark {
    /// The mount points seen read-write in this boot and mounted still.
    pub writable: BTreeSet<String>,
    /// The mount points asserted read-only, each with the id of its assertion and its device.
    pub read_only: BTreeMap<String, Asserted>,
}

/// One read-only remount asserted and not yet deasserted.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Asserted {
    pub event_id: String,
    pub subject: String,
}

/// The subject of a record of `device`: its name without `/dev/` (and `mapper/`).
#[must_use]
pub fn device_subject(device: &str) -> String {
    let name = device.strip_prefix("/dev/").unwrap_or(device);
    name.strip_prefix("mapper/").unwrap_or(name).to_owned()
}

/// The records of `mounts`, the mount table read at `now_unix` in the boot `boot`, against
/// `mark`, which it brings up to date: a mount read-only that `mark` saw read-write is asserted;
/// a mount asserted that is read-write again, or gone, is deasserted.
#[must_use]
pub fn remount_events(
    mounts: &[Mount],
    mark: &mut RemountMark,
    boot: &str,
    now_unix: u64,
) -> Vec<pb::HardwareEvent> {
    let mut events = Vec::new();
    let current: BTreeSet<&str> = mounts.iter().map(|m| m.point.as_str()).collect();
    let (gone, kept): (BTreeMap<String, Asserted>, BTreeMap<String, Asserted>) =
        std::mem::take(&mut mark.read_only)
            .into_iter()
            .partition(|(point, _)| !current.contains(point.as_str()));
    mark.read_only = kept;
    for (point, asserted) in gone {
        events.push(ended(
            &asserted,
            now_unix,
            &format!("{point} is no longer mounted"),
            "",
        ));
    }
    mark.writable
        .retain(|point| current.contains(point.as_str()));
    for mount in mounts {
        let point = &mount.point;
        if !mount.read_only {
            if let Some(asserted) = mark.read_only.remove(point) {
                events.push(ended(
                    &asserted,
                    now_unix,
                    &format!("{point} ({}) is writable again", asserted.subject),
                    &mount.line,
                ));
            }
            mark.writable.insert(point.clone());
        } else if mark.writable.contains(point) && !mark.read_only.contains_key(point) {
            let subject = device_subject(&mount.device);
            let event_id = format!("{SOURCE}:ro:{boot}:{}:{now_unix}", digest8(point));
            events.push(pb::HardwareEvent {
                event_id: event_id.clone(),
                time_unix: now_unix,
                of_previous_boot: false,
                component: pb::HardwareComponent::Disk.into(),
                subject: subject.clone(),
                severity: pb::HardwareSeverity::Critical.into(),
                state: pb::HardwareEventState::Asserted.into(),
                source: SOURCE.to_owned(),
                code: "filesystem_read_only".to_owned(),
                message: format!("{point} ({subject}) was remounted read-only"),
                raw: mount.line.clone(),
                about: String::new(),
            });
            mark.read_only
                .insert(point.clone(), Asserted { event_id, subject });
        }
    }
    events
}

/// The deassertions of every remount `mark` holds asserted, at the end of their boot, read at
/// `now_unix`; `mark` is emptied.
#[must_use]
pub fn remounts_ended(mark: &mut RemountMark, now_unix: u64) -> Vec<pb::HardwareEvent> {
    let held = std::mem::take(mark);
    held.read_only
        .iter()
        .map(|(point, asserted)| {
            ended(
                asserted,
                now_unix,
                &format!("{point} ({}): its boot ended", asserted.subject),
                "",
            )
        })
        .collect()
}

fn ended(asserted: &Asserted, now_unix: u64, message: &str, raw: &str) -> pb::HardwareEvent {
    pb::HardwareEvent {
        event_id: format!("{}:end", asserted.event_id),
        time_unix: now_unix,
        of_previous_boot: false,
        component: pb::HardwareComponent::Disk.into(),
        subject: asserted.subject.clone(),
        severity: pb::HardwareSeverity::Info.into(),
        state: pb::HardwareEventState::Deasserted.into(),
        source: SOURCE.to_owned(),
        code: "filesystem_read_only".to_owned(),
        message: message.to_owned(),
        raw: raw.to_owned(),
        about: asserted.event_id.clone(),
    }
}

/// One dump of the kernel's log that pstore kept: its files' text, read as one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dump {
    /// Its name: the first of its files (`dmesg-efi-171234567801001`), or the directory
    /// `systemd-pstore` archived it in.
    pub name: String,
    /// When it was written, Unix seconds; 0 when not known.
    pub mtime: u64,
    /// Its text.
    pub text: String,
}

/// The id of the record of `dump`.
#[must_use]
pub fn dump_id(dump: &Dump) -> String {
    format!("{SOURCE}:pstore:{}:{}", dump.mtime, digest8(&dump.name))
}

/// The record of `dump`, in a boot that began at `boot_unix` (0: not known, and every dump is of
/// a boot before).
#[must_use]
pub fn pstore_event(dump: &Dump, boot_unix: u64) -> pb::HardwareEvent {
    use pb::HardwareSeverity::{Critical, Info, Warning};
    let text = &dump.text;
    let line_of = |word: &str| text.lines().find(|line| line.contains(word));
    let panic_line = line_of("Kernel panic - not syncing");
    let panic = panic_line.is_some() || text.contains("Panic#");
    let oops = text.contains("Oops#") || text.contains("Oops:");
    let hardware = HARDWARE_WORDS.iter().copied().find_map(line_of);
    let name = &dump.name;
    let (severity, code, message) = match (hardware.is_some(), panic, oops) {
        (true, true, _) | (true, _, true) => (
            Critical,
            "panic_hardware",
            format!("Kernel crash, suspected hardware fault (pstore {name})"),
        ),
        (_, true, _) => (
            Warning,
            "kernel_panic",
            format!("Kernel panic (pstore {name})"),
        ),
        (_, _, true) => (
            Warning,
            "kernel_oops",
            format!("Kernel oops (pstore {name})"),
        ),
        _ => (
            Info,
            "kernel_dump",
            format!("Kernel log dump kept by pstore ({name})"),
        ),
    };
    let raw = panic_line
        .or(hardware)
        .or_else(|| line_of("Oops:"))
        .or_else(|| text.lines().find(|line| !line.trim().is_empty()))
        .unwrap_or_default();
    pb::HardwareEvent {
        event_id: dump_id(dump),
        time_unix: dump.mtime,
        of_previous_boot: boot_unix == 0 || dump.mtime < boot_unix,
        component: pb::HardwareComponent::Board.into(),
        subject: "board".to_owned(),
        severity: severity.into(),
        state: pb::HardwareEventState::Event.into(),
        source: SOURCE.to_owned(),
        code: code.to_owned(),
        message,
        raw: raw.trim().to_owned(),
        about: String::new(),
    }
}

#[cfg(test)]
#[path = "kernel_files_tests.rs"]
mod tests;
