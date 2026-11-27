//! SMART through `smartctl` (`hostkeeper_daemon::smart`): each recorded `smartctl --json` output
//! of `tests/fixtures/smart/` (SATA, NVMe and SAS disks passing, failing, with a critical warning,
//! a low spare, grown media errors; a disk asleep, one with no SMART, one that does not open)
//! judged into whole records; every way a read fails (the program missing, a timeout, output past
//! its cap or not JSON, a signal, each exit status bit) as the source's state; the disks a scratch
//! root lists and the ones it does not; and a disk gone or replaced. `smartctl` is a fake that
//! answers the fixtures; no real disk, `/sys` or program is touched.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_daemon::hardware_sources::{HardwareSource, SourceRead, poll};
use hostkeeper_daemon::health_state::cut;
use hostkeeper_daemon::host_health::{BootFacts, HealthKeeper};
use hostkeeper_daemon::shutdown_probe::{GoingDown, ShutdownProbe};
use hostkeeper_daemon::smart::{
    CODE_FAILING, CODE_MEDIA, DiskMark, DiskRead, MAX_SMART_DISKS, SMART_EVERY_SECS, SOURCE,
    SmartMark, SmartSource, list_disks, parse_smartctl, serial_digest, smartctl_args,
};
use hostkeeper_launchd::launchctl::OUTPUT_MAX;
use hostkeeper_launchd::{FakeLaunchctl, LaunchctlError, LaunchctlOutput};
use hostkeeper_link::HostHealthSource;
use hostkeeper_link::testing::{FakeClock, RecordingLog};
use hostkeeper_proto::health::MAX_SOURCE_REASON;
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use support::Scratch;

/// The second the tests read at.
const NOW: u64 = 1_795_662_000;

/// The keys of the fixtures' disks: the first 8 bytes of the SHA-256 of their serials, in hex.
const SATA_KEY: &str = "bc2d504f35703933";
const NVME_KEY: &str = "3d4971c77e66e84b";
const SAS_KEY: &str = "c53d737596037395";

fn fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/smart")
        .join(format!("{name}.json"));
    fs::read(&path).expect("read the fixture")
}

fn answer(code: i32, stdout: Vec<u8>) -> Result<LaunchctlOutput, LaunchctlError> {
    Ok(LaunchctlOutput {
        code: Some(code),
        stdout,
        stderr: Vec::new(),
    })
}

/// A scratch host: its root, with `sys/block`, and an installed `smartctl`.
struct Host {
    dir: Scratch,
    fake: FakeLaunchctl,
}

impl Host {
    fn new(name: &str) -> Host {
        let dir = Scratch::new(name);
        fs::create_dir_all(dir.path().join("sys/block")).expect("make sys/block");
        fs::write(dir.path().join("smartctl"), b"").expect("install smartctl");
        Host {
            dir,
            fake: FakeLaunchctl::new(),
        }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn smartctl(&self) -> PathBuf {
        self.dir.path().join("smartctl")
    }

    /// A physical disk `name`: a `device` entry, not removable.
    fn disk(&self, name: &str) {
        let dir = self.root().join("sys/block").join(name);
        fs::create_dir_all(dir.join("device")).expect("make the disk");
        fs::write(dir.join("removable"), b"0\n").expect("write removable");
    }

    fn unplug(&self, name: &str) {
        fs::remove_dir_all(self.root().join("sys/block").join(name)).expect("remove the disk");
    }

    /// Every read of `disk` answers `fixture` with `code`.
    fn says(&self, disk: &str, code: i32, name: &str) {
        self.fake
            .always(&smartctl_args(disk), answer(code, fixture(name)));
    }

    fn source(&self) -> SmartSource {
        SmartSource::new(self.root(), &self.smartctl(), Box::new(self.fake.clone()))
    }
}

/// Reads `source` at `now` after `mark`, giving the read and its mark as kept.
fn read(source: &mut SmartSource, mark: Option<&str>, now: u64) -> (SourceRead, String) {
    let read = source.read(mark, now);
    let kept = read.mark.clone().expect("a read that keeps a mark");
    (read, kept)
}

/// A record of the source, as the tests compare it whole.
#[allow(
    clippy::too_many_arguments,
    reason = "a whole record is compared, field by field"
)]
fn record(
    event_id: &str,
    subject: &str,
    severity: pb::HardwareSeverity,
    state: pb::HardwareEventState,
    code: &str,
    message: &str,
    raw: &str,
    about: &str,
) -> pb::HardwareEvent {
    pb::HardwareEvent {
        event_id: event_id.to_owned(),
        time_unix: NOW,
        of_previous_boot: false,
        component: pb::HardwareComponent::Disk.into(),
        subject: subject.to_owned(),
        severity: severity.into(),
        state: state.into(),
        source: SOURCE.to_owned(),
        code: code.to_owned(),
        message: message.to_owned(),
        raw: raw.to_owned(),
        about: about.to_owned(),
    }
}

