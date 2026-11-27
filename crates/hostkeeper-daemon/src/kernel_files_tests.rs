//! Tests of `kernel_files`: the taint flags, `HardwareCorrupted`, the ext4 error counters, the
//! read-only remount sequences over the mount table and the pstore dumps, all as whole records.

use super::*;

const PSTORE_PANIC_MCE: &str = include_str!("../tests/fixtures/kernel/pstore-panic-mce.txt");
const PSTORE_OOPS: &str = include_str!("../tests/fixtures/kernel/pstore-oops.txt");

fn taint_record(letter: char, bit_code: &str, tainted: u64) -> pb::HardwareEvent {
    let flag = TAINT_FLAGS
        .iter()
        .find(|flag| flag.letter == letter)
        .expect("a known flag");
    pb::HardwareEvent {
        event_id: format!("sysfs:taint:boot1:{letter}"),
        time_unix: 500,
        of_previous_boot: false,
        component: flag.component.into(),
        subject: if letter == 'B' { "memory" } else { "CPU" }.to_owned(),
        severity: flag.severity.into(),
        state: pb::HardwareEventState::Event.into(),
        source: "sysfs".to_owned(),
        code: bit_code.to_owned(),
        message: flag.message.to_owned(),
        raw: format!("/proc/sys/kernel/tainted: {tainted}"),
        about: String::new(),
    }
}

/// A trailing newline is ignored and a number is read.
#[test]
fn parse_tainted_reads_a_number_with_its_newline() {
    assert_eq!(parse_tainted("4128\n"), Some(4128));
    assert_eq!(parse_tainted("0"), Some(0));
}

/// Junk, an empty text and a negative number give none.
#[test]
fn parse_tainted_gives_none_for_junk() {
    assert_eq!(parse_tainted("tainted\n"), None);
    assert_eq!(parse_tainted(""), None);
    assert_eq!(parse_tainted("-1"), None);
}

/// Only the bits of M, B, D and L are kept.
#[test]
fn taint_bits_keeps_only_the_hardware_flags() {
    assert_eq!(taint_bits(0), 0);
    assert_eq!(taint_bits(1 | (1 << 6) | (1 << 12)), 0);
    assert_eq!(
        taint_bits(u64::MAX),
        (1 << 4) | (1 << 5) | (1 << 7) | (1 << 14)
    );
    assert_eq!(taint_bits((1 << 4) | (1 << 6)), 1 << 4);
}

/// Only the flags set and not seen give records, in M, B, D, L order, whole.
#[test]
fn taint_events_give_the_new_hardware_flags_as_whole_records() {
    let tainted = 1 | (1 << 4) | (1 << 5) | (1 << 7) | (1 << 14);
    let events = taint_events(tainted, 1 << 5, "boot1", 500);
    assert_eq!(
        events,
        vec![
            taint_record('M', "taint_machine_check", tainted),
            taint_record('D', "taint_die", tainted),
            taint_record('L', "taint_soft_lockup", tainted),
        ]
    );
    assert_eq!(events[0].severity, pb::HardwareSeverity::Critical as i32);
    assert_eq!(events[1].component, pb::HardwareComponent::Cpu as i32);
}

/// A bad page is a memory record with the memory subject.
#[test]
fn taint_events_name_memory_for_a_bad_page() {
    let tainted = 1 << 5;
    assert_eq!(
        taint_events(tainted, 0, "boot1", 500),
        vec![taint_record('B', "taint_bad_page", tainted)]
    );
    let event = &taint_events(tainted, 0, "boot1", 500)[0];
    assert_eq!(event.component, pb::HardwareComponent::Memory as i32);
    assert_eq!(event.severity, pb::HardwareSeverity::Warning as i32);
}

/// Flags that are not hardware, or all seen, give nothing.
#[test]
fn taint_events_give_nothing_for_other_flags_or_seen_ones() {
    assert!(taint_events(1 | (1 << 6) | (1 << 12), 0, "boot1", 500).is_empty());
    let all = (1 << 4) | (1 << 5) | (1 << 7) | (1 << 14);
    assert!(taint_events(all, all, "boot1", 500).is_empty());
    assert!(taint_events(0, 0, "boot1", 500).is_empty());
}

