//! A Linux host's capacity (`hostkeeper_daemon::capacity`) and its place in each heartbeat's host
//! health (`HealthKeeper::reading_capacity`): read from a scratch root holding a workstation's
//! recorded files (`tests/fixtures/capacity/workstation.tree`) and the SMBIOS memory devices of
//! the incident's host, encoded as its firmware's table gives them; the filters of the disks and
//! network interfaces; the decoder of SMBIOS type 17 entries; the memory devices sent in a boot's
//! heartbeats; the health's bound. No real `/proc`, `/sys` or program.

mod support;

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::sync::Arc;

use hostkeeper_daemon::capacity::{
    self, BLOCK_DIR, CPU_ONLINE, CPU_PRESENT, Capacity, MEMINFO, NET_DIR, PROC_CMDLINE,
    PROC_VMCORE, SMBIOS_ENTRIES, SMBIOS_SOURCE, SMT_CONTROL, SmbiosError, parse_cpu_list,
    parse_mem_total, parse_memory_device, serial_digest,
};
use hostkeeper_daemon::host_health::{BootFacts, HealthKeeper};
use hostkeeper_daemon::shutdown_probe::{GoingDown, ShutdownProbe};
use hostkeeper_link::HostHealthSource;
use hostkeeper_link::testing::RecordingLog;
use hostkeeper_proto::health::{MAX_EVENTS, MAX_HEALTH_BYTES, check_health};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use prost::Message;
use support::Scratch;

/// The workstation's capacity files, as recorded.
const WORKSTATION: &str = include_str!("fixtures/capacity/workstation.tree");

/// The incident host's `MemTotal` before the fault and after it (docs/design/host-health-alerts.md
/// section 1), KiB.
const INCIDENT_BEFORE_KIB: u64 = 268_320_876;
const INCIDENT_AFTER_KIB: u64 = 251_543_660;

const GIB: u64 = 1 << 30;

/// Builds the tree `text` describes (the fixture's format) under `root`.
fn plant(root: &Path, text: &str) {
    for line in text
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let mut parts = line.splitn(3, ' ');
        let (kind, path) = (parts.next().unwrap(), parts.next().unwrap());
        let rest = parts.next().unwrap_or("");
        let at = root.join(path);
        fs::create_dir_all(at.parent().unwrap()).unwrap();
        match kind {
            "f" => fs::write(at, rest.replace("\\n", "\n")).unwrap(),
            "l" => symlink(rest, at).unwrap(),
            "d" => fs::create_dir_all(at).unwrap(),
            other => panic!("unknown entry {other} in the tree"),
        }
    }
}

fn file(root: &Path, path: &str, text: &[u8]) {
    let at = root.join(path);
    fs::create_dir_all(at.parent().unwrap()).unwrap();
    fs::write(at, text).unwrap();
}

fn workstation(name: &str) -> Scratch {
    let dir = Scratch::new(name);
    plant(dir.path(), WORKSTATION);
    dir
}

/// One SMBIOS 2.8 memory device (type 17, 0x28 bytes) with the size word `size`, the extended
/// size `extended` (MiB), the device locator `locator` and the bank locator `bank`, its strings
/// as a firmware writes them.
fn type17(handle: u16, size: u16, extended: u32, locator: &str, bank: &str) -> Vec<u8> {
    let mut raw = vec![0u8; 0x28];
    raw[0] = 17;
    raw[1] = 0x28;
    raw[2..4].copy_from_slice(&handle.to_le_bytes());
    raw[0x04..0x06].copy_from_slice(&0x1000u16.to_le_bytes());
    raw[0x06..0x08].copy_from_slice(&0xFFFEu16.to_le_bytes());
    raw[0x08..0x0A].copy_from_slice(&72u16.to_le_bytes());
    raw[0x0A..0x0C].copy_from_slice(&64u16.to_le_bytes());
    raw[0x0C..0x0E].copy_from_slice(&size.to_le_bytes());
    raw[0x0E] = 0x09;
    raw[0x10] = 1;
    raw[0x11] = 2;
    raw[0x12] = 0x1A;
    raw[0x17] = 3;
    raw[0x18] = 4;
    raw[0x1C..0x20].copy_from_slice(&extended.to_le_bytes());
    for s in [locator, bank, "Vendor", "00000000"] {
        raw.extend_from_slice(s.as_bytes());
        raw.push(0);
    }
    raw.push(0);
    raw
}