fn status(read: &SourceRead) -> (pb::SourceStatus, &str) {
    (read.status, read.reason.as_str())
}

#[test]
fn the_arguments_are_fixed_and_write_nothing() {
    assert_eq!(
        smartctl_args("sda"),
        [
            "--json=c",
            "--nocheck=standby",
            "--info",
            "--health",
            "--attributes",
            "/dev/sda"
        ]
    );
    let writes = [
        "-t",
        "--test",
        "-s",
        "--smart",
        "-o",
        "--offlineauto",
        "-S",
        "--saveauto",
    ];
    assert!(
        smartctl_args("sda")
            .iter()
            .all(|a| !writes.contains(&a.as_str()) && !a.starts_with("--set"))
    );
}

#[test]
fn the_serial_digest_is_the_first_eight_bytes_of_its_sha256() {
    assert_eq!(serial_digest("SN-DISK-0001"), 0xbc2d_504f_3570_3933);
    assert_eq!(format!("{:016x}", serial_digest("SN-NVME-0001")), NVME_KEY);
}

#[test]
fn a_passing_sata_disk_gives_no_record_and_reads_ok() {
    let host = Host::new("smart-sata-pass");
    host.disk("sda");
    host.says("sda", 0, "sata_pass");
    let (read, kept) = read(&mut host.source(), None, NOW);
    assert_eq!(status(&read), (pb::SourceStatus::Ok, ""));
    assert!(read.events.is_empty(), "{:?}", read.events);
    let mark: SmartMark = serde_json::from_str(&kept).expect("the mark parses");
    assert_eq!(
        mark.disks.get(SATA_KEY),
        Some(&DiskMark {
            name: "sda".to_owned(),
            failing: String::new(),
            media: Some(0),
        })
    );
    assert_eq!(host.fake.calls(), [smartctl_args("sda")]);
}

#[test]
fn a_failing_sata_disk_is_asserted_and_its_old_media_errors_kept_as_info() {
    let host = Host::new("smart-sata-fail");
    host.disk("sda");
    host.says("sda", 24, "sata_failing");
    let (read, _) = read(&mut host.source(), None, NOW);
    assert_eq!(status(&read), (pb::SourceStatus::Ok, ""));
    let raw = "exit_status=24 passed=false reallocated_sectors=1904 pending_sectors=16";
    assert_eq!(
        read.events,
        [
            record(
                &format!("smart:{SATA_KEY}:failing:{NOW}"),
                "sda",
                pb::HardwareSeverity::Critical,
                pb::HardwareEventState::Asserted,
                CODE_FAILING,
                "Disk sda is failing: SMART health check failed",
                raw,
                "",
            ),
            record(
                &format!("smart:{SATA_KEY}:media:1920"),
                "sda",
                pb::HardwareSeverity::Info,
                pb::HardwareEventState::Event,
                CODE_MEDIA,
                "Disk sda has 1920 media errors at its first read",
                raw,
                "",
            ),
        ]
    );
}