/// The `HardwareCorrupted` line is found among the others.
#[test]
fn hardware_corrupted_is_read_from_meminfo() {
    let meminfo =
        "MemTotal:       16318412 kB\nHardwareCorrupted:      12 kB\nAnonHugePages: 0 kB\n";
    assert_eq!(hardware_corrupted_kib(meminfo), Some(12));
    assert_eq!(hardware_corrupted_kib("HardwareCorrupted: 0 kB\n"), Some(0));
}

/// An absent line gives none.
#[test]
fn hardware_corrupted_is_none_when_the_line_is_absent() {
    assert_eq!(
        hardware_corrupted_kib("MemTotal: 1 kB\nMemFree: 1 kB\n"),
        None
    );
    assert_eq!(hardware_corrupted_kib(""), None);
}

/// A line with a value that is not a number, or none, gives none.
#[test]
fn hardware_corrupted_is_none_when_the_line_is_malformed() {
    assert_eq!(hardware_corrupted_kib("HardwareCorrupted: lots kB\n"), None);
    assert_eq!(hardware_corrupted_kib("HardwareCorrupted:\n"), None);
}

/// The record of a size of corrupted memory, whole.
#[test]
fn corrupted_event_is_a_whole_memory_record() {
    assert_eq!(
        corrupted_event(64, "boot1", 700),
        pb::HardwareEvent {
            event_id: "sysfs:hwcorrupted:boot1:64".to_owned(),
            time_unix: 700,
            of_previous_boot: false,
            component: pb::HardwareComponent::Memory.into(),
            subject: "memory".to_owned(),
            severity: pb::HardwareSeverity::Warning.into(),
            state: pb::HardwareEventState::Event.into(),
            source: "sysfs".to_owned(),
            code: "memory_hardware_corrupted".to_owned(),
            message: "64 kB of memory taken out of use after hardware errors".to_owned(),
            raw: "HardwareCorrupted: 64 kB".to_owned(),
            about: String::new(),
        }
    );
}

fn sda1(count: u64, last_error_unix: u64) -> Ext4Errors {
    Ext4Errors {
        device: "sda1".to_owned(),
        count,
        last_error_unix,
    }
}

/// A count that did not grow gives none.
#[test]
fn ext4_event_is_none_at_an_equal_or_lower_count() {
    assert_eq!(ext4_event(&sda1(3, 100), 3, 900), None);
    assert_eq!(ext4_event(&sda1(2, 100), 3, 900), None);
    assert_eq!(ext4_event(&sda1(0, 0), 0, 900), None);
}

/// A grown count gives a whole record timed at the filesystem's last error.
#[test]
fn ext4_event_is_a_whole_record_at_the_last_error_time() {
    assert_eq!(
        ext4_event(&sda1(3, 1_700_000_000), 1, 1_800_000_000),
        Some(pb::HardwareEvent {
            event_id: "sysfs:ext4:sda1:3".to_owned(),
            time_unix: 1_700_000_000,
            of_previous_boot: false,
            component: pb::HardwareComponent::Disk.into(),
            subject: "sda1".to_owned(),
            severity: pb::HardwareSeverity::Warning.into(),
            state: pb::HardwareEventState::Event.into(),
            source: "sysfs".to_owned(),
            code: "filesystem_errors".to_owned(),
            message: "ext4 on sda1: 3 errors recorded, 2 new".to_owned(),
            raw: "/sys/fs/ext4/sda1/errors_count: 3".to_owned(),
            about: String::new(),
        })
    );
}

/// A last error time of 0 is replaced by now.
#[test]
fn ext4_event_uses_now_when_the_last_error_time_is_zero() {
    let event = ext4_event(&sda1(1, 0), 0, 1_800_000_000).expect("a record");
    assert_eq!(event.time_unix, 1_800_000_000);
    assert_eq!(event.message, "ext4 on sda1: 1 errors recorded, 1 new");
}

/// Loop, RAM, compressed RAM and network block devices are refused.
#[test]
fn ext4_device_read_refuses_loop_ram_and_network_devices() {
    for device in ["loop0", "ram0", "zram0", "nbd0", ""] {
        assert!(!ext4_device_read(device), "{device:?}");
    }
}

