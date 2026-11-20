//! The maintenance hook's failures over fakes: a run record that cannot be read or written, a
//! boot id that cannot be read, a reboot that cannot be asked for, a kind that did not take after
//! its reboot, a kind with no work and parameters that cannot be read. Each says why, and none goes
//! on to a later kind or a reboot.

mod support_maintenance;

use std::io::{self, Read};
use std::path::PathBuf;

use hostkeeper_daemon::cli::DEFAULT_PACKAGE_LOCK_WAIT_SECS;
use hostkeeper_daemon::maintenance_hook::{
    HookArgs, HookPorts, HookStep, Kind, KindError, KindState, REASON_BOOT_ID_UNREADABLE,
    REASON_REBOOT_FAILED, REASON_RUN_RECORD_FAILED, REASON_UNSUPPORTED_KIND, RecordError, Unwired,
    main_with, run_step,
};
use hostkeeper_hook::Outcome;
use hostkeeper_proto::hostkeeper::agent::v1::MaintenanceKind as WireKind;
use hostkeeper_steps::BootId;
use support_maintenance::{
    BOOT_A, BOOT_B, Effects, FakeKind, FakeRecord, Host, failed, params, rebooting,
};

/// Proves: when the run record cannot be written, a kind that needs a reboot fails
/// `run_record_failed` and the reboot is never asked for; a record that cannot be read fails
/// every step that reads it before any kind runs.
///
/// Catches: a reboot asked for with no record of it, which the next boot would read as a kind
/// never installed.
#[test]
fn no_reboot_is_asked_for_when_the_record_fails() {
    let effects = Effects::default();
    let host = Host::new(
        &effects,
        vec![FakeKind::rebooting(Kind::Macos, "26.2", &effects)],
    );
    let unwired = HookPorts {
        record: &Unwired,
        ..host.ports()
    };
    let ending = run_step(HookStep::Cutover, &params(&[WireKind::Macos]), &unwired);
    assert_eq!(ending.outcome, Outcome::Failure);
    assert_eq!(
        ending.report().to_line(),
        failed(
            REASON_RUN_RECORD_FAILED,
            "this host keeps no run record yet"
        )
        .line
    );
    assert_eq!(host.effects(), ["macos cutover"]);

    let broken = Host {
        record: FakeRecord {
            fail: Some(RecordError::Failed(
                "the record's file is not a record".to_owned(),
            )),
            ..FakeRecord::new(&effects)
        },
        ..host
    };
    let expected = failed(
        REASON_RUN_RECORD_FAILED,
        "the run record failed: the record's file is not a record",
    );
    for step in [HookStep::Cutover, HookStep::Verify] {
        assert_eq!(broken.run(step, &[WireKind::Macos]), expected);
    }
    assert_eq!(broken.effects(), Vec::<String>::new());
}

/// Proves: a boot id that cannot be read fails `boot_id_unreadable` before the record is written
/// or the reboot asked for, both when a kind first needs its reboot and when a later cutover
/// looks for the reboot's end.
///
/// Catches: a reboot recorded under an empty boot id, which every later boot would read as
/// changed or never changed.
#[test]
fn an_unreadable_boot_id_fails_before_the_record_or_the_reboot() {
    let effects = Effects::default();
    let host = Host::new(
        &effects,
        vec![FakeKind::rebooting(Kind::Linux, "6.8.0-60", &effects)],
    );
    host.boot.set("");
    let expected = failed(
        REASON_BOOT_ID_UNREADABLE,
        "the boot id read from the test's boot id is empty",
    );
    assert_eq!(host.run(HookStep::Cutover, &[WireKind::Linux]), expected);
    assert_eq!(host.effects(), ["linux cutover"]);
    let boot = BootId::uuid("test", BOOT_A).expect("uuid");
    host.record
        .set(Kind::Linux, KindState::RebootUnderWay { boot });
    assert_eq!(host.run(HookStep::Cutover, &[WireKind::Linux]), expected);
    assert_eq!(host.effects(), Vec::<String>::new());
}

/// Proves: a reboot that cannot be asked for fails `reboot_failed` with the rebooter's error,
/// after the record of the reboot under way.
///
/// Catches: a failed reboot reported as under way, which would leave the run waiting for a host
/// that never goes down.
#[test]
fn a_reboot_that_fails_is_the_steps_failure() {
    let effects = Effects::default();
    let mut host = Host::new(
        &effects,
        vec![FakeKind::rebooting(Kind::Linux, "6.8.0-60", &effects)],
    );
    host.rebooter.fail = true;
    assert_eq!(
        host.run(HookStep::Cutover, &[WireKind::Linux]),
        failed(REASON_REBOOT_FAILED, "the reboot failed: shutdown exited 1")
    );
    assert_eq!(
        host.effects(),
        [
            "linux cutover".to_owned(),
            format!("record run-7 linux reboot_under_way {BOOT_A}"),
            "reboot".to_owned(),
        ]
    );
}

