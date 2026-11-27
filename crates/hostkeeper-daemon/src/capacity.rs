//! A Linux host's capacity as the heartbeat's host health carries it (`HostHealth.baseline`,
//! docs/design/host-health-alerts.md sections 3.1 and 4.3): `MemTotal`, the memory devices of the
//! SMBIOS table, the CPUs present and online and the kernel's SMT control, the physical disks and
//! the network interfaces that have a device. The daemon only reports; the coordinator keeps the
//! baseline and judges it.
//!
//! Everything is read from files under the host's root (`/` on a host, a scratch directory in the
//! tests), with no program run and nothing written:
//!
//! - **Memory**: `MemTotal` of [`MEMINFO`], in KiB; the SMBIOS memory devices (type 17) of
//!   [`SMBIOS_ENTRIES`], the raw entries the kernel exports, decoded by [`parse_memory_device`]
//!   (the table `dmidecode --type 17` decodes, without `dmidecode`), populated or not, in the
//!   order of their instance. Whether the table was read is the source state [`SMBIOS_SOURCE`]:
//!   `unsupported` with no entries directory (a virtual machine without SMBIOS, a host whose
//!   kernel exports none), `unreadable` when an entry cannot be read or decoded or there are more
//!   populated devices than the bound, with the first reason; the devices that decoded are sent
//!   either way.
//! - **CPUs**: the lists of [`CPU_PRESENT`] and [`CPU_ONLINE`] counted, and [`SMT_CONTROL`] as it
//!   reads, so that turning SMT off is not read as lost cores.
//! - **Disks** of [`BLOCK_DIR`], by name, physical only: no `loop`, `ram`, `zram`, `dm-`, `md`,
//!   `nbd` or `sr` device, none without a `device` (a virtual one), no removable one and none
//!   attached through USB. Each with its size and an 8-byte digest of its serial
//!   ([`serial_digest`]), so the disk is known again and its serial is not sent.
//! - **Network interfaces** of [`NET_DIR`], by name, those with a `device` only (no loopback,
//!   bridge, veth, tunnel or bond) and no SR-IOV virtual function (a `device/physfn`); each with
//!   its speed while it has carrier, 0 otherwise.
//!
//! Each list keeps its bound of `hostkeeper_proto::health` (32 memory devices, the populated ones
//! first when there are more; 16 disks and 8 interfaces, by name) and each text its length. A
//! file that cannot be read gives 0 or nothing for its value, never an error of the heartbeat.
//!
//! **A kdump boot** (a [`PROC_VMCORE`], or `elfcorehdr=` on the kernel's command line) reports no
//! capacity at all: the capture kernel runs in its small reserved area, and its memory, CPUs and
//! devices are not the host's.

use std::fs;
use std::path::Path;

use hostkeeper_proto::health::{
    MAX_DISK_NAME, MAX_DISKS, MAX_LOCATOR, MAX_MEMORY_DEVICES, MAX_NIC_NAME, MAX_NICS, MAX_SMT,
    MAX_SOURCE_REASON,
};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use sha2::{Digest, Sha256};

use crate::health_state::cut;

/// The kernel's memory counters, under the host's root.
pub const MEMINFO: &str = "proc/meminfo";

/// The SMBIOS table's entries the kernel exports, one directory `<type>-<instance>` each holding
/// its `raw` bytes, under the host's root.
pub const SMBIOS_ENTRIES: &str = "sys/firmware/dmi/entries";

/// The CPUs the kernel counts as present, a CPU list, under the host's root.
pub const CPU_PRESENT: &str = "sys/devices/system/cpu/present";

/// The CPUs online, a CPU list, under the host's root.
pub const CPU_ONLINE: &str = "sys/devices/system/cpu/online";

/// The kernel's SMT control, under the host's root.
pub const SMT_CONTROL: &str = "sys/devices/system/cpu/smt/control";

/// The block devices, under the host's root.
pub const BLOCK_DIR: &str = "sys/block";

/// The network interfaces, under the host's root.
pub const NET_DIR: &str = "sys/class/net";

/// The previous kernel's memory, present only in a kdump capture kernel, under the host's root.
pub const PROC_VMCORE: &str = "proc/vmcore";

/// The kernel's command line, under the host's root.
pub const PROC_CMDLINE: &str = "proc/cmdline";

