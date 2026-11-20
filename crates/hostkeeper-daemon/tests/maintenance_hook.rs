//! The maintenance hook's steps over fakes (docs/design/maintenance-updates.md, section 5.2): the
//! order of the kinds, the reboot recorded before it is asked for and the cutover after it, the
//! outcomes and their report lines, and the parameters kept out of the output. A reboot is the test
//! changing the boot id and running the same `cutover` again.

mod support_maintenance;

use std::path::PathBuf;

use hostkeeper_daemon::maintenance_hook::{
    HookArgs, HookStep, Kind, KindError, KindState, MAX_DETAIL, MaintainParams, REASON_KIND_FAILED,
    REASON_NOT_APPLIED, REASON_UNSUPPORTED_KIND, Registry, main_with,
};
use hostkeeper_hook::{Class, Outcome};
use hostkeeper_proto::hostkeeper::agent::v1::{Maintain, MaintenanceKind as WireKind, XcodeEntry};
use hostkeeper_steps::BootId;
use support_maintenance::{
    BOOT_A, BOOT_B, Effects, FakeKind, Host, RUN, done, ending, failed, maintain, rebooting,
};

/// Proves: a cutover of `[xcode, macos]` applies macOS first, then Xcode, each recorded applied
/// with its version right after its own cutover, and ends done.
///
/// Catches: the kinds applied in the order the order lists them, so a new Xcode is installed on
/// the macOS it may not run on.
#[test]
fn cutover_applies_macos_before_xcode_whatever_order_the_params_give() {
    let effects = Effects::default();
    let host = Host::new(
        &effects,
        vec![
            FakeKind::applying(Kind::Macos, "26.2", &effects),
            FakeKind::applying(Kind::Xcode, "27.2", &effects),
        ],
    );
    let ended = host.run(HookStep::Cutover, &[WireKind::Xcode, WireKind::Macos]);
    assert_eq!(ended, done());
    assert_eq!(
        host.effects(),
        [
            "macos cutover",
            "record run-7 macos applied 26.2",
            "xcode cutover",
            "record run-7 xcode applied 27.2",
        ]
    );
}

/// Proves: a kind that needs a reboot is recorded with its reboot under way and this boot's id
/// before the reboot is asked for; the step ends there, done with `reboot_under_way`, and the
/// next kind is not touched.
///
/// Catches: the reboot asked for before the record is durable, so the boot after it cannot tell
/// the kind was installed and installs it again; or the next kind applied before the reboot.
#[test]
fn a_kind_that_needs_a_reboot_is_recorded_before_the_reboot_is_asked_for() {
    let effects = Effects::default();
    let host = Host::new(
        &effects,
        vec![
            FakeKind::rebooting(Kind::Macos, "26.2", &effects),
            FakeKind::applying(Kind::Xcode, "27.2", &effects),
        ],
    );
    let ended = host.run(HookStep::Cutover, &[WireKind::Macos, WireKind::Xcode]);
    assert_eq!(ended, rebooting("macos"));
    assert_eq!(
        host.effects(),
        [
            "macos cutover".to_owned(),
            format!("record run-7 macos reboot_under_way {BOOT_A}"),
            "reboot".to_owned(),
        ]
    );
}

/// Proves: after the reboot (a new boot id), the same cutover asks the rebooted kind for its new
/// version, records it applied, goes on with the next kind, and never runs the first kind's
/// cutover again; a third cutover then finds both applied and does nothing.
///
/// Catches: a kind installed twice across its reboot, a run that stops after the reboot, or a
/// run that repeats a kind already applied.
#[test]
fn after_the_reboot_the_same_cutover_goes_on_with_the_next_kind_and_installs_nothing_again() {
    let effects = Effects::default();
    let host = Host::new(
        &effects,
        vec![
            FakeKind::rebooting(Kind::Macos, "26.2", &effects),
            FakeKind::applying(Kind::Xcode, "27.2", &effects),
        ],
    );
    let kinds = [WireKind::Macos, WireKind::Xcode];
    assert_eq!(host.run(HookStep::Cutover, &kinds), rebooting("macos"));
    host.effects();
    host.boot.set(BOOT_B);
    assert_eq!(host.run(HookStep::Cutover, &kinds), done());
    assert_eq!(
        host.effects(),
        [
            "macos after_reboot",
            "record run-7 macos applied 26.2",
            "xcode cutover",
            "record run-7 xcode applied 27.2",
        ]
    );
    assert_eq!(host.run(HookStep::Cutover, &kinds), done());
    assert_eq!(host.effects(), Vec::<String>::new());
}

