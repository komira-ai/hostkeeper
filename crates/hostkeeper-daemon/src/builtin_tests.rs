//! The built-in steps of a repository version with no hook program: what each step does to the
//! run units, what the clock and the observer decide, and the exits a failure or a deferral gives.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::sync::Arc;

use hostkeeper_observe::fake::{
    FakeExpectedExe, FakeHealthClient, FakeProcessProbe, FakeServiceManager,
};
use hostkeeper_observe::{ActiveState, ObserveOptions};
use hostkeeper_placed::UnitControlError;
use hostkeeper_placed::testing::{Event, FakeUnitControl, Log, UnitCall};
use hostkeeper_proto::hostkeeper::release::v1::{
    Hook, Manifest as EntryManifest, StepBounds, Target,
};
use hostkeeper_steps::{Bounds, Staged};
use hostkeeper_testkit::Scratch;

use super::*;
use crate::testing::FakeClock;

const URL: &str = "http://127.0.0.1:8080/healthz";
const OWN: HostkeeperVersion = HostkeeperVersion {
    major: 0,
    minor: 1,
    patch: 70,
};

/// A scratch directory under the target directory's `tmp` (a unit test has no
/// `CARGO_TARGET_TMPDIR`): the test binary lives in `<target>/<profile>/deps`.
fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

/// An apt entry whose units are `run`, with the health URL `url`.
fn entry(run: &[&str], url: Option<&str>) -> Target {
    Target {
        installer: "apt".to_owned(),
        digest: "ab".repeat(32),
        manifest: Some(EntryManifest {
            package: Some("web".to_owned()),
            hook: Some(Hook {
                timeout_secs: Some(StepBounds {
                    prepare: 60,
                    cutover: 60,
                    verify: 60,
                    rollback: 60,
                    cleanup: 60,
                    uninstall: None,
                }),
                verify_deadline_secs: 300,
                ..Hook::default()
            }),
            run: run.iter().map(|unit| (*unit).to_owned()).collect(),
            health_url: url.map(str::to_owned),
            min_hostkeeper: "0.1.0".to_owned(),
            ..EntryManifest::default()
        }),
        repository: Some("main".to_owned()),
        package_version: Some("1.0.0-1".to_owned()),
        ..Target::default()
    }
}

struct Rig {
    /// Kept so the directory lives as long as the rig.
    _scratch: Scratch,
    packages: PathBuf,
    log: Log,
    units: Arc<FakeUnitControl>,
    clock: Arc<FakeClock>,
    manager: FakeServiceManager,
    health: FakeHealthClient,
    ports: BuiltinPorts,
}

impl Rig {
    fn new(name: &str) -> Rig {
        let scratch = scratch(name);
        let packages = scratch.path().join("packages");
        fs::create_dir(&packages).expect("the packages directory");
        let log = Log::new();
        let units = Arc::new(FakeUnitControl::new(log.clone()));
        let clock = FakeClock::new();
        let (manager, health) = (FakeServiceManager::default(), FakeHealthClient::default());
        let observer = Observer::new(
            hostkeeper_observe::Ports {
                manager: Box::new(manager.clone()),
                process: Box::new(FakeProcessProbe::default()),
                health: Box::new(health.clone()),
                expected: Box::new(FakeExpectedExe::default()),
            },
            ObserveOptions::default(),
        );
        let ports = BuiltinPorts {
            units: units.clone(),
            clock: clock.clone(),
            observer: Arc::new(observer),
            packages: packages.clone(),
            binaries: scratch.path().join("bin"),
            own: OWN,
        };
        Rig {
            _scratch: scratch,
            packages,
            log,
            units,
            clock,
            manager,
            health,
            ports,
        }
    }

    /// The version `version` of web, holding `entry` as `target.json`.
    fn add(&self, version: &str, entry: &Target) -> PathBuf {
        let dir = self.packages.join("web").join(version);
        fs::create_dir_all(&dir).expect("the version directory");
        target::write(&dir, entry).expect("target.json");
        dir
    }

    /// `current` of web names `version`.
    fn current(&self, version: &str) {
        symlink(version, self.packages.join("web/current")).expect("the link");
    }