/// The source state's name of the SMBIOS memory devices.
pub const SMBIOS_SOURCE: &str = "smbios";

/// The SMBIOS structure type of a memory device.
pub const MEMORY_DEVICE_TYPE: u8 = 17;

/// The block devices that are never a physical disk, by the start of their name.
pub const NOT_DISKS: [&str; 7] = ["loop", "ram", "zram", "dm-", "md", "nbd", "sr"];

/// The host's capacity as one heartbeat reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct Capacity {
    /// What the heartbeat's `HostHealth.baseline` carries.
    pub baseline: pb::Baseline,
    /// Whether the SMBIOS memory devices were read ([`SMBIOS_SOURCE`]).
    pub smbios: pb::SourceState,
}

/// Why one SMBIOS memory device entry was not decoded.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SmbiosError {
    /// The entry is not a memory device.
    #[error("entry of type {found}, not a memory device")]
    NotMemoryDevice {
        /// The type the entry gives.
        found: u8,
    },
    /// The entry is shorter than its header says, or than the fields it is read for.
    #[error("entry of {len} bytes is cut short")]
    Short {
        /// Its length in bytes.
        len: usize,
    },
    /// The entry's strings are not ended by two zero bytes.
    #[error("entry's strings are not terminated")]
    Unterminated,
    /// The device says its size is unknown.
    #[error("{locator} reports an unknown size")]
    UnknownSize {
        /// Its slot.
        locator: String,
    },
}

/// The capacity of the host whose root is `root`, read now (the module's doc); none in a kdump
/// boot.
#[must_use]
pub fn read(root: &Path) -> Option<Capacity> {
    if kdump_boot(root) {
        return None;
    }
    let (memory_devices, smbios) = memory_devices(root);
    let baseline = pb::Baseline {
        mem_total_kib: fs::read_to_string(root.join(MEMINFO)).map_or(0, |t| parse_mem_total(&t)),
        memory_devices,
        cpus_present: cpu_count(root, CPU_PRESENT),
        cpus_online: cpu_count(root, CPU_ONLINE),
        smt: fs::read_to_string(root.join(SMT_CONTROL))
            .map(|text| cut(text.trim(), MAX_SMT))
            .unwrap_or_default(),
        disks: disks(root),
        nics: nics(root),
    };
    Some(Capacity { baseline, smbios })
}

/// Whether the host whose root is `root` runs a kdump capture kernel.
fn kdump_boot(root: &Path) -> bool {
    root.join(PROC_VMCORE).exists()
        || fs::read_to_string(root.join(PROC_CMDLINE)).is_ok_and(|line| {
            line.split_ascii_whitespace()
                .any(|word| word.starts_with("elfcorehdr="))
        })
}

/// `MemTotal` of a `/proc/meminfo`, in KiB; 0 when it has none.
#[must_use]
pub fn parse_mem_total(meminfo: &str) -> u64 {
    meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))
        .and_then(|rest| rest.split_ascii_whitespace().next())
        .and_then(|kib| kib.parse().ok())
        .unwrap_or(0)
}

/// How many CPUs a kernel CPU list (`0-27`, `0-3,8-11`, `0`) names; 0 for an empty or malformed
/// list.
#[must_use]
pub fn parse_cpu_list(list: &str) -> u32 {
    let mut count: u32 = 0;
    for part in list.trim().split(',') {
        let (first, last) = part.split_once('-').unwrap_or((part, part));
        match (first.parse::<u32>(), last.parse::<u32>()) {
            (Ok(first), Ok(last)) if first <= last => {
                count = count.saturating_add(last - first + 1);
            }
            _ => return 0,
        }
    }
    count
}

fn cpu_count(root: &Path, list: &str) -> u32 {
    fs::read_to_string(root.join(list)).map_or(0, |text| parse_cpu_list(&text))
}

