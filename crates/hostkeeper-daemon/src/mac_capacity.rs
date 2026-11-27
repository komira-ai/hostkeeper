//! A Mac's capacity, as the heartbeat's `HostHealth.baseline` carries it
//! (docs/design/host-health-alerts.md sections 3.2 and 4.1): the memory, the CPUs and the internal
//! disks, read from `sysctl` and from `system_profiler -json`, which the coordinator compares with
//! the host's baseline. Nothing here runs a program: [`capacity`] judges what the reader of
//! `crate::mac_health` got.
//!
//! - **Memory** (`mem_total_kib`): `hw.memsize` (bytes) in KiB; when `sysctl` gave none, the
//!   `physical_memory` of `SPHardwareDataType` (`16 GB`, read as GiB: Apple's `GB` here counts
//!   2^30 bytes).
//! - **CPUs** (`cpus_present`, `cpus_online`): `hw.physicalcpu` and `hw.logicalcpu`; when `sysctl`
//!   gave none, the total of `SPHardwareDataType`'s `number_processors` (`proc 10:8:2`, the total
//!   first, or a plain count) for both.
//! - **Disks** (`disks`, at most `MAX_DISKS`): each device of `SPNVMeDataType` that has a BSD name
//!   and a size and is not removable: its BSD name (`disk0`), its `size_in_bytes`, and the first
//!   8 bytes of the SHA-256 of its `device_serial` (0 with none), so the serial is never sent.
//!
//! A Mac has no memory devices to list (Apple silicon has no SMBIOS table), no `smt/control` and
//! no NIC the compare reads, so those stay empty: a capacity compare, not a memory error watch
//! (decision 5 of the design).
//!
//! `sysctl -n hw.memsize hw.physicalcpu hw.logicalcpu` prints one value a line, in that order;
//! its output is read only when it has exactly three numbers. `system_profiler`'s output is read
//! only when it parses as JSON; one that does not (cut at the output cap, or another program's
//! words) gives nothing, and the `sysctl`s alone still give memory and CPUs.

use hostkeeper_proto::health::{MAX_DISK_NAME, MAX_DISKS};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use crate::health_state::cut;

/// The `sysctl` arguments of the memory and the CPUs, one value a line.
pub const CAPACITY_SYSCTL_ARGS: [&str; 4] = ["-n", "hw.memsize", "hw.physicalcpu", "hw.logicalcpu"];

/// The program that prints the hardware inventory.
pub const SYSTEM_PROFILER: &str = "/usr/sbin/system_profiler";

/// Its arguments: the hardware overview and the NVMe disks, as JSON.
pub const PROFILER_ARGS: [&str; 3] = ["-json", "SPHardwareDataType", "SPNVMeDataType"];

/// Bytes in a GiB.
const GIB: u64 = 1 << 30;

/// What `sysctl` printed, read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sysctls {
    /// `hw.memsize`, bytes.
    pub memsize: u64,
    /// `hw.physicalcpu`.
    pub physical_cpus: u32,
    /// `hw.logicalcpu`.
    pub logical_cpus: u32,
}

/// The output of [`CAPACITY_SYSCTL_ARGS`]; none unless it is exactly three numbers, one a line.
#[must_use]
pub fn parse_sysctls(text: &str) -> Option<Sysctls> {
    let mut values = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let memsize = values.next()?.parse().ok()?;
    let physical_cpus = values.next()?.parse().ok()?;
    let logical_cpus = values.next()?.parse().ok()?;
    if values.next().is_some() {
        return None;
    }
    Some(Sysctls {
        memsize,
        physical_cpus,
        logical_cpus,
    })
}

/// What `system_profiler` printed, as much of it as the capacity reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Profile {
    /// `physical_memory`, bytes; 0 when not given or not read.
    pub memory_bytes: u64,
    /// The total of `number_processors`; 0 when not given or not read.
    pub cpus: u32,
    /// The disks, in the order printed, removable ones left out.
    pub disks: Vec<pb::BaselineDisk>,
}

#[derive(Deserialize)]
struct ProfilerJson {
    #[serde(default, rename = "SPHardwareDataType")]
    hardware: Vec<HardwareJson>,
    #[serde(default, rename = "SPNVMeDataType")]
    nvme: Vec<ControllerJson>,
}

