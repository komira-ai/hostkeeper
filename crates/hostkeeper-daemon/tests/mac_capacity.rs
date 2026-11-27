//! A Mac's capacity read from `sysctl` and `system_profiler -json` (`hostkeeper_daemon::mac_capacity`):
//! the fixtures of `tests/fixtures/mac_health` give exactly the expected baseline, compared whole;
//! each output that is not what the reader expects gives nothing, and the other program's output
//! still gives what it can. The fixtures are written in the forms macOS prints, with generic
//! serials.

use hostkeeper_daemon::mac_capacity::{
    CAPACITY_SYSCTL_ARGS, PROFILER_ARGS, Profile, Sysctls, capacity, parse_profile, parse_sysctls,
    serial_digest,
};
use hostkeeper_proto::health::{MAX_DISK_NAME, MAX_DISKS, check_health};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;

fn fixture(name: &str) -> String {
    let path = format!(
        "{}/tests/fixtures/mac_health/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// The internal disk of the fixture.
fn disk0() -> pb::BaselineDisk {
    pb::BaselineDisk {
        name: "disk0".to_owned(),
        size_bytes: 500_277_790_720,
        serial_digest: 7_376_501_774_866_624_561,
    }
}

/// Proves the programs and their arguments are the fixed lists the design names.
#[test]
fn the_programs_are_run_with_fixed_arguments() {
    assert_eq!(
        CAPACITY_SYSCTL_ARGS,
        ["-n", "hw.memsize", "hw.physicalcpu", "hw.logicalcpu"]
    );
    assert_eq!(
        PROFILER_ARGS,
        ["-json", "SPHardwareDataType", "SPNVMeDataType"]
    );
}

/// Proves a Mac's capacity from both programs: `hw.memsize` in KiB, the physical and logical
/// CPUs, the internal NVMe disk by its BSD name with its serial's digest, the removable one left
/// out, and no memory devices, SMT or NICs; the health that carries it keeps its bounds.
#[test]
fn both_programs_give_the_whole_capacity() {
    let sysctls = parse_sysctls(&fixture("sysctl-capacity.txt"));
    assert_eq!(
        sysctls,
        Some(Sysctls {
            memsize: 34_359_738_368,
            physical_cpus: 12,
            logical_cpus: 12,
        })
    );
    let profile = parse_profile(&fixture("system-profiler.json")).expect("the fixture parses");
    assert_eq!(
        profile,
        Profile {
            memory_bytes: 34_359_738_368,
            cpus: 12,
            disks: vec![disk0()],
        }
    );
    let baseline = capacity(sysctls, Some(&profile)).expect("a capacity");
    assert_eq!(
        baseline,
        pb::Baseline {
            mem_total_kib: 33_554_432,
            memory_devices: Vec::new(),
            cpus_present: 12,
            cpus_online: 12,
            smt: String::new(),
            disks: vec![disk0()],
            nics: Vec::new(),
        }
    );
    let health = pb::HostHealth {
        baseline: Some(baseline),
        ..pb::HostHealth::default()
    };
    assert_eq!(check_health(&health), Ok(()));
}

/// Proves `sysctl` decides memory and CPUs when both programs give them, and `system_profiler`
/// stands in for both when `sysctl` gave none; with neither, or nothing either reads, there is
/// no capacity.
#[test]
fn sysctl_decides_and_system_profiler_stands_in() {
    let sysctls = Sysctls {
        memsize: 17_179_869_184,
        physical_cpus: 8,
        logical_cpus: 10,
    };
    let profile = parse_profile(&fixture("system-profiler.json")).expect("the fixture parses");
    let both = capacity(Some(sysctls), Some(&profile)).expect("a capacity");
    assert_eq!(
        (both.mem_total_kib, both.cpus_present, both.cpus_online),
        (16_777_216, 8, 10)
    );
    let profiled = capacity(None, Some(&profile)).expect("a capacity");
    assert_eq!(
        (
            profiled.mem_total_kib,
            profiled.cpus_present,
            profiled.cpus_online
        ),
        (33_554_432, 12, 12)
    );
    assert_eq!(profiled.disks, vec![disk0()]);
    let alone = capacity(Some(sysctls), None).expect("a capacity");
    assert!(alone.disks.is_empty());
    assert_eq!(alone.mem_total_kib, 16_777_216);
    assert_eq!(capacity(None, None), None);
    assert_eq!(capacity(None, Some(&Profile::default())), None);
    let zero = Sysctls {
        memsize: 0,
        physical_cpus: 0,
        logical_cpus: 0,
    };
    assert_eq!(capacity(Some(zero), None), None);
    let disk_only = Profile {
        disks: vec![disk0()],
        ..Profile::default()
    };
    assert_eq!(
        capacity(Some(zero), Some(&disk_only)).map(|b| b.disks),
        Some(vec![disk0()])
    );
    let cpus_only = Sysctls {
        physical_cpus: 4,
        ..zero
    };
    assert_eq!(
        capacity(Some(cpus_only), None).map(|b| b.cpus_present),
        Some(4)
    );
}

/// Proves `sysctl`'s output is read only as exactly three numbers, blank lines and spaces aside.
#[test]
fn sysctl_output_is_three_numbers_or_nothing() {
    assert_eq!(
        parse_sysctls(" 1024 \n\n2\n3\n"),
        Some(Sysctls {
            memsize: 1024,
            physical_cpus: 2,
            logical_cpus: 3,
        })
    );
    for bad in [
        "",
        "1024\n2\n",
        "1024\n2\n3\n4\n",
        "x\n2\n3\n",
        "1\ny\n3",
        "1\n2\nz",
        "1\n2\n-3",
    ] {
        assert_eq!(parse_sysctls(bad), None, "{bad:?}");
    }
}

/// Proves `system_profiler`'s output is read only as JSON of its shape: not JSON (cut at the
/// output cap), or a list where an object is, gives nothing; missing sections and fields give
/// nothing of theirs; a disk with no BSD name, an empty one or no size is left out.
#[test]
fn system_profiler_output_is_read_by_its_shape() {
    let whole = fixture("system-profiler.json");
    assert_eq!(parse_profile(&whole[..whole.len() / 2]), None);
    assert_eq!(parse_profile("[]"), None);
    assert_eq!(parse_profile("{}"), Some(Profile::default()));
    assert_eq!(
        parse_profile(r#"{"SPHardwareDataType":[{}],"SPNVMeDataType":[{}]}"#),
        Some(Profile::default())
    );
    let disks = parse_profile(
        r#"{"SPNVMeDataType":[{"_items":[
            {"size_in_bytes":1},
            {"bsd_name":"","size_in_bytes":1},
            {"bsd_name":"disk1"},
            {"bsd_name":"disk2","size_in_bytes":2},
            {"bsd_name":"disk3","size_in_bytes":3,"device_serial":"  ","removable_media":"no"}
        ]}]}"#,
    )
    .expect("parses");
    assert_eq!(
        disks.disks,
        vec![
            pb::BaselineDisk {
                name: "disk2".to_owned(),
                size_bytes: 2,
                serial_digest: 0,
            },
            pb::BaselineDisk {
                name: "disk3".to_owned(),
                size_bytes: 3,
                serial_digest: 0,
            },
        ]
    );
}

