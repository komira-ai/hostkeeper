//! A Mac's internal disk's SMART verdict (`hostkeeper_daemon::mac_disk`): `diskutil info -plist
//! disk0` read from fixtures written from the format `diskutil` prints (hand-written samples, each
//! to be replaced by a capture from a Mac), judged into whole records (failing asserted once,
//! verified again deasserted about it), and every failure of the program or its output as the
//! source's state; the source driven as the daemon drives it into a keeper over a scratch state
//! directory. `diskutil` is the scripted fake: nothing runs a program.

mod support;

use std::fs;
use std::sync::Arc;

use hostkeeper_daemon::hardware_sources::{HardwareSource, SourceRead, poll};
use hostkeeper_daemon::host_health::{BootFacts, HealthKeeper};
use hostkeeper_daemon::mac_disk::{
    DISKUTIL_ARGS, DISKUTIL_EVERY_SECS, DiskInfo, DiskutilSource, SOURCE, SmartStatus, judge,
    parse_info,
};
use hostkeeper_daemon::shutdown_probe::{GoingDown, ShutdownProbe};
use hostkeeper_launchd::{FakeLaunchctl, LaunchctlError, LaunchctlOutput};
use hostkeeper_link::HostHealthSource;
use hostkeeper_link::testing::{FakeClock, RecordingLog};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use support::Scratch;

/// The second the tests read at.
const NOW: u64 = 1_795_662_000;

/// The disk's name in the fixtures.
const MEDIA: &str = "APPLE SSD AP0512Z";

fn fixture(name: &str) -> String {
    let path = format!(
        "{}/tests/fixtures/mac_health/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn args() -> Vec<String> {
    DISKUTIL_ARGS.iter().map(|a| (*a).to_owned()).collect()
}

fn info(status: SmartStatus, word: &str) -> DiskInfo {
    DiskInfo {
        status,
        word: word.to_owned(),
        media: MEDIA.to_owned(),
    }
}

/// The record `diskutil` gives of `disk0`.
fn record(
    event_id: &str,
    at: u64,
    state: pb::HardwareEventState,
    message: &str,
    word: &str,
    about: &str,
) -> pb::HardwareEvent {
    pb::HardwareEvent {
        event_id: event_id.to_owned(),
        time_unix: at,
        of_previous_boot: false,
        component: pb::HardwareComponent::Disk.into(),
        subject: "disk0".to_owned(),
        severity: pb::HardwareSeverity::Critical.into(),
        state: state.into(),
        source: "diskutil".to_owned(),
        code: "disk_failing".to_owned(),
        message: message.to_owned(),
        raw: format!("disk0 SMARTStatus {word} (MediaName {MEDIA})"),
        about: about.to_owned(),
    }
}

fn failing(at: u64) -> pb::HardwareEvent {
    record(
        &format!("diskutil:disk0:failing:{at}"),
        at,
        pb::HardwareEventState::Asserted,
        "Disk disk0 says it is failing (SMART status Failing)",
        "Failing",
        "",
    )
}

fn passing(at: u64, about: &str) -> pb::HardwareEvent {
    record(
        &format!("diskutil:disk0:passing:{at}"),
        at,
        pb::HardwareEventState::Deasserted,
        "Disk disk0 passes its SMART check again (SMART status Verified)",
        "Verified",
        about,
    )
}

/// A `diskutil` whose next answers are `answers`, in order.
fn diskutil(answers: Vec<Result<LaunchctlOutput, LaunchctlError>>) -> FakeLaunchctl {
    let fake = FakeLaunchctl::new();
    for answer in answers {
        fake.then(&args(), answer);
    }
    fake
}

fn printed(name: &str) -> Result<LaunchctlOutput, LaunchctlError> {
    Ok(LaunchctlOutput::success(&fixture(name)))
}

struct Never;

impl ShutdownProbe for Never {
    fn going_down(&self) -> GoingDown {
        GoingDown::No("a test".to_owned())
    }
}

fn keeper(dir: &Scratch) -> Arc<HealthKeeper> {
    Arc::new(HealthKeeper::open(
        dir.path(),
        BootFacts {
            boot_id: Some("boot-1".to_owned()),
            boot_unix: NOW - 3600,
            machine_id: String::new(),
        },
        Box::new(Never),
        Arc::new(RecordingLog::default()),
    ))
}

fn state_of(keeper: &HealthKeeper) -> Option<pb::SourceState> {
    keeper
        .health()
        .sources
        .into_iter()
        .find(|s| s.source == SOURCE)
}

/// Proves each verdict of the fixtures is read with its word and the disk's name, and a disk with
/// no name has an empty one.
///
/// Catches: a failing disk read as another verdict, or the name the record's `raw` carries lost.
#[test]
fn the_verdicts_of_the_fixtures_are_read() {
    assert_eq!(
        parse_info(fixture("diskutil-verified.plist").as_bytes()),
        Ok(info(SmartStatus::Verified, "Verified"))
    );
    assert_eq!(
        parse_info(fixture("diskutil-failing.plist").as_bytes()),
        Ok(info(SmartStatus::Failing, "Failing"))
    );
    assert_eq!(
        parse_info(fixture("diskutil-not-supported.plist").as_bytes()),
        Ok(info(SmartStatus::NotSupported, "Not Supported"))
    );
    let unnamed = fixture("diskutil-verified.plist")
        .replace("<key>MediaName</key>", "<key>OtherName</key>")
        .replace("Verified", "About to Fail");
    assert_eq!(
        parse_info(unnamed.as_bytes()),
        Ok(DiskInfo {
            status: SmartStatus::Other("About to Fail".to_owned()),
            word: "About to Fail".to_owned(),
            media: String::new(),
        })
    );
}

/// Proves output that is not a property list, one whose top is not a dictionary, and one with no
/// `SMARTStatus` string each give their reason and no verdict.
///
/// Catches: a broken answer read as a disk with no fault.
#[test]
fn output_with_no_verdict_is_refused_with_its_reason() {
    let plist = |body: &str| {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n{body}\n</plist>\n"
        )
    };
    assert_eq!(
        parse_info(b"Could not find disk: disk0"),
        Err("diskutil printed no property list".to_owned())
    );
    assert_eq!(
        parse_info(plist("<array><string>Verified</string></array>").as_bytes()),
        Err("diskutil printed no dictionary".to_owned())
    );
    assert_eq!(
        parse_info(plist("<dict><key>MediaName</key><string>x</string></dict>").as_bytes()),
        Err("diskutil printed no SMARTStatus of disk0".to_owned())
    );
    assert_eq!(
        parse_info(plist("<dict><key>SMARTStatus</key><integer>1</integer></dict>").as_bytes()),
        Err("diskutil printed no SMARTStatus of disk0".to_owned())
    );
}