#[test]
fn a_failing_disk_is_asserted_once_and_deasserted_when_it_passes_again() {
    let host = Host::new("smart-fail-pass");
    host.disk("sda");
    host.says("sda", 24, "sata_failing");
    let mut source = host.source();
    let (_, first) = read(&mut source, None, NOW);
    let (again, second) = read(&mut source, Some(&first), NOW + 3600);
    assert!(again.events.is_empty(), "still failing: {:?}", again.events);
    host.says("sda", 0, "sata_pass");
    let later = NOW + 7200;
    let (passing, third) = read(&mut source, Some(&second), later);
    assert_eq!(
        passing.events,
        [pb::HardwareEvent {
            time_unix: later,
            ..record(
                &format!("smart:{SATA_KEY}:passing:{later}"),
                "sda",
                pb::HardwareSeverity::Critical,
                pb::HardwareEventState::Deasserted,
                CODE_FAILING,
                "Disk sda passes its SMART health check again",
                "exit_status=0 passed=true reallocated_sectors=0 pending_sectors=0",
                &format!("smart:{SATA_KEY}:failing:{NOW}"),
            )
        }]
    );
    let mark: SmartMark = serde_json::from_str(&third).expect("the mark parses");
    assert_eq!(mark.disks[SATA_KEY].failing, "");
}

#[test]
fn an_nvme_critical_warning_is_failing() {
    let host = Host::new("smart-nvme-warn");
    host.disk("nvme0n1");
    host.says("nvme0n1", 8, "nvme_critical_warning");
    let (read, _) = read(&mut host.source(), None, NOW);
    assert_eq!(
        read.events,
        [record(
            &format!("smart:{NVME_KEY}:failing:{NOW}"),
            "nvme0n1",
            pb::HardwareSeverity::Critical,
            pb::HardwareEventState::Asserted,
            CODE_FAILING,
            "Disk nvme0n1 is failing: NVMe critical warning 0x04",
            "exit_status=8 passed=false critical_warning=4 available_spare=100 \
             available_spare_threshold=10 media_errors=0",
            "",
        )]
    );
}

#[test]
fn an_nvme_spare_under_its_threshold_is_failing() {
    let host = Host::new("smart-nvme-spare");
    host.disk("nvme0n1");
    host.says("nvme0n1", 0, "nvme_low_spare");
    let (read, _) = read(&mut host.source(), None, NOW);
    assert_eq!(read.events.len(), 1, "{:?}", read.events);
    assert_eq!(
        read.events[0].message,
        "Disk nvme0n1 is failing: available spare 3% under its threshold 10%"
    );
    assert_eq!(read.events[0].code, CODE_FAILING);
}

#[test]
fn grown_media_errors_are_one_warning_per_growth() {
    let host = Host::new("smart-nvme-media");
    host.disk("nvme0n1");
    host.says("nvme0n1", 0, "nvme_pass");
    let mut source = host.source();
    let (first, kept) = read(&mut source, None, NOW);
    assert!(first.events.is_empty(), "{:?}", first.events);
    host.says("nvme0n1", 64, "nvme_media_grown");
    let (grown, kept) = read(&mut source, Some(&kept), NOW);
    assert_eq!(
        grown.events,
        [record(
            &format!("smart:{NVME_KEY}:media:7"),
            "nvme0n1",
            pb::HardwareSeverity::Warning,
            pb::HardwareEventState::Event,
            CODE_MEDIA,
            "Media errors on disk nvme0n1 grew from 0 to 7",
            "exit_status=64 passed=true critical_warning=0 available_spare=100 \
             available_spare_threshold=10 media_errors=7",
            "",
        )]
    );
    let (same, _) = read(&mut source, Some(&kept), NOW + 3600);
    assert!(same.events.is_empty(), "no growth: {:?}", same.events);
}

#[test]
fn a_sas_disks_grown_defects_at_its_first_read_are_info() {
    let host = Host::new("smart-sas");
    host.disk("sdc");
    host.says("sdc", 0, "sas_grown");
    let (read, _) = read(&mut host.source(), None, NOW);
    assert_eq!(
        read.events,
        [record(
            &format!("smart:{SAS_KEY}:media:3"),
            "sdc",
            pb::HardwareSeverity::Info,
            pb::HardwareEventState::Event,
            CODE_MEDIA,
            "Disk sdc has 3 media errors at its first read",
            "exit_status=0 passed=true grown_defects=3",
            "",
        )]
    );
}