/// The incident host's sixteen slots, `DIMM A1` to `DIMM D4`, each of 16 GiB but those `empty`
/// names, whose slot reads empty as its firmware lists a module it mapped out.
fn incident_dimms(root: &Path, empty: &[&str]) {
    let mut instance = 0;
    for bank in ['A', 'B', 'C', 'D'] {
        for slot in 1..=4 {
            let locator = format!("DIMM {bank}{slot}");
            let size = if empty.contains(&locator.as_str()) {
                0
            } else {
                0x4000
            };
            let raw = type17(
                0x1100 + instance,
                size,
                0,
                &locator,
                &format!("P0_Node0_Channel{bank}"),
            );
            file(root, &format!("{SMBIOS_ENTRIES}/17-{instance}/raw"), &raw);
            instance += 1;
        }
    }
    // Entries of other types sit beside them.
    file(
        root,
        &format!("{SMBIOS_ENTRIES}/16-0/raw"),
        &[16, 4, 0, 0, 0, 0],
    );
}

fn dimm(locator: &str, size_bytes: u64) -> pb::MemoryDevice {
    pb::MemoryDevice {
        locator: locator.to_owned(),
        size_bytes,
    }
}

fn smbios(status: pb::SourceStatus, reason: &str) -> pb::SourceState {
    pb::SourceState {
        source: SMBIOS_SOURCE.to_owned(),
        status: status.into(),
        reason: reason.to_owned(),
    }
}

fn read(root: &Path) -> Capacity {
    capacity::read(root).expect("not a kdump boot")
}

fn disk(name: &str, size_bytes: u64, serial_digest: u64) -> pb::BaselineDisk {
    pb::BaselineDisk {
        name: name.to_owned(),
        size_bytes,
        serial_digest,
    }
}

fn nic(name: &str, speed_mbps: u32) -> pb::BaselineNic {
    pb::BaselineNic {
        name: name.to_owned(),
        speed_mbps,
    }
}

/// The workstation's two NVMe disks, as its files give them.
fn workstation_disks() -> Vec<pb::BaselineDisk> {
    vec![
        disk("nvme0n1", 12_502_446_768 * 512, 6_819_836_585_078_478_694),
        disk("nvme1n1", 3_907_029_168 * 512, 11_619_775_814_950_299_074),
    ]
}

/// Proves the recorded workstation reports its `MemTotal`, its 28 CPUs and SMT, its two NVMe
/// disks with their sizes and serial digests, and its wired and wireless NICs (the wireless one
/// with no carrier at speed 0); its loop devices, container bridge, veth and loopback are not
/// reported, and with no SMBIOS entries the source is `unsupported`.
///
/// Catches: a loop device or a virtual interface counted as hardware (churn read as a lost disk
/// or NIC), a disk's size read in KiB rather than sectors, or a serial sent in the clear.
#[test]
fn a_recorded_workstation_reports_its_memory_cpus_physical_disks_and_nics() {
    let dir = workstation("capacity-workstation");
    assert_eq!(
        read(dir.path()),
        Capacity {
            baseline: pb::Baseline {
                mem_total_kib: 129_217_144,
                memory_devices: Vec::new(),
                cpus_present: 28,
                cpus_online: 28,
                smt: "on".to_owned(),
                disks: workstation_disks(),
                nics: vec![nic("eno1", 1000), nic("wlp0s20f3", 0)],
            },
            smbios: smbios(
                pb::SourceStatus::Unsupported,
                "/sys/firmware/dmi/entries is absent"
            ),
        }
    );
}