/// Proves: a cutover run again in the boot its reboot was asked in (the reboot has not happened)
/// asks for the reboot again and installs nothing, records nothing.
///
/// Catches: a kind installed a second time because its reboot was slow, or the kind recorded
/// applied without the reboot it needs.
#[test]
fn a_cutover_in_the_same_boot_asks_for_the_reboot_again_and_installs_nothing() {
    let effects = Effects::default();
    let host = Host::new(
        &effects,
        vec![FakeKind::rebooting(Kind::Linux, "6.8.0-60", &effects)],
    );
    assert_eq!(
        host.run(HookStep::Cutover, &[WireKind::Linux]),
        rebooting("linux")
    );
    host.effects();
    assert_eq!(
        host.run(HookStep::Cutover, &[WireKind::Linux]),
        rebooting("linux")
    );
    assert_eq!(host.effects(), ["reboot"]);
}

/// Proves: a `reboot`-only run calls no installer: of a registry holding every kind, only the
/// `reboot` kind is asked, its reboot recorded then asked for; after the reboot it is recorded
/// applied and nothing else runs.
///
/// Catches: a reboot-only run that installs pending updates of another kind.
#[test]
fn a_reboot_only_run_calls_no_installer_and_reboots() {
    let effects = Effects::default();
    let host = Host::new(
        &effects,
        vec![
            FakeKind::applying(Kind::Macos, "26.2", &effects),
            FakeKind::applying(Kind::Xcode, "27.2", &effects),
            FakeKind::applying(Kind::Linux, "6.8.0-60", &effects),
            FakeKind::rebooting(Kind::Reboot, BOOT_B, &effects),
        ],
    );
    assert_eq!(host.run(HookStep::Prepare, &[WireKind::Reboot]), done());
    assert_eq!(host.effects(), ["reboot prepare"]);
    assert_eq!(
        host.run(HookStep::Cutover, &[WireKind::Reboot]),
        rebooting("reboot")
    );
    assert_eq!(
        host.effects(),
        [
            "reboot cutover".to_owned(),
            format!("record run-7 reboot reboot_under_way {BOOT_A}"),
            "reboot".to_owned(),
        ]
    );
    host.boot.set(BOOT_B);
    assert_eq!(host.run(HookStep::Cutover, &[WireKind::Reboot]), done());
    assert_eq!(
        host.effects(),
        [
            "reboot after_reboot".to_owned(),
            format!("record run-7 reboot applied {BOOT_B}"),
        ]
    );
}

/// Proves: in a run of `linux` and `reboot`, once the Linux kind's own reboot is seen finished,
/// the `reboot` kind is recorded applied (under the new boot id) without its cutover and without
/// a second reboot.
///
/// Catches: a host rebooted twice in one turn for one needed reboot.
#[test]
fn a_reboot_kind_after_a_finished_reboot_of_the_run_is_applied_without_a_second_reboot() {
    let effects = Effects::default();
    let host = Host::new(
        &effects,
        vec![
            FakeKind::rebooting(Kind::Linux, "6.8.0-60", &effects),
            FakeKind::rebooting(Kind::Reboot, BOOT_B, &effects),
        ],
    );
    let kinds = [WireKind::Reboot, WireKind::Linux];
    assert_eq!(host.run(HookStep::Cutover, &kinds), rebooting("linux"));
    host.effects();
    host.boot.set(BOOT_B);
    assert_eq!(host.run(HookStep::Cutover, &kinds), done());
    assert_eq!(
        host.effects(),
        [
            "linux after_reboot".to_owned(),
            "record run-7 linux applied 6.8.0-60".to_owned(),
            format!("record run-7 reboot applied {BOOT_B}"),
        ]
    );
}