    /// The update of web to 2.0.0 from 1.0.0 (a first install when `first`), both holding the
    /// entries given.
    fn order(&self, previous: Option<&Target>, target: &Target) -> UpdateOrder {
        let staged = Staged::seal(&self.add("2.0.0", target)).expect("sealed");
        let before = previous.map(|entry| Staged::seal(&self.add("1.0.0", entry)).expect("sealed"));
        UpdateOrder {
            service: "web".to_owned(),
            target_version: "2.0.0".to_owned(),
            previous_version: previous.map(|_| "1.0.0".to_owned()),
            artifact_digest: "ab".repeat(32),
            staged,
            previous: before,
            hook_binary: PathBuf::from("bin/hook"),
            bounds: Bounds {
                prepare_secs: 60,
                cutover_secs: 60,
                verify_secs: 60,
                rollback_secs: 60,
                cleanup_secs: 60,
                verify_deadline_secs: 300,
                promotion_ceiling_secs: u64::MAX,
                cleanup_retries: 3,
            },
            declared_params: hostkeeper_hook::Params::default(),
            min_updater_protocol: None,
            order_try: None,
            builtin_steps: true,
        }
    }

    fn step(&self, step: Step, order: &UpdateOrder, bound: Bound) -> StepExit {
        builtin(self.ports.clone())(step, order, bound)
    }

    /// The calls the service manager got, in order.
    fn calls(&self) -> Vec<UnitCall> {
        self.log
            .events()
            .into_iter()
            .filter_map(|event| match event {
                Event::Unit(call) => Some(call),
                Event::Fs(_) | Event::Mark(_) => None,
            })
            .collect()
    }
}

fn secs(n: u64) -> Bound {
    Bound::Secs(std::num::NonZeroU64::new(n).expect("above 0"))
}

fn stop(unit: &str) -> UnitCall {
    UnitCall::Stop(unit.to_owned())
}

fn start(unit: &str) -> UnitCall {
    UnitCall::Start(unit.to_owned())
}

fn line(exit: &StepExit) -> serde_json::Value {
    serde_json::from_str(exit.last_line.as_deref().expect("a last line")).expect("JSON")
}

/// A service manager whose units start, and which never says whether one is active.
struct Mute;

impl UnitControl for Mute {
    fn daemon_reload(&self) -> Result<(), UnitControlError> {
        Ok(())
    }

    fn stop_unit(&self, _unit: &str) -> Result<(), UnitControlError> {
        Ok(())
    }

    fn start_unit(&self, _unit: &str) -> Result<(), UnitControlError> {
        Ok(())
    }

    fn is_active(&self, unit: &str) -> Result<bool, UnitControlError> {
        Err(UnitControlError {
            action: format!("is-active {unit}"),
            why: "no answer".to_owned(),
        })
    }
}

/// Proves prepare, cleanup and uninstall do nothing: exit 0, no last line, no call to the service
/// manager.
#[test]
fn prepare_cleanup_and_uninstall_do_nothing() {
    let rig = Rig::new("builtin-nothing");
    let order = rig.order(
        Some(&entry(&["web.service"], None)),
        &entry(&["web.service"], None),
    );
    for step in [Step::Prepare, Step::Cleanup, Step::Uninstall] {
        assert_eq!(rig.step(step, &order, secs(60)), BUILTIN_EXIT, "{step:?}");
    }
    assert_eq!(
        rig.step(Step::Prepare, &order, Bound::Unbounded),
        BUILTIN_EXIT
    );
    assert_eq!(rig.calls(), vec![]);
}

/// Proves cutover stops and starts each run unit, in the order of `run`, and returns once each is
/// active. Catches a cutover that only starts, which leaves the old process running.
#[test]
fn cutover_restarts_every_run_unit_and_waits_for_each() {
    let rig = Rig::new("builtin-cutover");
    let order = rig.order(None, &entry(&["a.service", "b.service"], None));
    rig.units.set_active("a.service");
    rig.units.active_after("b.service", 2);
    assert_eq!(rig.step(Step::Cutover, &order, secs(60)), BUILTIN_EXIT);
    let calls = rig.calls();
    assert_eq!(
        calls[..4],
        [
            stop("a.service"),
            stop("b.service"),
            start("a.service"),
            start("b.service")
        ]
    );
    let asks = calls
        .iter()
        .filter(|call| **call == UnitCall::IsActive("b.service".to_owned()))
        .count();
    assert_eq!(asks, 3, "b answers inactive twice, then active");
    assert_eq!(rig.clock.mono_secs(), 1000 + 2 * POLL_SECS);
}

/// Proves a cutover with no bound waits as long as the unit needs.
#[test]
fn an_unbounded_cutover_waits_until_the_unit_is_active() {
    let rig = Rig::new("builtin-unbounded");
    let order = rig.order(None, &entry(&["web.service"], None));
    rig.units.active_after("web.service", 40);
    assert_eq!(
        rig.step(Step::Cutover, &order, Bound::Unbounded),
        BUILTIN_EXIT
    );
    assert_eq!(rig.clock.mono_secs(), 1000 + 40 * POLL_SECS);
}