/// Proves the incident host reports its sixteen 16 GiB DIMMs and `MemTotal` before the fault,
/// and after it `DIMM D1` with size 0 and `MemTotal` 16 GiB less, every other slot unchanged.
///
/// Catches: an empty slot left out of the list (so the coordinator cannot name it), a size
/// word read as KiB, or the slots out of their table order.
#[test]
fn the_incident_host_reports_every_dimm_then_d1_empty_and_16_gib_less() {
    let before = Scratch::new("capacity-incident-before");
    file(
        before.path(),
        MEMINFO,
        format!("MemTotal:       {INCIDENT_BEFORE_KIB} kB\n").as_bytes(),
    );
    incident_dimms(before.path(), &[]);
    let after = Scratch::new("capacity-incident-after");
    file(
        after.path(),
        MEMINFO,
        format!("MemTotal:       {INCIDENT_AFTER_KIB} kB\n").as_bytes(),
    );
    incident_dimms(after.path(), &["DIMM D1"]);

    let slots: Vec<String> = ['A', 'B', 'C', 'D']
        .iter()
        .flat_map(|bank| (1..=4).map(move |slot| format!("DIMM {bank}{slot}")))
        .collect();
    let first = read(before.path());
    assert_eq!(first.baseline.mem_total_kib, INCIDENT_BEFORE_KIB);
    assert_eq!(
        first.baseline.memory_devices,
        slots.iter().map(|s| dimm(s, 16 * GIB)).collect::<Vec<_>>()
    );
    assert_eq!(first.smbios, smbios(pb::SourceStatus::Ok, ""));

    let second = read(after.path());
    assert_eq!(second.baseline.mem_total_kib, INCIDENT_AFTER_KIB);
    assert_eq!(
        second.baseline.memory_devices,
        slots
            .iter()
            .map(|s| dimm(s, if s == "DIMM D1" { 0 } else { 16 * GIB }))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        (INCIDENT_BEFORE_KIB - INCIDENT_AFTER_KIB) * 1024,
        16 * GIB,
        "the fixture's drop is one module"
    );
}

/// Proves a disk removed between two boots is absent from the second boot's report, the other
/// kept with the same serial digest, so the coordinator can tell which disk went.
///
/// Catches: a disk list keyed by position, so the remaining disk reads as the removed one.
#[test]
fn a_removed_disk_is_absent_from_the_next_boot() {
    let dir = workstation("capacity-disk-removed");
    assert_eq!(read(dir.path()).baseline.disks, workstation_disks());
    fs::remove_file(dir.path().join(BLOCK_DIR).join("nvme0n1")).unwrap();
    assert_eq!(
        read(dir.path()).baseline.disks,
        vec![workstation_disks().remove(1)]
    );
}

/// Proves no block device of a loop, ram, zram, device-mapper, md, nbd or optical name is a disk
/// even with a device behind it, nor one without a device, nor a removable one, nor one attached
/// through USB, while a SATA disk and an SD card on the board are.
///
/// Catches: USB sticks, LVM volumes or RAID arrays churning the disk baseline.
#[test]
fn virtual_removable_and_usb_block_devices_are_not_disks() {
    let dir = Scratch::new("capacity-disk-filters");
    let root = dir.path();
    let board = "sys/devices/pci0000:00/0000:00:17.0";
    let usb = "sys/devices/pci0000:00/0000:00:14.0/usb2/2-1/2-1:1.0/host6/target6:0:0/6:0:0:0";
    fs::create_dir_all(root.join(BLOCK_DIR)).unwrap();
    for (name, parent) in [
        ("loop7", board),
        ("ram0", board),
        ("zram0", board),
        ("dm-0", board),
        ("md127", board),
        ("nbd0", board),
        ("sr0", board),
        ("sda", board),
        ("sdb", board),
        ("sdc", usb),
        ("mmcblk0", board),
    ] {
        let real = root.join(parent).join("block").join(name);
        fs::create_dir_all(real.join("device")).unwrap();
        fs::write(real.join("size"), b"2048\n").unwrap();
        symlink(&real, root.join(BLOCK_DIR).join(name)).unwrap();
    }
    // sda says it is fixed, sdb removable; sdc sits behind a USB port; vda has no device.
    file(root, &format!("{board}/block/sda/removable"), b"0\n");
    file(root, &format!("{board}/block/sdb/removable"), b"1\n");
    file(root, &format!("{BLOCK_DIR}/vda/size"), b"2048\n");

    assert_eq!(
        read(root).baseline.disks,
        vec![disk("mmcblk0", 2048 * 512, 0), disk("sda", 2048 * 512, 0)]
    );
}

