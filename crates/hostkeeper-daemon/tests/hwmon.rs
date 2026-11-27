//! The hwmon temperatures (`hostkeeper_daemon::hwmon`) over a scratch root's
//! `sys/class/hwmon`: a sensor at its critical threshold asserted once and deasserted under its
//! hysteresis, or when it is gone, compared as whole records; the drivers judged and the ones
//! passed over; every bound (the values' range, a file's length, the chips' count, the subject's
//! length) and every failure as the source's state. No real `/sys` is read.

mod support;

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_daemon::hardware_sources::{HardwareSource, SourceRead, poll};
use hostkeeper_daemon::host_health::{BootFacts, HealthKeeper};
use hostkeeper_daemon::hwmon::{
    CODE_THERMAL, HWMON_EVERY_SECS, HwmonMark, HwmonSource, MAX_CHIPS, MAX_MILLI, MIN_MILLI,
    SOURCE, judge, list,
};
use hostkeeper_daemon::shutdown_probe::{GoingDown, ShutdownProbe};
use hostkeeper_link::HostHealthSource;
use hostkeeper_link::testing::{FakeClock, RecordingLog};
use hostkeeper_proto::health::MAX_SUBJECT;
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use support::Scratch;

/// The second the tests read at.
const NOW: u64 = 1_795_662_000;

/// A scratch root with `sys/class/hwmon`.
struct Root(Scratch);

impl Root {
    fn new(name: &str) -> Root {
        let dir = Scratch::new(name);
        fs::create_dir_all(dir.path().join("sys/class/hwmon")).expect("make the hwmon dir");
        Root(dir)
    }

    fn path(&self) -> &Path {
        self.0.path()
    }

    /// The chip `hwmon<n>` of `driver`.
    fn chip(&self, n: u32, driver: &str) -> PathBuf {
        let dir = self.path().join(format!("sys/class/hwmon/hwmon{n}"));
        fs::create_dir_all(&dir).expect("make the chip");
        fs::write(dir.join("name"), format!("{driver}\n")).expect("write its name");
        dir
    }

    fn source(&self) -> HwmonSource {
        HwmonSource::new(self.path())
    }
}

/// Writes `temp<n>_<what>` of `chip` as `value`.
fn set(chip: &Path, n: u32, what: &str, value: &str) {
    fs::write(chip.join(format!("temp{n}_{what}")), format!("{value}\n")).expect("write");
}

/// A sensor `temp<n>` of `chip` labelled `label`, reading `input` against `crit`.
fn sensor(chip: &Path, n: u32, label: &str, input: i64, crit: i64) {
    set(chip, n, "label", label);
    set(chip, n, "input", &input.to_string());
    set(chip, n, "crit", &crit.to_string());
}

fn record(
    event_id: &str,
    subject: &str,
    state: pb::HardwareEventState,
    message: &str,
    raw: &str,
    about: &str,
    now: u64,
) -> pb::HardwareEvent {
    pb::HardwareEvent {
        event_id: event_id.to_owned(),
        time_unix: now,
        of_previous_boot: false,
        component: pb::HardwareComponent::Thermal.into(),
        subject: subject.to_owned(),
        severity: pb::HardwareSeverity::Critical.into(),
        state: state.into(),
        source: SOURCE.to_owned(),
        code: CODE_THERMAL.to_owned(),
        message: message.to_owned(),
        raw: raw.to_owned(),
        about: about.to_owned(),
    }
}

/// Reads `source` at `now` after `mark`: the read, and the mark it keeps.
fn read(source: &mut HwmonSource, mark: Option<&str>, now: u64) -> (SourceRead, String) {
    let read = source.read(mark, now);
    let kept = read.mark.clone().expect("a read that keeps a mark");
    (read, kept)
}