/// Proves the rule: `Failing` with nothing open asserts, `Failing` while open makes nothing,
/// `Verified` while open deasserts about the assertion, and `Verified`, `Not Supported` or an
/// unknown word with nothing open makes nothing; the mark follows.
///
/// Catches: a failing disk recorded at every read, a recovery not recorded, or a disk that
/// stopped reporting read as passing.
#[test]
fn failing_asserts_once_and_verified_ends_it() {
    let failing_info = info(SmartStatus::Failing, "Failing");
    let verified = info(SmartStatus::Verified, "Verified");
    let open = format!("diskutil:disk0:failing:{NOW}");

    assert_eq!(
        judge(&failing_info, "", NOW),
        (open.clone(), Some(failing(NOW)))
    );
    assert_eq!(judge(&failing_info, &open, NOW + 900), (open.clone(), None));
    assert_eq!(
        judge(&verified, &open, NOW + 1800),
        (String::new(), Some(passing(NOW + 1800, &open)))
    );
    assert_eq!(judge(&verified, "", NOW), (String::new(), None));
    let silent = info(SmartStatus::NotSupported, "Not Supported");
    assert_eq!(judge(&silent, &open, NOW), (open.clone(), None));
    let other = info(
        SmartStatus::Other("About to Fail".to_owned()),
        "About to Fail",
    );
    assert_eq!(judge(&other, &open, NOW), (open.clone(), None));
    assert_eq!(judge(&other, "", NOW), (String::new(), None));
}