/// Proves a disk's serial is read from its device (NVMe, SCSI drivers that give one), from the
/// disk itself (virtio), or from the SCSI unit serial page after its header (cut to its length,
/// or the rest when the page says more), trimmed of spaces and zero bytes; a page shorter than
/// its header, a blank serial and none at all give 0 (a blank serial file in front of a page
/// falls back to the page); a size that does not parse gives 0.
///
/// Catches: the page header hashed with the serial, or a padded serial giving another digest
/// after a firmware update.
#[test]
fn disk_serials_are_read_from_the_device_the_disk_or_the_scsi_page() {
    let dir = Scratch::new("capacity-serials");
    let root = dir.path();
    let disks = [
        ("sda", "device/serial", b"  SN0000000000000001\n".to_vec()),
        ("sdb", "device/vpd_pg80", {
            let mut page = vec![0, 0x80, 0, 14];
            page.extend_from_slice(b"SCSI-PAGE-0001\0\0junk");
            page
        }),
        ("sdc", "device/vpd_pg80", b"\0\x80\0\x40SC".to_vec()),
        ("sdd", "device/vpd_pg80", b"\0\x80\0".to_vec()),
        ("sde", "device/serial", b" \0 \n".to_vec()),
        ("sdg", "device/serial", b"  \n".to_vec()),
        ("sdf", "device/model", b"no serial here\n".to_vec()),
        ("vda", "serial", b"VIRTIO-0001".to_vec()),
    ];
    for (name, path, text) in &disks {
        fs::create_dir_all(root.join(BLOCK_DIR).join(name).join("device")).unwrap();
        file(root, &format!("{BLOCK_DIR}/{name}/{path}"), text);
        file(root, &format!("{BLOCK_DIR}/{name}/size"), b"8\n");
    }
    file(root, &format!("{BLOCK_DIR}/sdf/size"), b"not a number\n");
    let mut page = vec![0, 0x80, 0, 14];
    page.extend_from_slice(b"SCSI-PAGE-0001");
    file(root, &format!("{BLOCK_DIR}/sdg/device/vpd_pg80"), &page);
    assert_eq!(
        read(root).baseline.disks,
        vec![
            disk("sda", 4096, serial_digest("SN0000000000000001")),
            disk("sdb", 4096, 9_258_804_133_276_289_950),
            disk("sdc", 4096, 3_664_354_640_403_570_131),
            disk("sdd", 4096, 0),
            disk("sde", 4096, 0),
            disk("sdf", 0, 0),
            disk("sdg", 4096, 9_258_804_133_276_289_950),
            disk("vda", 4096, 7_398_366_513_843_897_074),
        ]
    );
    assert_eq!(
        serial_digest("SN0000000000000001"),
        6_819_836_585_078_478_694
    );
}

/// Proves only interfaces with a device are reported, an SR-IOV virtual function is not, and an
/// interface's speed is its own only while it has carrier: no carrier, a carrier that cannot be
/// read and a speed of -1 all give 0.
///
/// Catches: a virtual function or a bond churning the NIC baseline, or an unplugged port's last
/// speed read as its link.
#[test]
fn nics_need_a_device_and_carrier_for_their_speed() {
    let dir = Scratch::new("capacity-nics");
    let root = dir.path();
    let nics: [(&str, Option<&str>, Option<&str>); 5] = [
        ("enp1s0f0", Some("1"), Some("25000")),
        ("enp1s0f1", Some("0"), Some("25000")),
        ("enp1s0f2", Some("1"), Some("-1")),
        ("enp1s0f3", None, Some("10000")),
        ("enp1s0v0", Some("1"), Some("25000")),
    ];
    for (name, carrier, speed) in nics {
        fs::create_dir_all(root.join(NET_DIR).join(name).join("device")).unwrap();
        if let Some(carrier) = carrier {
            file(
                root,
                &format!("{NET_DIR}/{name}/carrier"),
                carrier.as_bytes(),
            );
        }
        if let Some(speed) = speed {
            file(root, &format!("{NET_DIR}/{name}/speed"), speed.as_bytes());
        }
    }
    fs::create_dir_all(root.join(NET_DIR).join("enp1s0v0/device/physfn")).unwrap();
    fs::create_dir_all(root.join(NET_DIR).join("bond0")).unwrap();
    assert_eq!(
        read(root).baseline.nics,
        vec![
            nic("enp1s0f0", 25000),
            nic("enp1s0f1", 0),
            nic("enp1s0f2", 0),
            nic("enp1s0f3", 0),
        ]
    );
}

/// Proves the lists keep their bounds: 16 disks and 8 interfaces by name, past them none.
///
/// Catches: a host with many disks or ports overrunning the health's bound.
#[test]
fn disks_and_nics_are_kept_to_their_bounds() {
    let dir = Scratch::new("capacity-bounds");
    let root = dir.path();
    for n in 0..20 {
        let name = format!("nvme{n:02}n1");
        fs::create_dir_all(root.join(BLOCK_DIR).join(&name).join("device")).unwrap();
        let nic = format!("eth{n:02}");
        fs::create_dir_all(root.join(NET_DIR).join(&nic).join("device")).unwrap();
    }
    let baseline = read(root).baseline;
    assert_eq!(
        baseline
            .disks
            .iter()
            .map(|d| d.name.as_str())
            .collect::<Vec<_>>(),
        (0..16).map(|n| format!("nvme{n:02}n1")).collect::<Vec<_>>()
    );
    assert_eq!(
        baseline
            .nics
            .iter()
            .map(|d| d.name.as_str())
            .collect::<Vec<_>>(),
        (0..8).map(|n| format!("eth{n:02}")).collect::<Vec<_>>()
    );
}