/// Proves: with no reboot of the run seen finished, a `reboot` kind after an applied kind still
/// runs its own cutover.
///
/// Catches: the reboot kind passed over because an earlier kind of the run was applied without
/// a reboot.
#[test]
fn a_reboot_kind_after_a_kind_applied_without_a_reboot_still_reboots() {
    let effects = Effects::default();
    let host = Host::new(
        &effects,
        vec![
            FakeKind::applying(Kind::Linux, "6.8.0-60", &effects),
            FakeKind::rebooting(Kind::Reboot, BOOT_B, &effects),
        ],
    );
    let kinds = [WireKind::Linux, WireKind::Reboot];
    assert_eq!(host.run(HookStep::Cutover, &kinds), rebooting("reboot"));
    assert_eq!(
        host.effects(),
        [
            "linux cutover".to_owned(),
            "record run-7 linux applied 6.8.0-60".to_owned(),
            "reboot cutover".to_owned(),
            format!("record run-7 reboot reboot_under_way {BOOT_A}"),
            "reboot".to_owned(),
        ]
    );
}

/// The parameters of `kinds` as the JSON the hook reads.
fn params_json(kinds: &[&str]) -> String {
    let kinds: Vec<String> = kinds.iter().map(|kind| format!("{kind:?}")).collect();
    format!(
        r#"{{"runId":"{RUN}","action":"MAINTAIN_ACTION_RUN","kinds":[{}]}}"#,
        kinds.join(",")
    )
}

/// Runs `step` through [`main_with`] with `json` on standard input: its outcome and everything
/// it wrote to standard output.
fn main_on_stdin(host: &Host, step: HookStep, json: &str) -> (Outcome, String) {
    let args = HookArgs {
        step,
        params: PathBuf::from("-"),
    };
    let mut stdout = Vec::new();
    let outcome = main_with(&args, &host.ports(), &mut json.as_bytes(), &mut stdout);
    (outcome, String::from_utf8(stdout).expect("utf-8"))
}

/// Proves: a `prepare` whose first kind defers exits 75 with the deferral's reason and detail, of
/// class `environment`, as the whole of its standard output; it runs no later kind's prepare,
/// writes nothing to the run record and asks for no reboot.
///
/// Catches: a deferral read as a failure (which would halt a host that was never touched), or a
/// prepare that goes on to stage the next kind or touches the record.
#[test]
fn prepare_returning_deferred_exits_75_and_writes_nothing_else() {
    let effects = Effects::default();
    let mut macos = FakeKind::applying(Kind::Macos, "26.2", &effects);
    macos.prepare = Err(KindError::Deferred {
        reason: "disk_short",
        detail: "the data volume has 12 GB free of the 30 GB needed".to_owned(),
    });
    let host = Host::new(
        &effects,
        vec![macos, FakeKind::applying(Kind::Xcode, "27.2", &effects)],
    );
    let (outcome, stdout) = main_on_stdin(
        &host,
        HookStep::Prepare,
        &params_json(&["MAINTENANCE_KIND_XCODE", "MAINTENANCE_KIND_MACOS"]),
    );
    assert_eq!(outcome, Outcome::Deferred);
    assert_eq!(outcome.code(), 75);
    let expected = ending(
        Outcome::Deferred,
        Class::Environment,
        Some("disk_short"),
        Some("the data volume has 12 GB free of the 30 GB needed"),
    );
    assert_eq!(stdout, format!("{}\n", expected.line));
    assert_eq!(host.effects(), ["macos prepare"]);
    assert!(host.record.states.lock().expect("states").is_empty());
}