/// Proves a unit that is not active within the bound fails the cutover with exit 1, class
/// `release`, reason `unit_inactive`, naming the unit and the bound, with the wait ending at the
/// bound on the clock and no retry hint.
#[test]
fn a_unit_not_active_within_the_bound_fails_the_cutover() {
    let rig = Rig::new("builtin-late");
    let order = rig.order(None, &entry(&["web.service"], None));
    rig.units.active_after("web.service", 1_000);
    let exit = rig.step(Step::Cutover, &order, secs(7));
    assert_eq!(exit.code, Some(1));
    assert_eq!(
        line(&exit),
        serde_json::json!({
            "class": "release",
            "reason": "unit_inactive",
            "detail": "not active within the cutover bound (7): web.service",
        })
    );
    assert_eq!(rig.clock.mono_secs(), 1007);
}

/// Proves a bound that is not a multiple of the poll still ends exactly at the bound: the last
/// sleep is the rest of it.
#[test]
fn the_last_wait_is_the_rest_of_the_bound() {
    let rig = Rig::new("builtin-rest");
    let clock = FakeClock::new();
    let error = start_and_wait(
        &*rig.units,
        &*clock,
        &["web.service".to_owned()],
        POLL_SECS + 1,
        "the test bound",
    );
    assert_eq!(error, Ok(()));
    rig.units.active_after("slow.service", 100);
    let error = start_and_wait(
        &*rig.units,
        &*clock,
        &["slow.service".to_owned()],
        POLL_SECS * 2 + 1,
        "the test bound",
    );
    assert_eq!(
        error,
        Err(format!(
            "not active within the test bound ({}): slow.service",
            POLL_SECS * 2 + 1
        ))
    );
    assert_eq!(clock.mono_secs(), 1000 + POLL_SECS * 2 + 1);
}

/// Proves only the units still not active are named, and one seen active stays done.
#[test]
fn only_the_units_still_inactive_are_named() {
    let rig = Rig::new("builtin-named");
    rig.units.active_after("b.service", 1_000);
    let result = start_and_wait(
        &*rig.units,
        &*rig.clock,
        &["a.service".to_owned(), "b.service".to_owned()],
        3,
        "the cutover bound",
    );
    assert_eq!(
        result,
        Err("not active within the cutover bound (3): b.service".to_owned())
    );
}

/// Proves no run units is a wait that ends at once.
#[test]
fn nothing_to_start_is_done_at_once() {
    let rig = Rig::new("builtin-empty");
    assert_eq!(
        start_and_wait(&*rig.units, &*rig.clock, &[], 5, "a bound"),
        Ok(())
    );
    assert_eq!(rig.calls(), vec![]);
    assert_eq!(rig.clock.mono_secs(), 1000);
}

/// Proves an `is-active` that cannot be asked counts as not active, so the wait ends at the bound
/// with the unit named.
#[test]
fn an_unanswered_question_counts_as_inactive() {
    let clock = FakeClock::new();
    let result = start_and_wait(&Mute, &*clock, &["web.service".to_owned()], 2, "the bound");
    assert_eq!(
        result,
        Err("not active within the bound (2): web.service".to_owned())
    );
}

/// Proves a unit that fails to stop fails the cutover with the service manager's words, and no
/// unit is started after it.
#[test]
fn a_unit_that_does_not_stop_fails_the_cutover() {
    let rig = Rig::new("builtin-stop-fails");
    let order = rig.order(None, &entry(&["a.service", "b.service"], None));
    rig.units.fail_stop("b.service");
    let exit = rig.step(Step::Cutover, &order, secs(60));
    assert_eq!(exit.code, Some(1));
    let seen = line(&exit);
    assert_eq!(seen["reason"], "unit_inactive");
    assert!(
        seen["detail"].as_str().expect("text").contains("b.service"),
        "{seen}"
    );
    assert!(
        rig.calls()
            .iter()
            .all(|call| !matches!(call, UnitCall::Start(_)))
    );
}

/// Proves a unit that fails to start fails the cutover with exit 1 `unit_inactive`.
#[test]
fn a_unit_that_does_not_start_fails_the_cutover() {
    let rig = Rig::new("builtin-start-fails");
    let order = rig.order(None, &entry(&["web.service"], None));
    rig.units.fail_start("web.service");
    let exit = rig.step(Step::Cutover, &order, secs(60));
    assert_eq!(exit.code, Some(1));
    assert_eq!(line(&exit)["reason"], "unit_inactive");
    assert_eq!(line(&exit)["class"], "release");
}