/// Proves the decoder of a type 17 entry: a size in MiB, in KiB (bit 15), through the extended
/// size, 0 for an empty slot; the slot named by its device locator, else its bank locator, else
/// its handle; and each malformed entry refused by name.
///
/// Catches: a 32 GiB or larger module (the extended size) read as 32 GiB minus 1 MiB, or a
/// slot with no locator sent with an empty name.
#[test]
fn smbios_memory_devices_decode_sizes_and_locators_and_refuse_malformed_ones() {
    let ok = |raw: &[u8]| parse_memory_device(raw).map(|d| (d.locator, d.size_bytes));
    assert_eq!(
        ok(&type17(1, 0x4000, 0, "DIMM D1", "B")),
        Ok(("DIMM D1".to_owned(), 16 * GIB))
    );
    assert_eq!(
        ok(&type17(1, 0x8000 | 512, 0, "DIMM D1", "B")),
        Ok(("DIMM D1".to_owned(), 512 * 1024))
    );
    assert_eq!(
        ok(&type17(1, 0x7FFF, 0x8001_0000, "DIMM D1", "B")),
        Ok(("DIMM D1".to_owned(), 64 * GIB))
    );
    assert_eq!(
        ok(&type17(1, 0, 0, "DIMM D1", "B")),
        Ok(("DIMM D1".to_owned(), 0))
    );
    assert_eq!(
        ok(&type17(1, 0x4000, 0, "   ", "P0 CHANNEL A")),
        Ok(("P0 CHANNEL A".to_owned(), 16 * GIB))
    );
    let mut no_index = type17(0x2A, 0x4000, 0, "DIMM D1", "B");
    no_index[0x10] = 0;
    no_index[0x11] = 9;
    assert_eq!(ok(&no_index), Ok(("handle 0x002A".to_owned(), 16 * GIB)));
    // An SMBIOS 2.1 entry (0x11 bytes read, no bank locator byte) with no strings at all.
    let mut v21 = type17(0x2B, 0x4000, 0, "", "")[..0x11].to_vec();
    v21[1] = 0x11;
    v21.extend_from_slice(&[0, 0]);
    assert_eq!(ok(&v21), Ok(("handle 0x002B".to_owned(), 16 * GIB)));
    let long = "L".repeat(40);
    assert_eq!(
        ok(&type17(1, 0x4000, 0, &long, "B")),
        Ok(("L".repeat(32), 16 * GIB))
    );

    let mut not17 = type17(1, 0x4000, 0, "DIMM D1", "B");
    not17[0] = 16;
    let mut header_long = type17(1, 0x4000, 0, "DIMM D1", "B");
    header_long[1] = 0xF0;
    let mut too_short = v21;
    too_short[1] = 0x10;
    let mut v26 = type17(1, 0x7FFF, 0, "DIMM D1", "B")[..0x1C].to_vec();
    v26[1] = 0x1C;
    v26.extend_from_slice(b"DIMM D1\0\0");
    let unterminated = type17(1, 0x4000, 0, "DIMM D1", "B")[..0x2C].to_vec();
    let unknown = type17(1, 0xFFFF, 0, "DIMM D1", "B");
    let cases: [(&[u8], SmbiosError); 8] = [
        (&[0u8; 0][..], SmbiosError::Short { len: 0 }),
        (&[17u8][..], SmbiosError::Short { len: 1 }),
        (not17.as_slice(), SmbiosError::NotMemoryDevice { found: 16 }),
        (
            header_long.as_slice(),
            SmbiosError::Short {
                len: header_long.len(),
            },
        ),
        (
            too_short.as_slice(),
            SmbiosError::Short {
                len: too_short.len(),
            },
        ),
        (v26.as_slice(), SmbiosError::Short { len: v26.len() }),
        (unterminated.as_slice(), SmbiosError::Unterminated),
        (
            unknown.as_slice(),
            SmbiosError::UnknownSize {
                locator: "DIMM D1".to_owned(),
            },
        ),
    ];
    for (raw, error) in cases {
        assert_eq!(parse_memory_device(raw), Err(error));
    }
    assert_eq!(
        SmbiosError::UnknownSize {
            locator: "DIMM D1".to_owned()
        }
        .to_string(),
        "DIMM D1 reports an unknown size"
    );
}