/// Partitions, whole NVMe namespaces, device mapper and RAID devices are read.
#[test]
fn ext4_device_read_accepts_disks_mappers_and_arrays() {
    for device in ["sda1", "nvme0n1p2", "dm-0", "md0"] {
        assert!(ext4_device_read(device), "{device:?}");
    }
}

fn mount(device: &str, point: &str, kind: &str, options: &str) -> Mount {
    Mount {
        device: device.to_owned(),
        point: point.to_owned(),
        read_only: options.split(',').next() == Some("ro"),
        line: format!("{device} {point} {kind} {options} 0 0"),
    }
}

/// Block-device mounts of the judged types are kept, whole, and the rest dropped.
#[test]
fn parse_mounts_keeps_block_device_filesystems_and_drops_the_rest() {
    let table = "\
/dev/mapper/vg-root / ext4 rw,relatime 0 0
tmpfs /run tmpfs rw,nosuid 0 0
overlay /var/lib/x overlay rw,lowerdir=/a 0 0
proc /proc proc rw 0 0
nas:/export /mnt/nas nfs4 rw 0 0
/dev/sdb1 /boot/efi vfat rw 0 0
/dev/sdc1 /mnt/c btrfs ro,noatime 0 0
/dev/sdd1 /mnt/d xfs rw,ro 0 0
";
    assert_eq!(
        parse_mounts(table),
        vec![
            mount("/dev/mapper/vg-root", "/", "ext4", "rw,relatime"),
            mount("/dev/sdc1", "/mnt/c", "btrfs", "ro,noatime"),
            mount("/dev/sdd1", "/mnt/d", "xfs", "rw,ro"),
        ]
    );
}

/// The read-only flag is the first option, so `rw,ro` is writable and `ro,...` is read-only.
#[test]
fn parse_mounts_reads_read_only_from_the_first_option() {
    let mounts = parse_mounts("/dev/sda1 /a ext4 ro 0 0\n/dev/sda2 /b ext4 rw,ro 0 0\n");
    assert!(mounts[0].read_only);
    assert!(!mounts[1].read_only);
}

/// Lines that are blank or too short are passed over.
#[test]
fn parse_mounts_skips_malformed_lines() {
    let table = "\n/dev/sda1\n/dev/sda1 /a\n/dev/sda1 /a ext4\n/dev/sda1 /b ext4 rw 0 0\n";
    assert_eq!(
        parse_mounts(table),
        vec![mount("/dev/sda1", "/b", "ext4", "rw")]
    );
    assert!(parse_mounts("").is_empty());
}

/// The subject drops `/dev/` and `mapper/`, and a bare name stays.
#[test]
fn device_subject_drops_the_dev_and_mapper_prefixes() {
    assert_eq!(device_subject("/dev/sda1"), "sda1");
    assert_eq!(device_subject("/dev/mapper/vg-root"), "vg-root");
    assert_eq!(device_subject("sdb2"), "sdb2");
}

const RW: &str = "/dev/sda1 /data ext4 rw,relatime 0 0\n";
const RO: &str = "/dev/sda1 /data ext4 ro,relatime 0 0\n";

fn asserted_record(now: u64) -> pb::HardwareEvent {
    pb::HardwareEvent {
        event_id: format!("sysfs:ro:boot1:{}:{now}", digest8("/data")),
        time_unix: now,
        of_previous_boot: false,
        component: pb::HardwareComponent::Disk.into(),
        subject: "sda1".to_owned(),
        severity: pb::HardwareSeverity::Critical.into(),
        state: pb::HardwareEventState::Asserted.into(),
        source: "sysfs".to_owned(),
        code: "filesystem_read_only".to_owned(),
        message: "/data (sda1) was remounted read-only".to_owned(),
        raw: RO.trim_end().to_owned(),
        about: String::new(),
    }
}

fn deasserted_record(now: u64, assertion_now: u64, message: &str, raw: &str) -> pb::HardwareEvent {
    let about = asserted_record(assertion_now).event_id;
    pb::HardwareEvent {
        event_id: format!("{about}:end"),
        time_unix: now,
        of_previous_boot: false,
        component: pb::HardwareComponent::Disk.into(),
        subject: "sda1".to_owned(),
        severity: pb::HardwareSeverity::Info.into(),
        state: pb::HardwareEventState::Deasserted.into(),
        source: "sysfs".to_owned(),
        code: "filesystem_read_only".to_owned(),
        message: message.to_owned(),
        raw: raw.to_owned(),
        about,
    }
}