/// Proves rollback restarts the units of the previous release (from its sealed `target.json`),
/// not the target's.
#[test]
fn rollback_restarts_the_units_of_the_previous_release() {
    let rig = Rig::new("builtin-rollback");
    let order = rig.order(
        Some(&entry(&["old.service"], None)),
        &entry(&["new.service"], None),
    );
    assert_eq!(rig.step(Step::Rollback, &order, secs(60)), BUILTIN_EXIT);
    let calls = rig.calls();
    assert_eq!(calls[..2], [stop("old.service"), start("old.service")]);
    assert!(calls.iter().all(|call| match call {
        UnitCall::Stop(unit) | UnitCall::Start(unit) | UnitCall::IsActive(unit) =>
            unit == "old.service",
        _ => true,
    }));
}

/// Proves a rollback that cannot bring the previous units back fails like a cutover.
#[test]
fn a_rollback_whose_unit_stays_down_fails() {
    let rig = Rig::new("builtin-rollback-fails");
    let order = rig.order(
        Some(&entry(&["old.service"], None)),
        &entry(&["new.service"], None),
    );
    rig.units.fail_start("old.service");
    let exit = rig.step(Step::Rollback, &order, secs(60));
    assert_eq!(exit.code, Some(1));
    assert_eq!(line(&exit)["reason"], "unit_inactive");
}

/// Proves the rollback of a first install, whose package the switch removed, starts nothing.
#[test]
fn the_rollback_of_a_first_install_does_nothing() {
    let rig = Rig::new("builtin-first");
    let order = rig.order(None, &entry(&["web.service"], None));
    assert_eq!(rig.step(Step::Rollback, &order, secs(60)), BUILTIN_EXIT);
    assert_eq!(rig.calls(), vec![]);
}

/// Proves a rollback whose previous version is not named is read from the target's own directory
/// (the order names a previous release but not its version).
#[test]
fn a_rollback_with_no_previous_version_reads_the_targets_directory() {
    let rig = Rig::new("builtin-nameless");
    let mut order = rig.order(
        Some(&entry(&["old.service"], None)),
        &entry(&["new.service"], None),
    );
    order.previous_version = None;
    assert_eq!(rig.step(Step::Rollback, &order, secs(60)), BUILTIN_EXIT);
    assert_eq!(
        rig.calls()[..2],
        [stop("new.service"), start("new.service")]
    );
}

/// Proves a version without `target.json` (the daemon's own service) gets the no-op exit for every
/// step and touches no unit.
#[test]
fn a_version_without_target_json_runs_the_noop_exit_for_every_step() {
    let rig = Rig::new("builtin-archive");
    let mut order = rig.order(None, &entry(&["web.service"], None));
    fs::remove_file(order.staged.dir().join(target::TARGET_FILE)).expect("remove");
    order.previous_version = Some("1.0.0".to_owned());
    for step in [
        Step::Prepare,
        Step::Cutover,
        Step::Verify,
        Step::Rollback,
        Step::Cleanup,
        Step::Uninstall,
    ] {
        assert_eq!(rig.step(step, &order, secs(60)), BUILTIN_EXIT, "{step:?}");
    }
    assert_eq!(rig.calls(), vec![]);
}

/// Proves an unreadable `target.json` fails every step with exit 1, `manifest_invalid`, the
/// reader's words as the detail.
#[test]
fn an_unreadable_target_json_fails_with_manifest_invalid() {
    let rig = Rig::new("builtin-garbage");
    let order = rig.order(None, &entry(&["web.service"], None));
    let file = order.staged.dir().join(target::TARGET_FILE);
    fs::set_permissions(&file, std::os::unix::fs::PermissionsExt::from_mode(0o644)).expect("mode");
    fs::write(&file, b"not json").expect("write");
    for step in [Step::Cutover, Step::Verify, Step::Prepare] {
        let exit = rig.step(step, &order, secs(60));
        assert_eq!(exit.code, Some(1), "{step:?}");
        let seen = line(&exit);
        assert_eq!(seen["class"], "release");
        assert_eq!(seen["reason"], "manifest_invalid");
        assert!(
            seen["detail"]
                .as_str()
                .expect("text")
                .contains("target.json"),
            "{seen}"
        );
    }
}

/// Proves a service or version that is no name fails with `manifest_invalid` naming the rule.
#[test]
fn a_service_or_version_that_is_no_name_fails_with_manifest_invalid() {
    let rig = Rig::new("builtin-names");
    let mut order = rig.order(None, &entry(&["web.service"], None));
    order.service = "Not A Name".to_owned();
    let exit = rig.step(Step::Cutover, &order, secs(60));
    assert_eq!(exit.code, Some(1));
    assert_eq!(line(&exit)["reason"], "manifest_invalid");
    order.service = "web".to_owned();
    order.target_version = "no/version".to_owned();
    let exit = rig.step(Step::Cutover, &order, secs(60));
    assert_eq!(exit.code, Some(1));
    assert_eq!(line(&exit)["reason"], "manifest_invalid");
}