/// Proves: a `prepare` whose kinds all succeed runs each in order and exits 0 with a report
/// carrying no reason.
///
/// Catches: a prepare that stops after its first kind.
#[test]
fn prepare_runs_every_kind_and_exits_0() {
    let effects = Effects::default();
    let host = Host::new(
        &effects,
        vec![
            FakeKind::applying(Kind::Macos, "26.2", &effects),
            FakeKind::applying(Kind::Xcode, "27.2", &effects),
        ],
    );
    let (outcome, stdout) = main_on_stdin(
        &host,
        HookStep::Prepare,
        &params_json(&["MAINTENANCE_KIND_XCODE", "MAINTENANCE_KIND_MACOS"]),
    );
    assert_eq!(outcome.code(), 0);
    assert_eq!(stdout, format!("{}\n", done().line));
    assert_eq!(host.effects(), ["macos prepare", "xcode prepare"]);
}

/// Proves: a failing cutover exits 1 with the kind's reason and its detail cut to 256 bytes at a
/// character's boundary (a two-byte character would straddle byte 256), and records nothing.
///
/// Catches: an unbounded detail that a run record or heartbeat then refuses whole, a cut inside a
/// character, or a failure recorded as applied.
#[test]
fn a_failing_cutover_exits_non_zero_with_a_bounded_reason() {
    let effects = Effects::default();
    let mut linux = FakeKind::applying(Kind::Linux, "6.8.0-60", &effects);
    // One byte, then two-byte characters: byte 256 is inside one.
    let long = format!("x{}", "é".repeat(600));
    linux.cutover = Err(KindError::Failed {
        reason: "apt_failed",
        detail: long,
    });
    let host = Host::new(&effects, vec![linux]);
    let ended = host.run(HookStep::Cutover, &[WireKind::Linux]);
    let cut = format!("x{}", "é".repeat(127));
    assert_eq!(cut.len(), MAX_DETAIL - 1);
    assert_eq!(ended, failed("apt_failed", &cut));
    assert_eq!(ended.outcome.code(), 1);
    assert_eq!(host.effects(), ["linux cutover"]);
}

/// Proves: a detail of exactly 256 bytes is kept whole, and a kind's reason that is not a token
/// is reported as `kind_failed`.
///
/// Catches: a bound off by one, or a reason the daemon's strict reader would refuse.
#[test]
fn a_detail_at_the_bound_is_kept_and_a_reason_that_is_no_token_is_kind_failed() {
    let effects = Effects::default();
    let mut linux = FakeKind::applying(Kind::Linux, "6.8.0-60", &effects);
    let whole = "d".repeat(MAX_DETAIL);
    linux.verify = Err(KindError::Failed {
        reason: "Not A Token",
        detail: whole.clone(),
    });
    let host = Host::new(&effects, vec![linux]);
    host.record.set(Kind::Linux, KindState::Applied);
    let ended = host.run(HookStep::Verify, &[WireKind::Linux]);
    assert_eq!(ended, failed(REASON_KIND_FAILED, &whole));
}