/// The memory device of one raw SMBIOS type 17 entry: its slot (the device locator, or the bank
/// locator when it gives none, or the entry's handle) and its size in bytes, 0 for an empty slot.
///
/// # Errors
/// [`SmbiosError`]: the entry is not of type 17, is cut short, its strings are not terminated,
/// or its size is unknown.
pub fn parse_memory_device(raw: &[u8]) -> Result<pb::MemoryDevice, SmbiosError> {
    let short = || SmbiosError::Short { len: raw.len() };
    let (&kind, &len) = (
        raw.first().ok_or_else(short)?,
        raw.get(1).ok_or_else(short)?,
    );
    if kind != MEMORY_DEVICE_TYPE {
        return Err(SmbiosError::NotMemoryDevice { found: kind });
    }
    let len = usize::from(len);
    // The device locator's string index is the last field read in every version (offset 0x10);
    // the extended size (0x1C, four bytes) only when the size says so.
    if len < 0x11 || raw.len() < len {
        return Err(short());
    }
    let strings = strings(&raw[len..])?;
    let string = |offset: usize| {
        let index = usize::from(raw[offset]);
        index
            .checked_sub(1)
            .and_then(|i| strings.get(i))
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
    };
    let locator = cut(
        &string(0x10)
            .or_else(|| (len > 0x11).then(|| string(0x11)).flatten())
            .map_or_else(
                || format!("handle 0x{:04X}", u16::from_le_bytes([raw[2], raw[3]])),
                str::to_owned,
            ),
        MAX_LOCATOR,
    );
    let size = u16::from_le_bytes([raw[0x0C], raw[0x0D]]);
    let size_bytes = match size {
        0 => 0,
        0xFFFF => return Err(SmbiosError::UnknownSize { locator }),
        0x7FFF => {
            let extended = raw
                .get(0x1C..0x20)
                .filter(|_| len >= 0x20)
                .ok_or_else(short)?;
            let mib = u32::from_le_bytes([extended[0], extended[1], extended[2], extended[3]])
                & 0x7FFF_FFFF;
            u64::from(mib) << 20
        }
        _ if size & 0x8000 != 0 => u64::from(size & 0x7FFF) << 10,
        _ => u64::from(size) << 20,
    };
    Ok(pb::MemoryDevice {
        locator,
        size_bytes,
    })
}

/// The strings that follow an SMBIOS entry's formatted area: each ended by a zero byte, the set by
/// a second one (two zero bytes for an entry with none).
fn strings(area: &[u8]) -> Result<Vec<String>, SmbiosError> {
    let end = area
        .windows(2)
        .position(|pair| pair == [0, 0])
        .ok_or(SmbiosError::Unterminated)?;
    if end == 0 {
        return Ok(Vec::new());
    }
    Ok(area[..end]
        .split(|&b| b == 0)
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect())
}

/// The memory devices of the SMBIOS table under `root`, and the source state of reading them.
fn memory_devices(root: &Path) -> (Vec<pb::MemoryDevice>, pb::SourceState) {
    let dir = root.join(SMBIOS_ENTRIES);
    let Ok(entries) = fs::read_dir(&dir) else {
        return (
            Vec::new(),
            state(
                pb::SourceStatus::Unsupported,
                &format!("/{SMBIOS_ENTRIES} is absent"),
            ),
        );
    };
    let mut instances: Vec<(u32, String)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let instance = name.strip_prefix("17-")?.parse().ok()?;
            Some((instance, name))
        })
        .collect();
    instances.sort_unstable();
    let mut devices = Vec::new();
    let mut failure: Option<String> = None;
    for (_, name) in instances {
        let decoded = fs::read(dir.join(&name).join("raw"))
            .map_err(|e| format!("{name}: {e}"))
            .and_then(|raw| parse_memory_device(&raw).map_err(|e| format!("{name}: {e}")));
        match decoded {
            Ok(device) => devices.push(device),
            Err(why) => failure = failure.or(Some(why)),
        }
    }
    if devices.len() > MAX_MEMORY_DEVICES {
        // The populated ones first, each kept in its order, so that the modules installed are
        // all sent when the empty slots are past the bound.
        devices.sort_by_key(|device| device.size_bytes == 0);
        let populated = devices.iter().filter(|d| d.size_bytes != 0).count();
        if populated > MAX_MEMORY_DEVICES {
            failure = failure.or_else(|| {
                Some(format!(
                    "{populated} memory devices, more than {MAX_MEMORY_DEVICES}"
                ))
            });
        }
        devices.truncate(MAX_MEMORY_DEVICES);
    }
    let smbios = match failure {
        None => state(pb::SourceStatus::Ok, ""),
        Some(why) => state(pb::SourceStatus::Unreadable, &why),
    };
    (devices, smbios)
}