/// Read-write, then read-only, asserts once; read-only again gives nothing; read-write again
/// deasserts, each as a whole record.
#[test]
fn remount_events_assert_once_then_deassert_when_writable_again() {
    let mut mark = RemountMark::default();
    assert!(remount_events(&parse_mounts(RW), &mut mark, "boot1", 10).is_empty());
    assert_eq!(
        remount_events(&parse_mounts(RO), &mut mark, "boot1", 20),
        vec![asserted_record(20)]
    );
    assert!(remount_events(&parse_mounts(RO), &mut mark, "boot1", 30).is_empty());
    assert_eq!(
        remount_events(&parse_mounts(RW), &mut mark, "boot1", 40),
        vec![deasserted_record(
            40,
            20,
            "/data (sda1) is writable again",
            RW.trim_end()
        )]
    );
    assert!(mark.read_only.is_empty());
    assert!(mark.writable.contains("/data"));
    assert!(remount_events(&parse_mounts(RW), &mut mark, "boot1", 50).is_empty());
}

/// A mount that is read-only from the first read is never asserted.
#[test]
fn remount_events_never_assert_a_mount_read_only_from_the_start() {
    let mut mark = RemountMark::default();
    assert!(remount_events(&parse_mounts(RO), &mut mark, "boot1", 10).is_empty());
    assert!(remount_events(&parse_mounts(RO), &mut mark, "boot1", 20).is_empty());
    assert_eq!(mark, RemountMark::default());
}

/// An asserted mount that is unmounted is deasserted with an empty raw.
#[test]
fn remount_events_deassert_an_unmounted_mount() {
    let mut mark = RemountMark::default();
    let _ = remount_events(&parse_mounts(RW), &mut mark, "boot1", 10);
    let _ = remount_events(&parse_mounts(RO), &mut mark, "boot1", 20);
    assert_eq!(
        remount_events(&[], &mut mark, "boot1", 30),
        vec![deasserted_record(30, 20, "/data is no longer mounted", "")]
    );
    assert_eq!(mark, RemountMark::default());
}

/// A point unmounted is forgotten as writable, so a fresh read-only mount of it is not asserted.
#[test]
fn remount_events_forget_unmounted_points_as_writable() {
    let mut mark = RemountMark::default();
    let _ = remount_events(&parse_mounts(RW), &mut mark, "boot1", 10);
    assert!(remount_events(&[], &mut mark, "boot1", 20).is_empty());
    assert!(mark.writable.is_empty());
    assert!(remount_events(&parse_mounts(RO), &mut mark, "boot1", 30).is_empty());
    assert!(mark.read_only.is_empty());
}

/// The end of a boot deasserts every asserted mount, in point order, and empties the mark.
#[test]
fn remounts_ended_deassert_every_asserted_mount_and_empty_the_mark() {
    let rw = "/dev/sda1 /data ext4 rw 0 0\n/dev/mapper/vg-var /var ext4 rw 0 0\n";
    let ro = "/dev/sda1 /data ext4 ro 0 0\n/dev/mapper/vg-var /var ext4 ro 0 0\n";
    let mut mark = RemountMark::default();
    let _ = remount_events(&parse_mounts(rw), &mut mark, "boot1", 10);
    let asserted = remount_events(&parse_mounts(ro), &mut mark, "boot1", 20);
    assert_eq!(asserted.len(), 2);
    let ended = remounts_ended(&mut mark, 99);
    assert_eq!(
        ended,
        vec![
            pb::HardwareEvent {
                event_id: format!("{}:end", asserted[0].event_id),
                time_unix: 99,
                of_previous_boot: false,
                component: pb::HardwareComponent::Disk.into(),
                subject: "sda1".to_owned(),
                severity: pb::HardwareSeverity::Info.into(),
                state: pb::HardwareEventState::Deasserted.into(),
                source: "sysfs".to_owned(),
                code: "filesystem_read_only".to_owned(),
                message: "/data (sda1): its boot ended".to_owned(),
                raw: String::new(),
                about: asserted[0].event_id.clone(),
            },
            pb::HardwareEvent {
                event_id: format!("{}:end", asserted[1].event_id),
                time_unix: 99,
                of_previous_boot: false,
                component: pb::HardwareComponent::Disk.into(),
                subject: "vg-var".to_owned(),
                severity: pb::HardwareSeverity::Info.into(),
                state: pb::HardwareEventState::Deasserted.into(),
                source: "sysfs".to_owned(),
                code: "filesystem_read_only".to_owned(),
                message: "/var (vg-var): its boot ended".to_owned(),
                raw: String::new(),
                about: asserted[1].event_id.clone(),
            },
        ]
    );
    assert_eq!(mark, RemountMark::default());
    assert!(remounts_ended(&mut mark, 100).is_empty());
}