/// Proves: no parameter's value reaches standard output, whichever way a step ends: a done
/// prepare, a kind this host does not support, parameters that are not a `Maintain` (the
/// decoder's message, which quotes the value, is dropped), and parameters that break a bound.
/// Each output is checked whole.
///
/// Catches: the run id, an Xcode's digest or a version to remove echoed into a log line, a run
/// record or an alert.
#[test]
fn the_parameters_never_appear_in_the_output() {
    let effects = Effects::default();
    let host = Host::new(
        &effects,
        vec![FakeKind::applying(Kind::Macos, "26.2", &effects)],
    );
    let full = Maintain {
        xcodes: vec![XcodeEntry {
            version: "27.2".to_owned(),
            build: "27B52".to_owned(),
            size: 3_000_000_000,
            sha256: "ab".repeat(32),
        }],
        xcode_remove: vec!["25.9.9".to_owned()],
        xcode_default: "27.2".to_owned(),
        ..maintain(&[WireKind::Macos, WireKind::Xcode])
    };
    let json = r#"{"runId":"run-7","action":"MAINTAIN_ACTION_RUN","kinds":["MAINTENANCE_KIND_MACOS","MAINTENANCE_KIND_XCODE"],"xcodes":[{"version":"27.2","build":"27B52","size":"3000000000","sha256":"SHA"}],"xcodeRemove":["25.9.9"],"xcodeDefault":"27.2"}"#
        .replace("SHA", &"ab".repeat(32));
    assert_eq!(
        MaintainParams::read(&mut json.as_bytes()),
        MaintainParams::new(full)
    );
    let unsupported = failed(
        REASON_UNSUPPORTED_KIND,
        "xcode is not supported on this host",
    );
    let not_maintain = failed(
        "params_invalid",
        "the parameters are not the JSON of a hostkeeper.agent.v1.Maintain",
    );
    let long_run = json.replace("run-7", &"r".repeat(65));
    let cases = [
        (HookStep::Prepare, json.clone(), unsupported.clone()),
        (HookStep::Cutover, json.clone(), unsupported),
        (
            HookStep::Prepare,
            json.replace(r#""27B52""#, "27352"),
            not_maintain.clone(),
        ),
        (
            HookStep::Prepare,
            r#"{"runId":"run-7","kinds":["MAINTENANCE_KIND_MACOS"],"xcodeDefault":{"v":"25.9.9"}}"#
                .to_owned(),
            not_maintain,
        ),
        (
            HookStep::Verify,
            long_run,
            failed("params_invalid", "run_id is not 1 to 64 bytes"),
        ),
    ];
    for (step, json, expected) in cases {
        let (outcome, stdout) = main_on_stdin(&host, step, &json);
        assert_eq!(outcome, expected.outcome, "{json}");
        assert_eq!(stdout, format!("{}\n", expected.line), "{json}");
        for value in ["run-7", "27B52", "27352", "25.9.9", "abab", "rrrr"] {
            assert!(!stdout.contains(value), "{value} in {stdout}");
        }
    }
}

/// Proves: `verify` fails `not_applied`, naming the kind, for a kind whose reboot is under way or
/// that never ran, without running its check; with every kind applied it runs each kind's
/// `verify` in order and ends done; a kind's failed check is the step's failure.
///
/// Catches: a verify that passes a host whose update never took.
#[test]
fn verify_checks_each_kind_once_the_record_holds_it_applied() {
    let effects = Effects::default();
    let mut xcode = FakeKind::applying(Kind::Xcode, "27.2", &effects);
    let host = Host::new(
        &effects,
        vec![
            FakeKind::applying(Kind::Macos, "26.2", &effects),
            xcode.clone(),
        ],
    );
    let kinds = [WireKind::Macos, WireKind::Xcode];
    let boot = BootId::uuid("test", BOOT_A).expect("uuid");
    host.record
        .set(Kind::Macos, KindState::RebootUnderWay { boot });
    assert_eq!(
        host.run(HookStep::Verify, &kinds),
        failed(REASON_NOT_APPLIED, "macos is not recorded applied")
    );
    assert_eq!(host.effects(), Vec::<String>::new());
    host.record.set(Kind::Macos, KindState::Applied);
    assert_eq!(
        host.run(HookStep::Verify, &kinds),
        failed(REASON_NOT_APPLIED, "xcode is not recorded applied")
    );
    assert_eq!(host.effects(), ["macos verify"]);
    host.record.set(Kind::Xcode, KindState::Applied);
    assert_eq!(host.run(HookStep::Verify, &kinds), done());
    assert_eq!(host.effects(), ["macos verify", "xcode verify"]);
    xcode.verify = Err(KindError::Failed {
        reason: "wrong_build",
        detail: "/Applications/Xcode-27.2.app is build 27B51".to_owned(),
    });
    let host = Host {
        kinds: Registry::new()
            .with(
                Kind::Macos,
                Box::new(FakeKind::applying(Kind::Macos, "26.2", &effects)),
            )
            .with(Kind::Xcode, Box::new(xcode)),
        ..host
    };
    assert_eq!(
        host.run(HookStep::Verify, &kinds),
        failed("wrong_build", "/Applications/Xcode-27.2.app is build 27B51")
    );
}