fn state(status: pb::SourceStatus, reason: &str) -> pb::SourceState {
    pb::SourceState {
        source: SMBIOS_SOURCE.to_owned(),
        status: status.into(),
        reason: cut(reason, MAX_SOURCE_REASON),
    }
}

/// The names of the entries of `dir`, in order; none when it cannot be read.
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter_map(|entry| entry.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    names.sort_unstable();
    names
}

/// The first line of the file at `path`, trimmed; none when it cannot be read.
fn line(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|t| t.trim().to_owned())
}

/// Whether the block device `name` of `dir`, under the host's root `root`, is a physical disk
/// (the module's doc): attached through USB when the device's path under the root, its links
/// followed, passes through a `usb` bus or port.
fn is_disk(root: &Path, dir: &Path, name: &str) -> bool {
    let at = dir.join(name);
    let through_usb = || {
        fs::canonicalize(&at)
            .ok()
            .zip(fs::canonicalize(root).ok())
            .is_some_and(|(path, root)| {
                path.strip_prefix(&root).is_ok_and(|path| {
                    path.components()
                        .any(|c| c.as_os_str().to_str().is_some_and(|c| c.starts_with("usb")))
                })
            })
    };
    !NOT_DISKS.iter().any(|prefix| name.starts_with(prefix))
        && at.join("device").exists()
        && line(&at.join("removable")).as_deref() != Some("1")
        && !through_usb()
}

fn disks(root: &Path) -> Vec<pb::BaselineDisk> {
    let dir = root.join(BLOCK_DIR);
    names(&dir)
        .into_iter()
        .filter(|name| is_disk(root, &dir, name))
        .take(MAX_DISKS)
        .map(|name| {
            let at = dir.join(&name);
            pb::BaselineDisk {
                size_bytes: line(&at.join("size"))
                    .and_then(|sectors| sectors.parse::<u64>().ok())
                    .map_or(0, |sectors| sectors.saturating_mul(512)),
                serial_digest: disk_serial(&at).map_or(0, |serial| serial_digest(&serial)),
                name: cut(&name, MAX_DISK_NAME),
            }
        })
        .collect()
}

/// The serial of the disk whose block directory is `at`: its device's `serial` (NVMe, and SCSI
/// where the driver gives one), its own `serial` (virtio), or the unit serial number page of its
/// SCSI device (`vpd_pg80`, the serial after the page's four-byte header); none when it reports
/// none.
fn disk_serial(at: &Path) -> Option<String> {
    let vpd = || {
        let page = fs::read(at.join("device/vpd_pg80")).ok()?;
        let (header, body) = page.split_at_checked(4)?;
        let len = usize::from(u16::from_be_bytes([header[2], header[3]]));
        Some(String::from_utf8_lossy(body.get(..len).unwrap_or(body)).into_owned())
    };
    [at.join("device/serial"), at.join("serial")]
        .iter()
        .find_map(|path| fs::read_to_string(path).ok())
        .or_else(vpd)
        .map(|serial| {
            serial
                .trim_matches(|c: char| c.is_whitespace() || c == '\0')
                .to_owned()
        })
        .filter(|serial| !serial.is_empty())
}

/// The first 8 bytes of the SHA-256 of `serial`, as a big-endian integer.
#[must_use]
pub fn serial_digest(serial: &str) -> u64 {
    let digest = Sha256::digest(serial.as_bytes());
    let mut first = [0u8; 8];
    first.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(first)
}

fn nics(root: &Path) -> Vec<pb::BaselineNic> {
    let dir = root.join(NET_DIR);
    names(&dir)
        .into_iter()
        .filter(|name| {
            let device = dir.join(name).join("device");
            device.exists() && !device.join("physfn").exists()
        })
        .take(MAX_NICS)
        .map(|name| {
            let at = dir.join(&name);
            let carrier = line(&at.join("carrier")).as_deref() == Some("1");
            pb::BaselineNic {
                speed_mbps: if carrier {
                    line(&at.join("speed"))
                        .and_then(|speed| speed.parse::<u32>().ok())
                        .unwrap_or(0)
                } else {
                    0
                },
                name: cut(&name, MAX_NIC_NAME),
            }
        })
        .collect()
}