#[test]
fn a_disk_asleep_is_left_so_and_keeps_its_state() {
    let host = Host::new("smart-standby");
    host.disk("sda");
    host.says("sda", 24, "sata_failing");
    let mut source = host.source();
    let (_, kept) = read(&mut source, None, NOW);
    host.says("sda", 2, "standby");
    let (asleep, after) = read(&mut source, Some(&kept), NOW + 3600);
    assert_eq!(status(&asleep), (pb::SourceStatus::Ok, ""));
    assert!(asleep.events.is_empty(), "{:?}", asleep.events);
    assert_eq!(after, kept, "an asleep disk keeps its assertion and count");
}

#[test]
fn each_exit_status_and_output_is_judged() {
    let cases: [(&str, Option<i32>, Vec<u8>, DiskRead); 9] = [
        ("standby", Some(2), fixture("standby"), DiskRead::Standby),
        (
            "no SMART, bit 2",
            Some(4),
            fixture("no_smart"),
            DiskRead::Unsupported("/dev/sdb reports no SMART".to_owned()),
        ),
        (
            "no SMART, bit 1",
            Some(2),
            br#"{"smartctl":{"messages":[{"string":"/dev/sdb: Unable to detect device type"}]}}"#
                .to_vec(),
            DiskRead::Unsupported("/dev/sdb reports no SMART".to_owned()),
        ),
        (
            "open failed",
            Some(2),
            fixture("open_failed"),
            DiskRead::Unreadable(
                "/dev/sdb did not open: Smartctl open device: /dev/sdb failed: No such device"
                    .to_owned(),
            ),
        ),
        (
            "command line refused",
            Some(1),
            br#"{"smartctl":{"messages":[{"string":"unrecognized option"}]}}"#.to_vec(),
            DiskRead::Unreadable("/dev/sdb: smartctl refused: unrecognized option".to_owned()),
        ),
        (
            "a command failed, no verdict",
            Some(4),
            br#"{"smartctl":{}}"#.to_vec(),
            DiskRead::Unreadable("/dev/sdb: a SMART command failed: no message".to_owned()),
        ),
        (
            "no verdict",
            Some(0),
            br#"{"device":{"name":"/dev/sdb"}}"#.to_vec(),
            DiskRead::Unsupported("/dev/sdb reports no SMART".to_owned()),
        ),
        (
            "not JSON",
            Some(1),
            b"smartctl 6.6: unknown option --json".to_vec(),
            DiskRead::Unreadable("/dev/sdb: output is not JSON (exit 1)".to_owned()),
        ),
        (
            "a signal",
            None,
            fixture("sata_pass"),
            DiskRead::Unreadable("/dev/sdb: smartctl ended by a signal".to_owned()),
        ),
    ];
    for (what, code, stdout, judged) in cases {
        assert_eq!(parse_smartctl("sdb", code, &stdout), judged, "{what}");
    }
}

#[test]
fn output_at_its_cap_is_unreadable() {
    let mut stdout = fixture("sata_pass");
    stdout.resize(OUTPUT_MAX, b' ');
    assert_eq!(
        parse_smartctl("sda", Some(0), &stdout),
        DiskRead::Unreadable(format!("/dev/sda: output past {OUTPUT_MAX} bytes"))
    );
    stdout.truncate(OUTPUT_MAX - 1);
    assert!(matches!(
        parse_smartctl("sda", Some(0), &stdout),
        DiskRead::Read(_)
    ));
}

#[test]
fn the_failing_bit_is_failing_whatever_the_json_says() {
    let DiskRead::Read(facts) = parse_smartctl("sda", Some(8), &fixture("sata_pass")) else {
        panic!("not read");
    };
    assert_eq!(facts.failing.as_deref(), Some("SMART health check failed"));
    assert_eq!(facts.serial, "SN-DISK-0001");
    assert_eq!(facts.media, Some(0));
}

