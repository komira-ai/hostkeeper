//! Host health messages for the tests of `additions_host_health.rs`,
//! `additions_host_health_bounds.rs` and `additions_filesystem_use.rs`: the incident's
//! (docs/design/host-health-alerts.md section 2.2) and the fullest one within the bounds.

#![allow(
    dead_code,
    reason = "each test binary that includes this file uses only some of its messages"
)]

use hostkeeper_proto::health::{
    MAX_CODE, MAX_DISK_NAME, MAX_EVENT_ID, MAX_EVENTS, MAX_MESSAGE, MAX_NIC_NAME, MAX_RAW, MAX_SMT,
    MAX_SOURCE, MAX_SUBJECT,
};
use hostkeeper_proto::hostkeeper::agent::v1 as agent;

/// The incident's record of DIMM D1, read from the BMC's event log after the reboot, as
/// section 2.2 of the design gives it (less what the coordinator adds).
pub fn sel_record() -> agent::HardwareEvent {
    agent::HardwareEvent {
        event_id: "sel:01a4@1790000000".to_owned(),
        time_unix: 1_795_662_012,
        of_previous_boot: true,
        component: agent::HardwareComponent::Memory.into(),
        subject: "DIMM D1".to_owned(),
        severity: agent::HardwareSeverity::Critical.into(),
        state: agent::HardwareEventState::Event.into(),
        source: "sel".to_owned(),
        code: "memory_uncorrectable".to_owned(),
        message: "Uncorrectable ECC error on DIMM D1".to_owned(),
        raw: "01a4 | 11/26/2026 | 03:00:12 | Memory #0x53 DIMM_D1 | Uncorrectable ECC | Asserted"
            .to_owned(),
        about: String::new(),
    }
}

/// The incident host's health in the first heartbeats of the new boot: the SEL record, the
/// capacity with D1 empty, and its sources.
pub fn incident() -> agent::HostHealth {
    agent::HostHealth {
        events: vec![sel_record()],
        baseline: Some(agent::Baseline {
            mem_total_kib: 251_543_660,
            memory_devices: vec![
                agent::MemoryDevice {
                    locator: "DIMM D1".to_owned(),
                    size_bytes: 0,
                },
                agent::MemoryDevice {
                    locator: "DIMM D2".to_owned(),
                    size_bytes: 16 << 30,
                },
            ],
            cpus_present: 64,
            cpus_online: 64,
            smt: "on".to_owned(),
            disks: vec![agent::BaselineDisk {
                name: "nvme0n1".to_owned(),
                size_bytes: 1_920_383_410_176,
                serial_digest: 0x0123_4567_89ab_cdef,
            }],
            nics: vec![agent::BaselineNic {
                name: "eno1".to_owned(),
                speed_mbps: 25_000,
            }],
        }),
        boot_id: "b7c1".to_owned(),
        boot_unix: 1_795_662_060,
        clean_previous_shutdown: false,
        sources: vec![
            agent::SourceState {
                source: "sel".to_owned(),
                status: agent::SourceStatus::Ok.into(),
                reason: String::new(),
            },
            agent::SourceState {
                source: "rasdaemon".to_owned(),
                status: agent::SourceStatus::Unreadable.into(),
                reason: "database is locked".to_owned(),
            },
        ],
        filesystems: vec![agent::FilesystemUse {
            mount: "/".to_owned(),
            size_bytes: 1_888_000_000_000,
            available_bytes: 1_200_000_000_000,
        }],
    }
}

/// Record `n` with every text at its bound.
pub fn record_at_bound(n: usize) -> agent::HardwareEvent {
    let id = format!("sel:{n:04}@");
    agent::HardwareEvent {
        event_id: format!("{id}{}", "0".repeat(MAX_EVENT_ID - id.len())),
        time_unix: u64::MAX,
        of_previous_boot: true,
        component: agent::HardwareComponent::Host.into(),
        subject: "s".repeat(MAX_SUBJECT),
        severity: agent::HardwareSeverity::Critical.into(),
        state: agent::HardwareEventState::Deasserted.into(),
        source: "e".repeat(MAX_SOURCE),
        code: "c".repeat(MAX_CODE),
        message: "m".repeat(MAX_MESSAGE),
        raw: "r".repeat(MAX_RAW),
        about: "a".repeat(MAX_EVENT_ID),
    }
}

/// The fullest health the daemon sends in a beat after a boot's first ones: 6 records with every
/// text at its bound, and a baseline with no memory devices, 2 disks and 2 interfaces at their
/// bounds; no sources.
pub fn fullest() -> agent::HostHealth {
    agent::HostHealth {
        events: (0..MAX_EVENTS).map(record_at_bound).collect(),
        baseline: Some(agent::Baseline {
            mem_total_kib: u64::MAX,
            memory_devices: Vec::new(),
            cpus_present: u32::MAX,
            cpus_online: u32::MAX,
            smt: "t".repeat(MAX_SMT),
            disks: (0..2)
                .map(|_| agent::BaselineDisk {
                    name: "d".repeat(MAX_DISK_NAME),
                    size_bytes: u64::MAX,
                    serial_digest: u64::MAX,
                })
                .collect(),
            nics: (0..2)
                .map(|_| agent::BaselineNic {
                    name: "n".repeat(MAX_NIC_NAME),
                    speed_mbps: u32::MAX,
                })
                .collect(),
        }),
        boot_id: "0".repeat(hostkeeper_proto::health::MAX_BOOT_ID),
        boot_unix: u64::MAX,
        clean_previous_shutdown: true,
        sources: Vec::new(),
        filesystems: Vec::new(),
    }
}