#[test]
fn a_sensor_at_its_crit_is_asserted_once_and_deasserted_under_its_margin() {
    let root = Root::new("hwmon-hot");
    let chip = root.chip(0, "coretemp");
    sensor(&chip, 1, "Package id 0", 100_000, 100_000);
    let mut source = root.source();
    let subject = "coretemp/Package id 0";
    let (hot, kept) = read(&mut source, None, NOW);
    assert_eq!(
        (hot.status, hot.reason.as_str()),
        (pb::SourceStatus::Ok, "")
    );
    let assertion = format!("hwmon:{subject}:hot:{NOW}");
    assert_eq!(
        hot.events,
        [record(
            &assertion,
            subject,
            pb::HardwareEventState::Asserted,
            "Sensor coretemp/Package id 0 at 100 C, at or over its critical 100 C",
            "hwmon0 coretemp: temp1_input=100000 temp1_crit=100000 clear_at=95000",
            "",
            NOW,
        )]
    );
    set(&chip, 1, "input", "101000");
    let (still, kept) = read(&mut source, Some(&kept), NOW + 60);
    assert!(still.events.is_empty(), "still hot: {:?}", still.events);
    set(&chip, 1, "input", "95001");
    let (warm, kept) = read(&mut source, Some(&kept), NOW + 120);
    assert!(warm.events.is_empty(), "over its margin: {:?}", warm.events);
    set(&chip, 1, "input", "95000");
    let later = NOW + 180;
    let (cool, kept) = read(&mut source, Some(&kept), later);
    assert_eq!(
        cool.events,
        [record(
            &format!("hwmon:{subject}:cool:{later}"),
            subject,
            pb::HardwareEventState::Deasserted,
            "Sensor coretemp/Package id 0 back to 95 C, under its critical 100 C",
            "hwmon0 coretemp: temp1_input=95000 temp1_crit=100000 clear_at=95000",
            &assertion,
            later,
        )]
    );
    let mark: HwmonMark = serde_json::from_str(&kept).expect("the mark parses");
    assert!(mark.hot.is_empty());
}

#[test]
fn one_millidegree_under_crit_is_not_asserted() {
    let root = Root::new("hwmon-under");
    let chip = root.chip(0, "k10temp");
    sensor(&chip, 1, "Tctl", 99_999, 100_000);
    let (read, _) = read(&mut root.source(), None, NOW);
    assert!(read.events.is_empty(), "{:?}", read.events);
    assert_eq!(read.status, pb::SourceStatus::Ok);
}

#[test]
fn the_drivers_hysteresis_ends_the_assertion_when_it_is_under_crit() {
    let root = Root::new("hwmon-hyst");
    let chip = root.chip(0, "coretemp");
    sensor(&chip, 1, "Core 0", 105_000, 100_000);
    set(&chip, 1, "crit_hyst", "90000");
    let listing = list(root.path()).expect("listed");
    assert_eq!(listing.sensors[0].clear_at, 90_000);
    set(&chip, 1, "crit_hyst", "100000");
    let listing = list(root.path()).expect("listed");
    assert_eq!(
        listing.sensors[0].clear_at, 95_000,
        "a hysteresis at crit is not under it"
    );
    let mut mark = HwmonMark::default();
    set(&chip, 1, "crit_hyst", "90000");
    let hot = judge(&list(root.path()).expect("listed"), &mut mark, NOW);
    assert_eq!(hot.len(), 1);
    set(&chip, 1, "input", "92000");
    assert!(judge(&list(root.path()).expect("listed"), &mut mark, NOW).is_empty());
    set(&chip, 1, "input", "90000");
    let cool = judge(&list(root.path()).expect("listed"), &mut mark, NOW);
    assert_eq!(cool[0].state, i32::from(pb::HardwareEventState::Deasserted));
}

#[test]
fn only_the_named_drivers_with_a_crit_are_judged() {
    let root = Root::new("hwmon-drivers");
    let acpi = root.chip(0, "acpitz");
    sensor(&acpi, 1, "zone", 120_000, 100_000);
    let nocrit = root.chip(1, "coretemp");
    set(&nocrit, 1, "input", "120000");
    set(&nocrit, 2, "label", "no input");
    set(&nocrit, 2, "crit", "100000");
    let bad = root.chip(2, "nvme");
    sensor(&bad, 1, "Composite", 120_000, 0);
    sensor(&bad, 2, "Sensor 1", 120_000, MAX_MILLI + 1);
    let read = root.source().read(None, NOW);
    assert_eq!(
        read,
        SourceRead::unsupported(
            "no coretemp, k10temp or nvme sensor with a critical threshold".to_owned()
        )
    );
}