#[test]
fn a_timeout_or_a_program_that_cannot_start_is_unreadable() {
    let host = Host::new("smart-timeout");
    host.disk("sda");
    host.fake.then(
        &smartctl_args("sda"),
        Err(LaunchctlError::TimedOut {
            args: "x".to_owned(),
        }),
    );
    let mut source = host.source();
    let read = source.read(None, NOW);
    assert_eq!(
        status(&read),
        (pb::SourceStatus::Unreadable, "/dev/sda: smartctl timed out")
    );
    assert!(read.events.is_empty());
    let read = source.read(None, NOW);
    assert_eq!(
        status(&read),
        (
            pb::SourceStatus::Unreadable,
            "smartctl cannot run: the fake has no answer"
        )
    );
}

#[test]
fn smartctl_not_installed_is_unsupported_and_runs_nothing() {
    let host = Host::new("smart-missing");
    host.disk("sda");
    fs::remove_file(host.smartctl()).expect("uninstall smartctl");
    let read = host.source().read(None, NOW);
    assert_eq!(
        read,
        SourceRead::unsupported(format!("{} is not installed", host.smartctl().display()))
    );
    assert!(host.fake.calls().is_empty());
}

#[test]
fn no_physical_disk_is_unsupported_and_no_listing_is_unreadable() {
    let host = Host::new("smart-no-disk");
    let read = host.source().read(None, NOW);
    assert_eq!(
        status(&read),
        (pb::SourceStatus::Unsupported, "no physical disk is listed")
    );
    fs::remove_dir(host.root().join("sys/block")).expect("remove sys/block");
    let read = host.source().read(None, NOW);
    assert_eq!(read.status, pb::SourceStatus::Unreadable);
    assert!(
        read.reason.starts_with("sys/block is not listed: "),
        "{}",
        read.reason
    );
}

#[test]
fn only_physical_disks_with_plain_names_are_listed_and_read() {
    let host = Host::new("smart-filter");
    for name in [
        "sda", "nvme0n1", "loop0", "dm-0", "sr0", "zram0", "md127", "nbd0", "ram0",
    ] {
        host.disk(name);
    }
    host.disk("fd0");
    host.disk("sdb");
    fs::write(host.root().join("sys/block/sdb/removable"), b"1\n").expect("removable");
    host.disk("-a");
    host.disk("Sdc");
    host.disk(&"a".repeat(33));
    fs::create_dir_all(host.root().join("sys/block/sdd")).expect("no device entry");
    host.disk("sde");
    fs::remove_file(host.root().join("sys/block/sde/removable")).expect("no removable file");
    assert_eq!(
        list_disks(host.root()).expect("listed"),
        ["nvme0n1", "sda", "sde"]
    );
    host.says("sda", 0, "sata_pass");
    host.says("nvme0n1", 0, "nvme_pass");
    host.says("sde", 2, "standby");
    let read = host.source().read(None, NOW);
    assert_eq!(status(&read), (pb::SourceStatus::Ok, ""));
    assert_eq!(
        host.fake.calls(),
        [
            smartctl_args("nvme0n1"),
            smartctl_args("sda"),
            smartctl_args("sde")
        ]
    );
}

#[test]
fn a_failing_disk_that_is_unplugged_is_deasserted_and_forgotten() {
    let host = Host::new("smart-gone");
    host.disk("sda");
    host.disk("sdb");
    host.says("sda", 24, "sata_failing");
    host.says("sdb", 0, "sas_grown");
    let mut source = host.source();
    let (_, kept) = read(&mut source, None, NOW);
    host.unplug("sda");
    let later = NOW + 3600;
    let (read, after) = read(&mut source, Some(&kept), later);
    let why = "it is no longer listed";
    assert_eq!(
        read.events,
        [pb::HardwareEvent {
            time_unix: later,
            ..record(
                &format!("smart:{SATA_KEY}:passing:{later}"),
                "sda",
                pb::HardwareSeverity::Critical,
                pb::HardwareEventState::Deasserted,
                CODE_FAILING,
                &format!("Disk sda is no longer failing: {why}"),
                why,
                &format!("smart:{SATA_KEY}:failing:{NOW}"),
            )
        }]
    );
    let mark: SmartMark = serde_json::from_str(&after).expect("the mark parses");
    assert_eq!(mark.disks.keys().collect::<Vec<_>>(), [SAS_KEY]);
}