#[derive(Deserialize)]
struct HardwareJson {
    #[serde(default)]
    physical_memory: Option<String>,
    #[serde(default)]
    number_processors: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct ControllerJson {
    #[serde(default, rename = "_items")]
    items: Vec<DeviceJson>,
}

#[derive(Deserialize)]
struct DeviceJson {
    #[serde(default)]
    bsd_name: Option<String>,
    #[serde(default)]
    size_in_bytes: Option<u64>,
    #[serde(default)]
    device_serial: Option<String>,
    #[serde(default)]
    removable_media: Option<String>,
}

/// The output of `system_profiler` with [`PROFILER_ARGS`]; none when it is not JSON of that
/// shape.
#[must_use]
pub fn parse_profile(text: &str) -> Option<Profile> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    if !value.is_object() {
        return None;
    }
    let json: ProfilerJson = serde_json::from_value(value).ok()?;
    let hardware = json.hardware.first();
    let memory_bytes = hardware
        .and_then(|h| h.physical_memory.as_deref())
        .and_then(parse_memory)
        .unwrap_or(0);
    let cpus = hardware
        .and_then(|h| h.number_processors.as_ref())
        .and_then(parse_processors)
        .unwrap_or(0);
    let disks = json
        .nvme
        .iter()
        .flat_map(|c| &c.items)
        .filter(|d| d.removable_media.as_deref() != Some("yes"))
        .filter_map(|d| {
            let name = d.bsd_name.as_deref().filter(|n| !n.is_empty())?;
            Some(pb::BaselineDisk {
                name: cut(name, MAX_DISK_NAME),
                size_bytes: d.size_in_bytes?,
                serial_digest: d.device_serial.as_deref().map_or(0, serial_digest),
            })
        })
        .collect();
    Some(Profile {
        memory_bytes,
        cpus,
        disks,
    })
}

/// `16 GB` (or `16 GiB`, `512 MB`) as bytes, `GB` and `MB` counted in powers of two as macOS
/// counts installed memory.
fn parse_memory(text: &str) -> Option<u64> {
    let mut words = text.split_whitespace();
    let amount: u64 = words.next()?.parse().ok()?;
    let unit = match words.next()? {
        "GB" | "GiB" => GIB,
        "MB" | "MiB" => 1 << 20,
        "TB" | "TiB" => GIB << 10,
        _ => return None,
    };
    if words.next().is_some() {
        return None;
    }
    amount.checked_mul(unit)
}

/// `number_processors`: `proc 10:8:2` (the total, then each kind's count) or a plain count, as a
/// string or a number; its total.
fn parse_processors(value: &serde_json::Value) -> Option<u32> {
    match value {
        serde_json::Value::Number(n) => n.as_u64().and_then(|n| u32::try_from(n).ok()),
        serde_json::Value::String(s) => {
            let counts = s.strip_prefix("proc ").unwrap_or(s);
            counts.split(':').next()?.trim().parse().ok()
        }
        _ => None,
    }
}

/// The first 8 bytes of the SHA-256 of `serial` (trimmed), as a big-endian integer; 0 for an
/// empty serial.
#[must_use]
pub fn serial_digest(serial: &str) -> u64 {
    let serial = serial.trim();
    if serial.is_empty() {
        return 0;
    }
    let digest = Sha256::digest(serial.as_bytes());
    let mut first = [0u8; 8];
    first.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(first)
}

/// The capacity of the Mac from what `sysctl` and `system_profiler` gave (either may be none);
/// none when neither gave anything the capacity reads.
#[must_use]
pub fn capacity(sysctls: Option<Sysctls>, profile: Option<&Profile>) -> Option<pb::Baseline> {
    let (memsize, physical, logical) = match sysctls {
        Some(s) => (s.memsize, s.physical_cpus, s.logical_cpus),
        None => {
            let p = profile?;
            (p.memory_bytes, p.cpus, p.cpus)
        }
    };
    let disks: Vec<pb::BaselineDisk> = profile
        .map(|p| p.disks.iter().take(MAX_DISKS).cloned().collect())
        .unwrap_or_default();
    if memsize == 0 && physical == 0 && disks.is_empty() {
        return None;
    }
    Some(pb::Baseline {
        mem_total_kib: memsize / 1024,
        memory_devices: Vec::new(),
        cpus_present: physical,
        cpus_online: logical,
        smt: String::new(),
        disks,
        nics: Vec::new(),
    })
}