/// Proves the installed memory's words (`GB`, `GiB`, `MB`, `MiB`, `TB`, `TiB`, powers of two) and
/// the processors' forms (`proc` with each kind's count, a plain count, a number) are read, and
/// anything else gives none.
#[test]
fn memory_and_processor_words_are_read_and_others_refused() {
    let hardware = |memory: &str, processors: &str| {
        parse_profile(&format!(
            r#"{{"SPHardwareDataType":[{{"physical_memory":{memory},"number_processors":{processors}}}]}}"#
        ))
        .expect("parses")
    };
    let gib = 1u64 << 30;
    let cases = [
        (r#""16 GB""#, r#""proc 10:8:2""#, 16 * gib, 10),
        (r#""16 GiB""#, r#""8""#, 16 * gib, 8),
        (r#""512 MB""#, "24", 512 << 20, 24),
        (r#""512 MiB""#, r#""proc 6""#, 512 << 20, 6),
        (r#""1 TB""#, r#""proc x:1""#, gib << 10, 0),
        (r#""2 TiB""#, "-1", 2 * (gib << 10), 0),
        (r#""16""#, "4294967296", 0, 0),
        (r#""16 KB""#, "1.5", 0, 0),
        (r#""16 GB extra""#, "true", 0, 0),
        (r#""x GB""#, "null", 0, 0),
        (r#""18446744073709551615 GB""#, r#""""#, 0, 0),
        ("null", "[]", 0, 0),
    ];
    for (memory, processors, bytes, cpus) in cases {
        let profile = hardware(memory, processors);
        assert_eq!(profile.memory_bytes, bytes, "{memory}");
        assert_eq!(profile.cpus, cpus, "{processors}");
    }
}

/// Proves at most `MAX_DISKS` disks are sent, a long BSD name is cut to its bound, and a serial's
/// digest is its SHA-256's first 8 bytes, the same for the same serial whatever spaces around it.
#[test]
fn disks_are_bounded_and_serials_digested() {
    let items: Vec<String> = (0..MAX_DISKS + 3)
        .map(|n| {
            format!(
                r#"{{"bsd_name":"disk{n}{}","size_in_bytes":{n}}}"#,
                "x".repeat(40)
            )
        })
        .collect();
    let profile = parse_profile(&format!(
        r#"{{"SPNVMeDataType":[{{"_items":[{}]}}]}}"#,
        items.join(",")
    ))
    .expect("parses");
    assert_eq!(profile.disks.len(), MAX_DISKS + 3);
    assert!(profile.disks.iter().all(|d| d.name.len() == MAX_DISK_NAME));
    let baseline = capacity(None, Some(&profile)).expect("disks are a capacity");
    assert_eq!(baseline.disks.len(), MAX_DISKS);
    assert_eq!(baseline.disks[0].size_bytes, 0);
    assert_eq!(serial_digest("0000000000000001"), disk0().serial_digest);
    assert_eq!(serial_digest(" 0000000000000001\n"), disk0().serial_digest);
    assert_eq!(serial_digest(""), 0);
}