/// Proves an entry that needs a newer daemon fails the step with `manifest_invalid`.
#[test]
fn an_entry_that_needs_a_newer_daemon_fails_with_manifest_invalid() {
    let rig = Rig::new("builtin-too-old");
    let mut target = entry(&["web.service"], None);
    target.manifest.as_mut().expect("a manifest").min_hostkeeper = "9.0.0".to_owned();
    let order = rig.order(None, &target);
    let exit = rig.step(Step::Cutover, &order, secs(60));
    assert_eq!(exit.code, Some(1));
    assert_eq!(line(&exit)["reason"], "manifest_invalid");
}

/// Proves verify passes when every run unit is active and the health URL answers 2xx.
#[test]
fn verify_passes_when_every_unit_is_active_and_healthy() {
    let rig = Rig::new("builtin-verify-ok");
    let order = rig.order(None, &entry(&["web.service"], Some(URL)));
    rig.current("2.0.0");
    rig.units.set_active("web.service");
    rig.manager.set("web.service", ActiveState::Active, None);
    rig.health.answer(URL, 200);
    assert_eq!(rig.step(Step::Verify, &order, secs(60)), BUILTIN_EXIT);
    assert_eq!(rig.health.requests(), vec![URL.to_owned()]);
}

/// Proves verify defers (exit 75, class `environment`, reason `not_serving`, retry in 5 seconds)
/// naming the units that are not active, and does not ask the health URL.
#[test]
fn verify_defers_naming_the_inactive_units() {
    let rig = Rig::new("builtin-verify-down");
    let order = rig.order(None, &entry(&["a.service", "b.service"], Some(URL)));
    rig.current("2.0.0");
    rig.units.set_active("a.service");
    let exit = rig.step(Step::Verify, &order, secs(60));
    assert_eq!(exit.code, Some(75));
    assert_eq!(
        line(&exit),
        serde_json::json!({
            "class": "environment",
            "reason": "not_serving",
            "detail": "not active: b.service",
            "retry_after_secs": VERIFY_RETRY_SECS,
        })
    );
    assert_eq!(rig.health.requests(), Vec::<String>::new());
}

/// Proves verify defers when the health URL answers outside 2xx, with the observer's reason.
#[test]
fn verify_defers_when_the_health_url_does_not_answer_2xx() {
    let rig = Rig::new("builtin-verify-unhealthy");
    let order = rig.order(None, &entry(&["web.service"], Some(URL)));
    rig.current("2.0.0");
    rig.units.set_active("web.service");
    rig.manager.set("web.service", ActiveState::Active, None);
    rig.health.answer(URL, 503);
    let exit = rig.step(Step::Verify, &order, secs(60));
    assert_eq!(exit.code, Some(75));
    let seen = line(&exit);
    assert_eq!(seen["reason"], "not_serving");
    assert_eq!(seen["detail"], "the health URL is status_non_2xx");
    assert_eq!(seen["retry_after_secs"], 5);
}

/// Proves a service with no health URL passes verify on its units alone, and one the observer has
/// no input for (no `current` link) is not held back by it.
#[test]
fn verify_without_a_health_url_or_an_observation_passes_on_the_units() {
    let rig = Rig::new("builtin-verify-quiet");
    let order = rig.order(None, &entry(&["web.service"], None));
    rig.units.set_active("web.service");
    assert_eq!(
        rig.step(Step::Verify, &order, secs(60)),
        BUILTIN_EXIT,
        "no current link"
    );
    rig.current("2.0.0");
    rig.manager.set("web.service", ActiveState::Active, None);
    assert_eq!(rig.step(Step::Verify, &order, secs(60)), BUILTIN_EXIT);
    assert_eq!(rig.health.requests(), Vec::<String>::new());
}

/// Proves a unit whose state cannot be asked is not active for verify.
#[test]
fn a_unit_whose_state_cannot_be_asked_defers_verify() {
    let rig = Rig::new("builtin-verify-mute");
    let mut ports = rig.ports.clone();
    ports.units = Arc::new(Mute);
    let exit = verify(
        &ports,
        &ServiceName::new("web").expect("a name"),
        &["web.service".to_owned()],
    );
    assert_eq!(exit.code, Some(75));
    assert_eq!(line(&exit)["detail"], "not active: web.service");
}