#[test]
fn no_hwmon_directory_is_unsupported_and_one_not_listed_is_unreadable() {
    let root = Root::new("hwmon-absent");
    fs::remove_dir(root.path().join("sys/class/hwmon")).expect("remove it");
    assert_eq!(
        root.source().read(None, NOW),
        SourceRead::unsupported("sys/class/hwmon is absent".to_owned())
    );
    fs::write(root.path().join("sys/class/hwmon"), b"").expect("a file in its place");
    let read = root.source().read(None, NOW);
    assert_eq!(read.status, pb::SourceStatus::Unreadable);
    assert!(
        read.reason.starts_with("sys/class/hwmon is not listed: "),
        "{}",
        read.reason
    );
}

#[test]
fn a_value_out_of_bounds_or_not_a_number_is_unreadable_and_the_rest_judged() {
    let root = Root::new("hwmon-bounds");
    let chip = root.chip(0, "coretemp");
    sensor(&chip, 1, "Core 0", MAX_MILLI + 1, 100_000);
    sensor(&chip, 2, "Core 1", 100_000, 100_000);
    let read = root.source().read(None, NOW);
    assert_eq!(
        (read.status, read.reason.as_str()),
        (
            pb::SourceStatus::Unreadable,
            "coretemp/Core 0: 200001 is outside -40000 to 200000"
        )
    );
    assert_eq!(read.events.len(), 1, "Core 1 is judged: {:?}", read.events);
    assert_eq!(read.events[0].subject, "coretemp/Core 1");
    set(&chip, 1, "input", "hot");
    let read = root.source().read(None, NOW);
    assert_eq!(read.reason, r#"coretemp/Core 0: "hot" is not a number"#);
    set(&chip, 1, "input", &format!("1{}", "0".repeat(100)));
    let read = root.source().read(None, NOW);
    assert_eq!(
        read.reason,
        format!(r#"coretemp/Core 0: "1{}" is not a number"#, "0".repeat(63)),
        "a file is read to 64 bytes"
    );
}

#[test]
fn the_ends_of_the_ranges_are_readings() {
    let root = Root::new("hwmon-ends");
    let chip = root.chip(0, "coretemp");
    sensor(&chip, 1, "low", MIN_MILLI, 1);
    sensor(&chip, 2, "high", MAX_MILLI, MAX_MILLI);
    let listing = list(root.path()).expect("listed");
    assert_eq!(listing.problem, None);
    let inputs: Vec<(i64, i64)> = listing.sensors.iter().map(|s| (s.input, s.crit)).collect();
    assert_eq!(inputs, [(MIN_MILLI, 1), (MAX_MILLI, MAX_MILLI)]);
    set(&chip, 1, "input", &(MIN_MILLI - 1).to_string());
    let listing = list(root.path()).expect("listed");
    assert_eq!(
        listing.problem.as_deref(),
        Some("coretemp/low: -40001 is outside -40000 to 200000")
    );
}

#[test]
fn a_sensor_with_no_label_is_named_by_its_file() {
    let root = Root::new("hwmon-nolabel");
    let chip = root.chip(0, "coretemp");
    set(&chip, 2, "input", "40000");
    set(&chip, 2, "crit", "100000");
    set(&chip, 3, "input", "40000");
    set(&chip, 3, "crit", "100000");
    set(&chip, 3, "label", "");
    let listing = list(root.path()).expect("listed");
    let subjects: Vec<&str> = listing.sensors.iter().map(|s| s.subject.as_str()).collect();
    assert_eq!(subjects, ["coretemp/temp2", "coretemp/temp3"]);
}

#[test]
fn chips_of_one_driver_are_named_by_their_device() {
    let root = Root::new("hwmon-two-nvme");
    let first = root.chip(1, "nvme");
    symlink("../../devices/pci0000:00/nvme/nvme0", first.join("device")).expect("link");
    sensor(&first, 1, "Composite", 40_000, 84_850);
    let second = root.chip(2, "nvme");
    symlink("../../devices/pci0000:00/nvme/nvme1", second.join("device")).expect("link");
    sensor(&second, 1, "Composite", 40_000, 84_850);
    let third = root.chip(3, "nvme");
    sensor(&third, 1, "Composite", 40_000, 84_850);
    let solo = root.chip(4, "coretemp");
    symlink("../../devices/platform/coretemp.0", solo.join("device")).expect("link");
    sensor(&solo, 1, "Package id 0", 40_000, 100_000);
    let listing = list(root.path()).expect("listed");
    let subjects: Vec<&str> = listing.sensors.iter().map(|s| s.subject.as_str()).collect();
    assert_eq!(
        subjects,
        [
            "nvme0/Composite",
            "nvme1/Composite",
            "hwmon3/Composite",
            "coretemp/Package id 0"
        ]
    );
}

#[test]
fn a_long_subject_is_cut_to_its_bound() {
    let root = Root::new("hwmon-long");
    let chip = root.chip(0, "coretemp");
    sensor(&chip, 1, &"x".repeat(40), 100_000, 100_000);
    let (read, _) = read(&mut root.source(), None, NOW);
    let subject = format!("coretemp/{}", "x".repeat(MAX_SUBJECT - "coretemp/".len()));
    assert_eq!(read.events[0].subject, subject);
    assert_eq!(
        read.events[0].event_id,
        format!("hwmon:{subject}:hot:{NOW}")
    );
    assert!(read.events[0].event_id.len() <= 64);
}

#[test]
fn a_hot_sensor_no_longer_listed_is_deasserted() {
    let root = Root::new("hwmon-gone");
    let chip = root.chip(0, "nvme");
    sensor(&chip, 1, "Composite", 90_000, 84_850);
    let other = root.chip(1, "coretemp");
    sensor(&other, 1, "Core 0", 40_000, 100_000);
    let mut source = root.source();
    let (_, kept) = read(&mut source, None, NOW);
    fs::remove_dir_all(&chip).expect("the disk is pulled");
    let later = NOW + 60;
    let (read, kept) = read(&mut source, Some(&kept), later);
    assert_eq!(
        read.events,
        [record(
            &format!("hwmon:nvme/Composite:cool:{later}"),
            "nvme/Composite",
            pb::HardwareEventState::Deasserted,
            "Sensor nvme/Composite is no longer listed",
            "",
            &format!("hwmon:nvme/Composite:hot:{NOW}"),
            later,
        )]
    );
    let mark: HwmonMark = serde_json::from_str(&kept).expect("the mark parses");
    assert!(mark.hot.is_empty());
}

#[test]
fn a_hot_sensor_that_cannot_be_read_stays_asserted() {
    let root = Root::new("hwmon-unread");
    let chip = root.chip(0, "coretemp");
    sensor(&chip, 1, "Core 0", 100_000, 100_000);
    let mut source = root.source();
    let (_, kept) = read(&mut source, None, NOW);
    set(&chip, 1, "input", "garbage");
    let (read, after) = read(&mut source, Some(&kept), NOW + 60);
    assert_eq!(read.status, pb::SourceStatus::Unreadable);
    assert!(read.events.is_empty(), "{:?}", read.events);
    assert_eq!(after, kept);
}

#[test]
fn more_chips_than_the_bound_are_unreadable_and_end_nothing() {
    let root = Root::new("hwmon-many");
    let first = root.chip(0, "coretemp");
    sensor(&first, 1, "Core 0", 100_000, 100_000);
    let mut source = root.source();
    let (_, kept) = read(&mut source, None, NOW);
    for n in 1..=u32::try_from(MAX_CHIPS).expect("fits") {
        root.chip(n + 100, "acpitz");
    }
    fs::remove_file(first.join("temp1_crit")).expect("the hot sensor is no longer judged");
    let (read, after) = read(&mut source, Some(&kept), NOW + 60);
    assert_eq!(
        (read.status, read.reason.as_str()),
        (
            pb::SourceStatus::Unreadable,
            format!("{} chips listed, the first {MAX_CHIPS} read", MAX_CHIPS + 1).as_str()
        )
    );
    assert!(read.events.is_empty(), "{:?}", read.events);
    assert_eq!(after, kept, "an incomplete listing ends no assertion");
}

#[test]
fn a_mark_that_does_not_parse_is_empty() {
    let root = Root::new("hwmon-bad-mark");
    let chip = root.chip(0, "coretemp");
    sensor(&chip, 1, "Core 0", 100_000, 100_000);
    let (read, _) = read(&mut root.source(), Some("{"), NOW);
    assert_eq!(read.events.len(), 1);
}

/// A probe that never says the system is going down.
struct Staying;

impl ShutdownProbe for Staying {
    fn going_down(&self) -> GoingDown {
        GoingDown::No("the tests stay up".to_owned())
    }
}

#[test]
fn the_keeper_holds_the_assertion_and_reports_the_state() {
    let root = Root::new("hwmon-keeper");
    let state = Scratch::new("hwmon-keeper-state");
    let keeper = HealthKeeper::open(
        state.path(),
        BootFacts::default(),
        Box::new(Staying),
        Arc::new(RecordingLog::default()),
    );
    let chip = root.chip(0, "coretemp");
    sensor(&chip, 1, "Core 0", 100_000, 100_000);
    let mut source = root.source();
    assert_eq!(source.name(), SOURCE);
    assert_eq!(source.every_secs(), HWMON_EVERY_SECS);
    let clock = FakeClock::new(NOW, 0);
    assert_eq!(poll(&mut source, &keeper, &clock), 1);
    assert_eq!(
        poll(&mut source, &keeper, &clock),
        0,
        "still hot: no record"
    );
    let health = keeper.health();
    assert_eq!(
        health.sources,
        [pb::SourceState {
            source: SOURCE.to_owned(),
            status: pb::SourceStatus::Ok.into(),
            reason: String::new(),
        }]
    );
    assert_eq!(health.events[0].code, CODE_THERMAL);
}

#[test]
fn a_crit_that_cannot_be_read_is_unreadable_and_keeps_the_assertion() {
    let root = Root::new("hwmon-bad-crit");
    let chip = root.chip(0, "coretemp");
    sensor(&chip, 1, "Core 0", 100_000, 100_000);
    let mut source = root.source();
    let (_, kept) = read(&mut source, None, NOW);
    set(&chip, 1, "crit", "x");
    let (read, after) = read(&mut source, Some(&kept), NOW + 60);
    assert_eq!(
        (read.status, read.reason.as_str()),
        (
            pb::SourceStatus::Unreadable,
            r#"coretemp/Core 0: temp1_crit: "x" is not a number"#
        )
    );
    assert!(read.events.is_empty(), "not taken for gone: {:?}", read.events);
    assert_eq!(after, kept);
}

#[test]
fn the_last_hot_sensor_gone_ends_its_assertion_and_the_source_is_unsupported() {
    let root = Root::new("hwmon-last-gone");
    let chip = root.chip(0, "coretemp");
    sensor(&chip, 1, "Core 0", 100_000, 100_000);
    let mut source = root.source();
    let (_, kept) = read(&mut source, None, NOW);
    fs::remove_dir_all(root.path().join("sys/class/hwmon")).expect("the driver is unloaded");
    let (read, after) = read(&mut source, Some(&kept), NOW + 60);
    assert_eq!(
        (read.status, read.reason.as_str()),
        (pb::SourceStatus::Unsupported, "sys/class/hwmon is absent")
    );
    assert_eq!(read.events.len(), 1, "{:?}", read.events);
    assert_eq!(read.events[0].message, "Sensor coretemp/Core 0 is no longer listed");
    assert_eq!(read.events[0].about, format!("hwmon:coretemp/Core 0:hot:{NOW}"));
    let mark: HwmonMark = serde_json::from_str(&after).expect("the mark parses");
    assert!(mark.hot.is_empty());
    assert_eq!(
        source.read(Some(&after), NOW + 120),
        SourceRead::unsupported("sys/class/hwmon is absent".to_owned()),
        "nothing more to end: the mark is kept as it is"
    );
}

#[test]
fn a_chip_listing_no_sensor_after_one_was_hot_is_unsupported_with_the_end() {
    let root = Root::new("hwmon-no-sensor-left");
    let chip = root.chip(0, "coretemp");
    sensor(&chip, 1, "Core 0", 100_000, 100_000);
    let mut source = root.source();
    let (_, kept) = read(&mut source, None, NOW);
    fs::remove_file(chip.join("temp1_crit")).expect("its threshold is gone");
    let (read, _) = read(&mut source, Some(&kept), NOW + 60);
    assert_eq!(
        (read.status, read.reason.as_str()),
        (
            pb::SourceStatus::Unsupported,
            "no coretemp, k10temp or nvme sensor with a critical threshold"
        )
    );
    assert_eq!(read.events.len(), 1, "{:?}", read.events);
    assert!(read.mark.is_some());
}