fn dump(name: &str, mtime: u64, text: &str) -> Dump {
    Dump {
        name: name.to_owned(),
        mtime,
        text: text.to_owned(),
    }
}

fn pstore_record(
    dump: &Dump,
    previous: bool,
    severity: pb::HardwareSeverity,
    code: &str,
    message: &str,
    raw: &str,
) -> pb::HardwareEvent {
    pb::HardwareEvent {
        event_id: format!("sysfs:pstore:{}:{}", dump.mtime, digest8(&dump.name)),
        time_unix: dump.mtime,
        of_previous_boot: previous,
        component: pb::HardwareComponent::Board.into(),
        subject: "board".to_owned(),
        severity: severity.into(),
        state: pb::HardwareEventState::Event.into(),
        source: "sysfs".to_owned(),
        code: code.to_owned(),
        message: message.to_owned(),
        raw: raw.to_owned(),
        about: String::new(),
    }
}

/// A panic after a machine check is a critical hardware crash whose raw is the panic line.
#[test]
fn pstore_event_calls_a_panic_after_a_machine_check_a_hardware_crash() {
    let d = dump("dmesg-efi-1", 1000, PSTORE_PANIC_MCE);
    assert_eq!(
        pstore_event(&d, 2000),
        pstore_record(
            &d,
            true,
            pb::HardwareSeverity::Critical,
            "panic_hardware",
            "Kernel crash, suspected hardware fault (pstore dmesg-efi-1)",
            "<0>[86399.200000] Kernel panic - not syncing: Fatal machine check",
        )
    );
}

/// An oops without hardware words is a kernel oops warning whose raw is the `Oops:` line.
#[test]
fn pstore_event_calls_an_oops_without_hardware_words_a_kernel_oops() {
    let d = dump("dmesg-efi-2", 1000, PSTORE_OOPS);
    assert_eq!(
        pstore_event(&d, 2000),
        pstore_record(
            &d,
            true,
            pb::HardwareSeverity::Warning,
            "kernel_oops",
            "Kernel oops (pstore dmesg-efi-2)",
            "<4>[  512.000100] Oops: 0000 [#1] PREEMPT SMP NOPTI",
        )
    );
}

/// A panic with no hardware words is a kernel panic warning.
#[test]
fn pstore_event_calls_a_panic_without_hardware_words_a_kernel_panic() {
    let text = "Panic#1 Part1\n<0>[1.0] Kernel panic - not syncing: VFS: Unable to mount root fs\n";
    let d = dump("dmesg-efi-3", 1000, text);
    assert_eq!(
        pstore_event(&d, 2000),
        pstore_record(
            &d,
            true,
            pb::HardwareSeverity::Warning,
            "kernel_panic",
            "Kernel panic (pstore dmesg-efi-3)",
            "<0>[1.0] Kernel panic - not syncing: VFS: Unable to mount root fs",
        )
    );
}

/// A dump marked as a panic only by its header is still a panic, with the first line as raw.
#[test]
fn pstore_event_takes_a_panic_from_the_header_alone() {
    let d = dump("dmesg-efi-4", 1000, "Panic#2 Part1\n<6>[1.0] quiet\n");
    let event = pstore_event(&d, 2000);
    assert_eq!(event.code, "kernel_panic");
    assert_eq!(event.raw, "Panic#2 Part1");
}