/// Proves: after the reboot, a kind whose new version cannot be read fails with the kind's
/// reason and is not recorded applied, and the next kind is not run.
///
/// Catches: a kind recorded applied when the reboot came back on the old version.
#[test]
fn a_kind_that_did_not_take_after_its_reboot_is_not_recorded_applied() {
    let effects = Effects::default();
    let mut macos = FakeKind::rebooting(Kind::Macos, "26.2", &effects);
    macos.after_reboot = Err(KindError::Failed {
        reason: "build_unchanged",
        detail: "sw_vers reports 25B78 after the reboot".to_owned(),
    });
    let host = Host::new(
        &effects,
        vec![macos, FakeKind::applying(Kind::Xcode, "27.2", &effects)],
    );
    let kinds = [WireKind::Macos, WireKind::Xcode];
    assert_eq!(host.run(HookStep::Cutover, &kinds), rebooting("macos"));
    host.effects();
    host.boot.set(BOOT_B);
    assert_eq!(
        host.run(HookStep::Cutover, &kinds),
        failed("build_unchanged", "sw_vers reports 25B78 after the reboot")
    );
    assert_eq!(host.effects(), ["macos after_reboot"]);
}

/// Proves: a kind the registry holds no work for fails `unsupported_kind` in every step before
/// any kind runs, naming the kind.
///
/// Catches: a kind of the order silently passed over, so the run reports done with nothing
/// applied.
#[test]
fn a_kind_without_work_fails_unsupported_kind_in_every_step() {
    let effects = Effects::default();
    let host = Host::new(
        &effects,
        vec![FakeKind::applying(Kind::Xcode, "27.2", &effects)],
    );
    let expected = failed(
        REASON_UNSUPPORTED_KIND,
        "macos is not supported on this host",
    );
    host.record.set(Kind::Macos, KindState::Applied);
    assert_eq!(host.run(HookStep::Verify, &[WireKind::Macos]), expected);
    host.record.set(Kind::Macos, KindState::Pending);
    for step in [HookStep::Prepare, HookStep::Cutover] {
        assert_eq!(
            host.run(step, &[WireKind::Macos, WireKind::Xcode]),
            expected
        );
    }
    let boot = BootId::uuid("test", BOOT_A).expect("uuid");
    host.record
        .set(Kind::Macos, KindState::RebootUnderWay { boot });
    host.boot.set(BOOT_B);
    assert_eq!(host.run(HookStep::Cutover, &[WireKind::Macos]), expected);
    assert_eq!(host.effects(), Vec::<String>::new());
}

/// A reader that fails.
struct Broken;

impl Read for Broken {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::other("the pipe broke"))
    }
}

/// Proves: parameters whose standard input cannot be read fail `params_invalid`, saying so, and
/// run no kind.
///
/// Catches: a read error taken as empty parameters.
#[test]
fn unreadable_parameters_fail_and_run_nothing() {
    let effects = Effects::default();
    let host = Host::new(
        &effects,
        vec![FakeKind::applying(Kind::Linux, "6.8", &effects)],
    );
    let args = HookArgs {
        step: HookStep::Prepare,
        params: PathBuf::from("-"),
        package_lock_wait_secs: DEFAULT_PACKAGE_LOCK_WAIT_SECS,
    };
    let mut stdout = Vec::new();
    let outcome = main_with(&args, &host.ports(), &mut Broken, &mut stdout);
    assert_eq!(outcome, Outcome::Failure);
    let expected = failed(
        "params_invalid",
        "the parameters could not be read: the pipe broke",
    );
    assert_eq!(
        String::from_utf8(stdout).expect("utf-8"),
        format!("{}\n", expected.line)
    );
    assert_eq!(host.effects(), Vec::<String>::new());
}

/// Proves: a run record that cannot record a kind applied fails the cutover
/// `run_record_failed`, with the record's words, at each place a kind is recorded applied: after
/// a cutover that applied it, after the reboot it needed, and for a `reboot` kind after a
/// finished reboot. No later kind runs.
///
/// Catches: a kind run on past a write that did not reach the record, so that the next cutover
/// installs it again.
#[test]
fn a_record_that_cannot_record_a_kind_applied_fails_the_cutover() {
    let expected = failed(
        REASON_RUN_RECORD_FAILED,
        "the run record failed: the disk is full",
    );
    let effects = Effects::default();
    let host = Host::new(
        &effects,
        vec![
            FakeKind::applying(Kind::Macos, "26.2", &effects),
            FakeKind::applying(Kind::Xcode, "27.2", &effects),
        ],
    );
    host.record.allow_writes(0);
    let kinds = [WireKind::Macos, WireKind::Xcode];
    assert_eq!(host.run(HookStep::Cutover, &kinds), expected);
    assert_eq!(host.effects(), ["macos cutover"]);

    let boot = BootId::uuid("test", BOOT_A).expect("uuid");
    host.record
        .set(Kind::Macos, KindState::RebootUnderWay { boot });
    host.boot.set(BOOT_B);
    assert_eq!(host.run(HookStep::Cutover, &kinds), expected);
    assert_eq!(host.effects(), ["macos after_reboot"]);

    let host = Host::new(
        &effects,
        vec![
            FakeKind::rebooting(Kind::Linux, "6.8.0-60", &effects),
            FakeKind::rebooting(Kind::Reboot, BOOT_B, &effects),
        ],
    );
    // The Linux kind's reboot is seen finished and recorded applied (the one write allowed);
    // recording the reboot kind applied then fails.
    host.record.allow_writes(1);
    let boot = BootId::uuid("test", BOOT_A).expect("uuid");
    host.record
        .set(Kind::Linux, KindState::RebootUnderWay { boot });
    host.boot.set(BOOT_B);
    assert_eq!(
        host.run(HookStep::Cutover, &[WireKind::Linux, WireKind::Reboot]),
        expected
    );
    assert_eq!(
        host.effects(),
        ["linux after_reboot", "record run-7 linux applied 6.8.0-60"]
    );
}