/// Proves the source as the daemon drives it: a verified disk is `ok` with no record and the
/// state file untouched; failing holds one critical assertion, kept once however often it is
/// read; `Not Supported` is `unsupported` and keeps the assertion; verified again holds the
/// deassertion about it. `diskutil` is asked exactly `info -plist disk0` each time.
///
/// Catches: a failing Mac disk never reaching the coordinator, reported at every read, or its
/// end lost.
#[test]
fn a_failing_disk_is_held_once_and_its_end_too() {
    let dir = Scratch::new("mac-disk-cycle");
    let keeper = keeper(&dir);
    let clock = FakeClock::new(NOW, 0);
    let fake = diskutil(vec![
        printed("diskutil-verified.plist"),
        printed("diskutil-failing.plist"),
        printed("diskutil-failing.plist"),
        printed("diskutil-not-supported.plist"),
        printed("diskutil-verified.plist"),
    ]);
    let mut source = DiskutilSource::new(Box::new(fake.clone()));
    assert_eq!(source.name(), SOURCE);
    assert_eq!(source.every_secs(), DISKUTIL_EVERY_SECS);

    assert_eq!(poll(&mut source, &keeper, &clock), 0);
    assert_eq!(
        state_of(&keeper).map(|s| s.status),
        Some(i32::from(pb::SourceStatus::Ok))
    );
    assert_eq!(keeper.mark(SOURCE), None, "nothing to keep yet");

    clock.advance(DISKUTIL_EVERY_SECS);
    let failed_at = NOW + DISKUTIL_EVERY_SECS;
    assert_eq!(poll(&mut source, &keeper, &clock), 1);
    assert_eq!(keeper.health().events, vec![failing(failed_at)]);
    let open = format!("diskutil:disk0:failing:{failed_at}");
    assert_eq!(keeper.mark(SOURCE), Some(open.clone()));

    clock.advance(DISKUTIL_EVERY_SECS);
    assert_eq!(poll(&mut source, &keeper, &clock), 0);

    clock.advance(DISKUTIL_EVERY_SECS);
    assert_eq!(poll(&mut source, &keeper, &clock), 0);
    assert_eq!(
        state_of(&keeper),
        Some(pb::SourceState {
            source: SOURCE.to_owned(),
            status: pb::SourceStatus::Unsupported.into(),
            reason: "disk0 reports no SMART status".to_owned(),
        })
    );
    assert_eq!(keeper.mark(SOURCE), Some(open.clone()));

    clock.advance(DISKUTIL_EVERY_SECS);
    let passed_at = NOW + 4 * DISKUTIL_EVERY_SECS;
    assert_eq!(poll(&mut source, &keeper, &clock), 1);
    assert_eq!(
        keeper.health().events,
        vec![failing(failed_at), passing(passed_at, &open)]
    );
    assert_eq!(keeper.mark(SOURCE), Some(String::new()));
    assert_eq!(fake.calls(), vec![args(); 5]);
}

/// Proves every failure is the source `unreadable` with its reason, no record and the mark
/// kept: `diskutil` not run, exiting 1, ended by a signal, printing no property list, and a
/// verdict this release does not know.
///
/// Catches: a Mac whose disk cannot be read shown as one with a healthy disk.
#[test]
fn every_failure_is_unreadable_with_its_reason() {
    let unknown = fixture("diskutil-verified.plist").replace("Verified", "About to Fail");
    let cases: Vec<(Result<LaunchctlOutput, LaunchctlError>, &str)> = vec![
        (
            Err(LaunchctlError::Spawn {
                args: "info -plist disk0".to_owned(),
                why: "no such file".to_owned(),
            }),
            "diskutil did not run: cannot run launchctl info -plist disk0: no such file",
        ),
        (
            Ok(LaunchctlOutput::exit(1, "Could not find disk: disk0")),
            "diskutil exited 1",
        ),
        (
            Ok(LaunchctlOutput {
                code: None,
                stdout: Vec::new(),
                stderr: Vec::new(),
            }),
            "diskutil was ended by a signal",
        ),
        (
            Ok(LaunchctlOutput::success("not a plist")),
            "diskutil printed no property list",
        ),
        (
            Ok(LaunchctlOutput::success(&unknown)),
            "disk0 has the SMART status \"About to Fail\"",
        ),
    ];
    for (answer, why) in cases {
        let mut source = DiskutilSource::new(Box::new(diskutil(vec![answer])));
        assert_eq!(
            source.read(Some("diskutil:disk0:failing:1"), NOW),
            SourceRead::unreadable(why.to_owned()),
            "for {why}"
        );
    }
}