/// An oops that names an uncorrected machine check is a critical hardware crash whose raw is
/// that line.
#[test]
fn pstore_event_calls_an_oops_naming_a_machine_check_a_hardware_crash() {
    let text =
        "Oops#1 Part1\n<4>[2.0] Machine Check Exception: 5 Bank 4\n<4>[2.1] Oops: 0000 [#1]\n";
    let d = dump("dmesg-efi-5", 1000, text);
    assert_eq!(
        pstore_event(&d, 2000),
        pstore_record(
            &d,
            true,
            pb::HardwareSeverity::Critical,
            "panic_hardware",
            "Kernel crash, suspected hardware fault (pstore dmesg-efi-5)",
            "<4>[2.0] Machine Check Exception: 5 Bank 4",
        )
    );
}

/// An oops whose log holds only a corrected error's lines is a plain oops, never a hardware
/// crash: a corrected error does not make a crash a hardware fault.
#[test]
fn pstore_event_does_not_call_an_oops_after_a_corrected_error_a_hardware_crash() {
    let text = "Oops#1 Part1\n<4>[1.0] mce: [Hardware Error]: Machine check events logged\n\
                <4>[1.1] {1}[Hardware Error]: event severity: corrected\n<4>[2.1] Oops: 0000 [#1]\n";
    let event = pstore_event(&dump("dmesg-efi-9", 1000, text), 2000);
    assert_eq!(event.code, "kernel_oops");
    assert_eq!(event.severity, pb::HardwareSeverity::Warning as i32);
    assert_eq!(event.raw, "<4>[2.1] Oops: 0000 [#1]");
}

/// A dump that is neither a panic nor an oops is an info dump whose raw is its first non-empty line.
#[test]
fn pstore_event_calls_any_other_dump_a_kernel_dump() {
    let d = dump("dmesg-efi-6", 1000, "\n  \n  first line here  \nsecond\n");
    assert_eq!(
        pstore_event(&d, 2000),
        pstore_record(
            &d,
            true,
            pb::HardwareSeverity::Info,
            "kernel_dump",
            "Kernel log dump kept by pstore (dmesg-efi-6)",
            "first line here",
        )
    );
}

/// A hardware word without a panic or an oops stays an info dump, raw its first line.
#[test]
fn pstore_event_does_not_call_hardware_words_alone_a_crash() {
    let d = dump(
        "dmesg-efi-7",
        1000,
        "<4>[1.0] [Hardware Error]: corrected\n",
    );
    let event = pstore_event(&d, 2000);
    assert_eq!(event.code, "kernel_dump");
    assert_eq!(event.severity, pb::HardwareSeverity::Info as i32);
    assert_eq!(event.raw, "<4>[1.0] [Hardware Error]: corrected");
}

/// An empty dump has an empty raw.
#[test]
fn pstore_event_gives_an_empty_raw_for_an_empty_dump() {
    let event = pstore_event(&dump("dmesg-efi-8", 1000, ""), 2000);
    assert_eq!(event.code, "kernel_dump");
    assert_eq!(event.raw, "");
}

/// A dump written before the boot began is of the previous boot; one written at or after is not.
#[test]
fn pstore_event_is_of_the_previous_boot_when_written_before_the_boot_began() {
    let before = dump("a", 1999, PSTORE_OOPS);
    let at = dump("b", 2000, PSTORE_OOPS);
    let after = dump("c", 2001, PSTORE_OOPS);
    assert!(pstore_event(&before, 2000).of_previous_boot);
    assert!(!pstore_event(&at, 2000).of_previous_boot);
    assert!(!pstore_event(&after, 2000).of_previous_boot);
}

/// With an unknown boot start every dump is of a previous boot.
#[test]
fn pstore_event_is_of_the_previous_boot_when_the_boot_start_is_unknown() {
    assert!(pstore_event(&dump("a", 5000, PSTORE_OOPS), 0).of_previous_boot);
}

/// The id is `sysfs:pstore:<mtime>:<digest8 of the name>`.
#[test]
fn dump_id_joins_the_mtime_and_the_digest_of_the_name() {
    let d = dump("dmesg-efi-171234567801001", 171_234_567, "x");
    assert_eq!(
        dump_id(&d),
        format!(
            "sysfs:pstore:171234567:{}",
            digest8("dmesg-efi-171234567801001")
        )
    );
    assert_eq!(digest8("dmesg-efi-171234567801001").len(), 8);
}