/// Proves a table with an entry that does not decode, and one whose `raw` cannot be read, is
/// `unreadable` with the first reason while the devices that decoded are still sent; entries of
/// other types and names that are not `17-<n>` are passed over; the entries go in the order of
/// their instance, not of their names.
///
/// Catches: one bad entry hiding every module, or `17-10` sorted before `17-2`.
#[test]
fn an_smbios_table_with_a_bad_entry_is_unreadable_and_keeps_the_rest() {
    let dir = Scratch::new("capacity-smbios-bad");
    let root = dir.path();
    for instance in [0u16, 2, 10] {
        let raw = type17(instance, 0x4000, 0, &format!("DIMM {instance}"), "B");
        file(root, &format!("{SMBIOS_ENTRIES}/17-{instance}/raw"), &raw);
    }
    file(root, &format!("{SMBIOS_ENTRIES}/17-3/raw"), &[17, 0x28, 0]);
    fs::create_dir_all(root.join(SMBIOS_ENTRIES).join("17-4/raw")).unwrap();
    fs::create_dir_all(root.join(SMBIOS_ENTRIES).join("17-x")).unwrap();
    fs::create_dir_all(
        root.join(SMBIOS_ENTRIES)
            .join(OsStr::from_bytes(b"17-\xff")),
    )
    .unwrap();
    file(
        root,
        &format!("{SMBIOS_ENTRIES}/127-0/raw"),
        &[127, 4, 0, 0, 0, 0],
    );
    let capacity = read(root);
    assert_eq!(
        capacity.baseline.memory_devices,
        vec![
            dimm("DIMM 0", 16 * GIB),
            dimm("DIMM 2", 16 * GIB),
            dimm("DIMM 10", 16 * GIB),
        ]
    );
    assert_eq!(
        capacity.smbios,
        smbios(
            pb::SourceStatus::Unreadable,
            "17-3: entry of 3 bytes is cut short"
        )
    );

    fs::remove_dir_all(root.join(SMBIOS_ENTRIES).join("17-3")).unwrap();
    assert_eq!(
        read(root).smbios,
        smbios(
            pb::SourceStatus::Unreadable,
            "17-4: Is a directory (os error 21)"
        )
    );
}

/// Proves a board of more than 32 slots sends its populated slots first, each in its order, up
/// to 32 and stays `ok`; with more than 32 modules installed the source is `unreadable`, as the
/// coordinator could not count them.
///
/// Catches: the populated slots of a 48-slot board cut off behind its empty ones, so its memory
/// reads short.
#[test]
fn past_32_memory_devices_the_populated_ones_go_first() {
    let dir = Scratch::new("capacity-smbios-many");
    let root = dir.path();
    for instance in 0u16..48 {
        let size = if instance % 2 == 1 { 0x4000 } else { 0 };
        let raw = type17(instance, size, 0, &format!("S{instance}"), "B");
        file(root, &format!("{SMBIOS_ENTRIES}/17-{instance}/raw"), &raw);
    }
    let capacity = read(root);
    let expected: Vec<pb::MemoryDevice> = (0..48)
        .filter(|n| n % 2 == 1)
        .map(|n| dimm(&format!("S{n}"), 16 * GIB))
        .chain((0..8).map(|n| dimm(&format!("S{}", n * 2), 0)))
        .collect();
    assert_eq!(capacity.baseline.memory_devices, expected);
    assert_eq!(capacity.smbios, smbios(pb::SourceStatus::Ok, ""));

    for instance in 0u16..48 {
        let raw = type17(instance, 0x4000, 0, &format!("S{instance}"), "B");
        file(root, &format!("{SMBIOS_ENTRIES}/17-{instance}/raw"), &raw);
    }
    let capacity = read(root);
    assert_eq!(capacity.baseline.memory_devices.len(), 32);
    assert_eq!(
        capacity.smbios,
        smbios(
            pb::SourceStatus::Unreadable,
            "48 memory devices, more than 32"
        )
    );
}