#[test]
fn a_disk_replaced_in_its_slot_ends_the_old_disks_assertion() {
    let host = Host::new("smart-replaced");
    host.disk("sda");
    host.says("sda", 24, "sata_failing");
    let mut source = host.source();
    let (_, kept) = read(&mut source, None, NOW);
    let replacement = String::from_utf8(fixture("sata_pass"))
        .expect("utf-8")
        .replace("SN-DISK-0001", "SN-DISK-0002");
    host.fake
        .always(&smartctl_args("sda"), answer(0, replacement.into_bytes()));
    let (read, after) = read(&mut source, Some(&kept), NOW);
    assert_eq!(read.events.len(), 1, "{:?}", read.events);
    assert_eq!(
        read.events[0].state,
        i32::from(pb::HardwareEventState::Deasserted)
    );
    assert_eq!(
        read.events[0].message,
        "Disk sda is no longer failing: sda now holds another disk"
    );
    assert_eq!(
        read.events[0].about,
        format!("smart:{SATA_KEY}:failing:{NOW}")
    );
    let mark: SmartMark = serde_json::from_str(&after).expect("the mark parses");
    assert_eq!(
        mark.disks.keys().collect::<Vec<_>>(),
        [&format!("{:016x}", serial_digest("SN-DISK-0002"))]
    );
}