/// Proves the CPU lists are counted by their ranges, `MemTotal` is read from its line, and a
/// list, a `meminfo` or an SMT control that is malformed or absent gives 0 or nothing.
///
/// Catches: an offline CPU range counted as present, or a malformed list read as a CPU count.
#[test]
fn cpu_lists_mem_total_and_smt_read_as_zero_when_absent_or_malformed() {
    for (list, count) in [
        ("0-27\n", 28),
        ("0-3,8-11", 8),
        ("0", 1),
        ("", 0),
        ("5-2", 0),
        ("0-3,x", 0),
    ] {
        assert_eq!(parse_cpu_list(list), count, "{list:?}");
    }
    for (meminfo, kib) in [
        ("MemFree: 1 kB\nMemTotal:  42 kB\n", 42),
        ("MemTotal: lots\n", 0),
        ("MemTotal:\n", 0),
        ("MemFree: 1 kB\n", 0),
    ] {
        assert_eq!(parse_mem_total(meminfo), kib, "{meminfo:?}");
    }

    let dir = Scratch::new("capacity-empty");
    assert_eq!(
        read(dir.path()).baseline,
        pb::Baseline::default(),
        "an empty root reads as nothing"
    );
    file(dir.path(), CPU_PRESENT, b"0-63\n");
    file(dir.path(), CPU_ONLINE, b"0-31\n");
    file(dir.path(), SMT_CONTROL, b"forceoff\n");
    let baseline = read(dir.path()).baseline;
    assert_eq!(
        (
            baseline.cpus_present,
            baseline.cpus_online,
            baseline.smt.as_str()
        ),
        (64, 32, "forceoff")
    );
}

/// Proves a kdump capture kernel (a `/proc/vmcore`, or `elfcorehdr=` on its command line)
/// reports no capacity, and a command line with neither does.
///
/// Catches: the capture kernel's small memory sent as the host's, a short boot that is not one.
#[test]
fn a_kdump_boot_reports_no_capacity() {
    let dir = workstation("capacity-kdump");
    assert!(capacity::read(dir.path()).is_some());
    file(
        dir.path(),
        PROC_CMDLINE,
        b"BOOT_IMAGE=/boot/vmlinuz irqpoll nr_cpus=1 elfcorehdr=0x3f000000 reset_devices\n",
    );
    assert_eq!(capacity::read(dir.path()), None);
    file(dir.path(), PROC_CMDLINE, b"BOOT_IMAGE=/boot/vmlinuz ro\n");
    file(dir.path(), PROC_VMCORE, b"");
    assert_eq!(capacity::read(dir.path()), None);
}

/// A probe that answers no.
struct Running;

impl ShutdownProbe for Running {
    fn going_down(&self) -> GoingDown {
        GoingDown::No("running".to_owned())
    }
}

fn keeper_of(state: &Scratch, root: Option<&Path>) -> HealthKeeper {
    HealthKeeper::open(
        state.path(),
        BootFacts {
            boot_id: Some("b".repeat(64)),
            boot_unix: 1_795_000_000,
            machine_id: "m-1".to_owned(),
        },
        Box::new(Running),
        Arc::new(RecordingLog::default()),
    )
    .reading_capacity(root)
}

/// Proves each heartbeat's health carries the host's capacity as read then, with the SMBIOS
/// source state and the memory devices in every heartbeat, D1 reported empty in the heartbeat
/// after it empties; a keeper reading no capacity sends none.
///
/// Catches: a capacity read once at the start, so a module lost later is never reported, or the
/// devices left out of later heartbeats while the coordinator may never have had them.
#[test]
fn heartbeats_carry_the_capacity_and_the_devices_read_then() {
    let state = Scratch::new("capacity-keeper-state");
    let host = workstation("capacity-keeper-host");
    incident_dimms(host.path(), &[]);
    let keeper = keeper_of(&state, Some(host.path()));
    let with_devices = |health: &pb::HostHealth| {
        let baseline = health.baseline.as_ref().expect("a baseline");
        assert_eq!(baseline.mem_total_kib, 129_217_144);
        assert_eq!(health.sources, [smbios(pb::SourceStatus::Ok, "")]);
        baseline.memory_devices.len()
    };
    for _ in 0..20 {
        assert_eq!(with_devices(&keeper.health()), 16);
    }
    file(host.path(), MEMINFO, b"MemTotal:       129000000 kB\n");
    assert_eq!(
        keeper.health().baseline.map(|b| b.mem_total_kib),
        Some(129_000_000),
        "each heartbeat reads the capacity again"
    );

    incident_dimms(host.path(), &["DIMM D1"]);
    let changed = keeper.health();
    assert_eq!(
        changed
            .baseline
            .as_ref()
            .map(|b| b.memory_devices[12].clone()),
        Some(dimm("DIMM D1", 0))
    );
    assert_eq!(keeper.health().baseline.unwrap().memory_devices.len(), 16);

    let none = keeper_of(&Scratch::new("capacity-keeper-none"), None).health();
    assert_eq!((none.baseline, none.sources), (None, Vec::new()));
}

/// Proves six records at their bounds and the capacity of a host at every list's bound keep the
/// health within 4096 bytes: the memory devices and the source states go first, then the latest
/// records, which come in the next heartbeat once the first are written; once the records are
/// all written the devices are back.
///
/// Catches: a health over its bound refused by the coordinator, the records lost when they do not
/// fit, or the devices gone for good once dropped.
#[test]
fn records_and_capacity_at_their_bounds_fit_the_health_bound() {
    let state = Scratch::new("capacity-fit-state");
    let host = Scratch::new("capacity-fit-host");
    let root = host.path();
    for instance in 0u16..32 {
        let raw = type17(
            instance,
            0x7FFF,
            0x0010_0000,
            &format!("{instance:L>32}"),
            "B",
        );
        file(root, &format!("{SMBIOS_ENTRIES}/17-{instance}/raw"), &raw);
    }
    for n in 0..16 {
        let name = format!("{n:d>32}");
        fs::create_dir_all(root.join(BLOCK_DIR).join(&name).join("device")).unwrap();
        file(root, &format!("{BLOCK_DIR}/{name}/size"), b"999999999999\n");
        file(
            root,
            &format!("{BLOCK_DIR}/{name}/serial"),
            format!("S{n}").as_bytes(),
        );
    }
    for n in 0..8 {
        let name = format!("{n:e>16}");
        fs::create_dir_all(root.join(NET_DIR).join(&name).join("device")).unwrap();
        file(root, &format!("{NET_DIR}/{name}/carrier"), b"1");
        file(root, &format!("{NET_DIR}/{name}/speed"), b"400000");
    }
    file(root, MEMINFO, b"MemTotal: 4294967296 kB\n");
    file(root, CPU_PRESENT, b"0-511\n");
    file(root, CPU_ONLINE, b"0-511\n");
    file(root, SMT_CONTROL, b"notimplemented\n");
    let keeper = keeper_of(&state, Some(root));
    let alone = keeper.health();
    assert_eq!(check_health(&alone), Ok(()));
    assert_eq!(alone.baseline.as_ref().unwrap().memory_devices.len(), 32);

    let full = |n: u64| pb::HardwareEvent {
        event_id: format!("{n:0>64}"),
        time_unix: n,
        severity: pb::HardwareSeverity::Warning.into(),
        subject: "s".repeat(32),
        source: "s".repeat(16),
        code: "c".repeat(32),
        message: "m".repeat(120),
        raw: "r".repeat(256),
        about: "a".repeat(64),
        ..pb::HardwareEvent::default()
    };
    keeper.record("sel", "", (1..=8).map(full).collect(), 1_795_662_000);
    let crowded = keeper.health();
    assert_eq!(check_health(&crowded), Ok(()));
    assert!(crowded.encoded_len() <= MAX_HEALTH_BYTES);
    let baseline = crowded.baseline.clone().unwrap();
    assert_eq!(
        (
            baseline.memory_devices.len(),
            baseline.disks.len(),
            baseline.nics.len()
        ),
        (0, 16, 8)
    );
    assert!(crowded.sources.is_empty());
    let sent: Vec<u64> = crowded.events.iter().map(|e| e.time_unix).collect();
    assert!(
        !sent.is_empty() && sent.len() < MAX_EVENTS,
        "fewer records, not none: {sent:?}"
    );
    assert_eq!(sent, (1..=sent.len() as u64).collect::<Vec<u64>>());

    let ids = |health: &pb::HostHealth| -> Vec<String> {
        health.events.iter().map(|e| e.event_id.clone()).collect()
    };
    keeper.written(&ids(&crowded));
    let next = keeper.health();
    assert_eq!(
        next.events.first().map(|e| e.time_unix),
        Some(sent.len() as u64 + 1)
    );
    assert!(next.baseline.as_ref().unwrap().memory_devices.is_empty());
    keeper.written(&ids(&next));
    let empty = keeper.health();
    assert!(empty.events.is_empty());
    assert_eq!(empty.baseline.unwrap().memory_devices.len(), 32);
    assert_eq!(empty.sources.len(), 1);
}