#[test]
fn a_disk_with_no_serial_is_keyed_by_its_name() {
    let host = Host::new("smart-no-serial");
    host.disk("sda");
    let unnamed = String::from_utf8(fixture("sata_failing"))
        .expect("utf-8")
        .replace(r#""serial_number":"SN-DISK-0001","#, "");
    host.fake
        .always(&smartctl_args("sda"), answer(24, unnamed.into_bytes()));
    let (read, _) = read(&mut host.source(), None, NOW);
    assert_eq!(
        read.events[0].event_id,
        format!("smart:name-sda:failing:{NOW}")
    );
}

#[test]
fn the_worst_disk_decides_the_state() {
    let host = Host::new("smart-worst");
    host.disk("sda");
    host.disk("sdb");
    host.says("sda", 0, "sata_pass");
    host.says("sdb", 4, "no_smart");
    let read = host.source().read(None, NOW);
    assert_eq!(
        status(&read),
        (pb::SourceStatus::Unsupported, "/dev/sdb reports no SMART")
    );
    host.disk("sdc");
    host.says("sdc", 2, "open_failed");
    host.says("sda", 24, "sata_failing");
    let read = host.source().read(None, NOW);
    assert_eq!(read.status, pb::SourceStatus::Unreadable);
    assert!(
        read.reason.starts_with("/dev/sdc did not open"),
        "{}",
        read.reason
    );
    assert_eq!(
        read.events[0].code, CODE_FAILING,
        "the disks read are judged whatever the state"
    );
}

#[test]
fn more_disks_than_the_bound_are_unreadable_and_the_first_are_read() {
    let host = Host::new("smart-many");
    let names: Vec<String> = (0..=MAX_SMART_DISKS).map(|n| format!("sd{n:03}")).collect();
    for name in &names {
        host.disk(name);
        host.says(name, 0, "sata_pass");
    }
    let read = host.source().read(None, NOW);
    assert_eq!(
        status(&read),
        (
            pb::SourceStatus::Unreadable,
            format!(
                "{} disks listed, the first {MAX_SMART_DISKS} read",
                MAX_SMART_DISKS + 1
            )
            .as_str()
        )
    );
    assert_eq!(host.fake.calls().len(), MAX_SMART_DISKS);
    assert!(
        !host
            .fake
            .calls()
            .contains(&smartctl_args(&names[MAX_SMART_DISKS]))
    );
}

#[test]
fn a_mark_that_does_not_parse_is_a_first_read() {
    let host = Host::new("smart-bad-mark");
    host.disk("sda");
    host.says("sda", 24, "sata_failing");
    let (read, _) = read(&mut host.source(), Some("not json"), NOW);
    assert_eq!(read.events.len(), 2, "{:?}", read.events);
}

/// A probe that never says the system is going down.
struct Staying;

impl ShutdownProbe for Staying {
    fn going_down(&self) -> GoingDown {
        GoingDown::No("the tests stay up".to_owned())
    }
}

#[test]
fn the_keeper_holds_the_records_and_reports_the_state() {
    let host = Host::new("smart-keeper");
    let state = Scratch::new("smart-keeper-state");
    let keeper = HealthKeeper::open(
        state.path(),
        BootFacts::default(),
        Box::new(Staying),
        Arc::new(RecordingLog::default()),
    );
    let clock = FakeClock::new(NOW, 0);
    fs::remove_file(host.smartctl()).expect("uninstall smartctl");
    let mut source = host.source();
    assert_eq!(source.name(), SOURCE);
    assert_eq!(source.every_secs(), SMART_EVERY_SECS);
    assert_eq!(poll(&mut source, &keeper, &clock), 0);
    let missing = cut(
        &format!("{} is not installed", host.smartctl().display()),
        MAX_SOURCE_REASON,
    );
    assert_eq!(
        keeper.health().sources,
        [pb::SourceState {
            source: SOURCE.to_owned(),
            status: pb::SourceStatus::Unsupported.into(),
            reason: missing,
        }]
    );
    assert_eq!(
        keeper.mark(SOURCE),
        None,
        "the tool missing is no first read"
    );
    fs::write(host.smartctl(), b"").expect("install smartctl");
    host.disk("sda");
    host.says("sda", 24, "sata_failing");
    assert_eq!(poll(&mut source, &keeper, &clock), 2);
    let health = keeper.health();
    assert_eq!(health.sources[0].status, i32::from(pb::SourceStatus::Ok));
    assert_eq!(health.sources[0].reason, "");
    assert_eq!(
        health.events[0].event_id,
        format!("smart:{SATA_KEY}:failing:{NOW}")
    );
    assert!(keeper.mark(SOURCE).is_some_and(|m| m.contains(SATA_KEY)));
}

#[test]
fn the_failing_bit_with_no_verdict_is_failing() {
    let DiskRead::Read(facts) = parse_smartctl("sdb", Some(8), br#"{"serial_number":"S-1"}"#)
    else {
        panic!("not read");
    };
    assert_eq!(facts.failing.as_deref(), Some("SMART health check failed"));
    assert_eq!(facts.raw, "exit_status=8 passed=false");
    assert_eq!(facts.media, None);
}

#[test]
fn disks_renumbered_between_reads_keep_their_state() {
    let host = Host::new("smart-renumbered");
    host.disk("sda");
    host.disk("sdb");
    host.says("sda", 24, "sata_failing");
    host.says("sdb", 0, "nvme_pass");
    let mut source = host.source();
    let (_, kept) = read(&mut source, None, NOW);
    host.says("sda", 0, "nvme_pass");
    host.says("sdb", 24, "sata_failing");
    let (read, after) = read(&mut source, Some(&kept), NOW + 3600);
    assert!(read.events.is_empty(), "{:?}", read.events);
    let mark: SmartMark = serde_json::from_str(&after).expect("the mark parses");
    assert_eq!(mark.disks[SATA_KEY].name, "sdb");
    assert_eq!(
        mark.disks[SATA_KEY].failing,
        format!("smart:{SATA_KEY}:failing:{NOW}")
    );
    assert_eq!(mark.disks[NVME_KEY].name, "sda");
}

#[test]
fn a_disk_asleep_under_its_name_is_kept_while_its_neighbours_are_read() {
    let host = Host::new("smart-asleep-kept");
    host.disk("sda");
    host.disk("sdb");
    host.says("sda", 24, "sata_failing");
    host.says("sdb", 0, "nvme_pass");
    let mut source = host.source();
    let (_, kept) = read(&mut source, None, NOW);
    host.says("sda", 2, "standby");
    let (read, after) = read(&mut source, Some(&kept), NOW + 3600);
    assert!(read.events.is_empty(), "{:?}", read.events);
    assert_eq!(after, kept);
}

#[test]
fn a_serial_missing_for_one_read_keeps_the_disks_key() {
    let host = Host::new("smart-serial-blip");
    host.disk("sda");
    host.says("sda", 24, "sata_failing");
    let mut source = host.source();
    let (_, kept) = read(&mut source, None, NOW);
    let unnamed = String::from_utf8(fixture("sata_failing"))
        .expect("utf-8")
        .replace(r#""serial_number":"SN-DISK-0001","#, "");
    host.fake
        .always(&smartctl_args("sda"), answer(24, unnamed.into_bytes()));
    let (read, after) = read(&mut source, Some(&kept), NOW + 3600);
    assert!(
        read.events.is_empty(),
        "no end and no new assertion: {:?}",
        read.events
    );
    assert_eq!(after, kept);
}

#[test]
fn every_disk_gone_ends_their_assertions_and_the_source_is_unsupported() {
    let host = Host::new("smart-all-gone");
    host.disk("sda");
    host.says("sda", 24, "sata_failing");
    let mut source = host.source();
    let (_, kept) = read(&mut source, None, NOW);
    host.unplug("sda");
    let (read, after) = read(&mut source, Some(&kept), NOW + 3600);
    assert_eq!(
        status(&read),
        (pb::SourceStatus::Unsupported, "no physical disk is listed")
    );
    assert_eq!(read.events.len(), 1, "{:?}", read.events);
    assert_eq!(
        read.events[0].state,
        i32::from(pb::HardwareEventState::Deasserted)
    );
    let mark: SmartMark = serde_json::from_str(&after).expect("the mark parses");
    assert!(mark.disks.is_empty());
    assert_eq!(
        source.read(Some(&after), NOW + 7200),
        SourceRead::unsupported("no physical disk is listed".to_owned())
    );
}

#[test]
fn a_disk_with_no_media_count_keeps_none_and_its_first_count_is_info() {
    let host = Host::new("smart-no-count");
    host.disk("sdc");
    let bare = br#"{"serial_number":"SN-SAS-0001","smart_status":{"passed":true}}"#;
    host.fake
        .always(&smartctl_args("sdc"), answer(0, bare.to_vec()));
    let mut source = host.source();
    let (first, kept) = read(&mut source, None, NOW);
    assert_eq!(status(&first), (pb::SourceStatus::Ok, ""));
    assert!(first.events.is_empty(), "{:?}", first.events);
    let mark: SmartMark = serde_json::from_str(&kept).expect("the mark parses");
    assert_eq!(mark.disks[SAS_KEY].media, None);
    host.says("sdc", 0, "sas_grown");
    let (counted, _) = read(&mut source, Some(&kept), NOW);
    assert_eq!(
        counted.events,
        [record(
            &format!("smart:{SAS_KEY}:media:3"),
            "sdc",
            pb::HardwareSeverity::Info,
            pb::HardwareEventState::Event,
            CODE_MEDIA,
            "Disk sdc has 3 media errors at its first read",
            "exit_status=0 passed=true grown_defects=3",
            "",
        )]
    );
}

#[test]
fn a_passing_disk_that_is_unplugged_is_forgotten_with_no_record() {
    let host = Host::new("smart-gone-passing");
    host.disk("sda");
    host.disk("sdc");
    host.says("sda", 0, "sata_pass");
    host.says("sdc", 0, "sas_grown");
    let mut source = host.source();
    let (_, kept) = read(&mut source, None, NOW);
    host.unplug("sda");
    let (read, after) = read(&mut source, Some(&kept), NOW + 3600);
    assert!(read.events.is_empty(), "{:?}", read.events);
    let mark: SmartMark = serde_json::from_str(&after).expect("the mark parses");
    assert_eq!(mark.disks.keys().collect::<Vec<_>>(), [SAS_KEY]);
}
