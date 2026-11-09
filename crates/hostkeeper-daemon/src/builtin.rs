//! The daemon's built-in steps (packages-lifecycle.md section 6.3): the steps of a service whose
//! version has no hook program, shipped code like an installer, which the step runner runs in
//! place of a hook ([`hostkeeper_steps::StepRunner::with_builtin`]).
//!
//! | Step | Does |
//! |---|---|
//! | `prepare`, `cleanup`, `uninstall` | Nothing |
//! | `cutover` | Restarts every `run` unit and waits, within the step's bound, for each to be active; one that is not is `Failed`, `release`, [`REASON_UNIT_INACTIVE`] |
//! | `verify` | Passes when every `run` unit is active and the health URL, if any, answers 2xx; otherwise defers ([`VERIFY_RETRY_SECS`]), until the runner's verify window is over |
//! | `rollback` | Restarts the `run` units of the release `current` was switched back to; for a first install, whose package the switch removed, nothing |
//!
//! They apply to a repository version (one with `target.json`, [`crate::target`]) whose package
//! carries no hook; the `rollback` of a move from `hostkeeper-archive` restarts the units of the
//! archive version it went back to. The daemon's own service runs [`BUILTIN_EXIT`], every step
//! exiting 0, whatever its version: its unit runs the daemon, and its new binary switches once it
//! has started ([`crate::own_package`]); so does a version of another service without
//! `target.json`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_config::manifest::{Expected, HostkeeperVersion, Manifest};
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_hook::Step;
use hostkeeper_observe::{HealthView, Observer};
use hostkeeper_placed::UnitControl;
use hostkeeper_steps::{BUILTIN_EXIT, Bound, Builtin, Clock, StepExit, UpdateOrder};

use crate::target::{self, TARGET_FILE};
use crate::verbs::force::SELF_SERVICE;

/// The reason of a `cutover` whose `run` unit did not become active within the step's bound.
pub const REASON_UNIT_INACTIVE: &str = "unit_inactive";
/// The reason of a `verify` that defers: a `run` unit is not active, or the health URL did not
/// answer 2xx.
pub const REASON_NOT_SERVING: &str = "not_serving";
/// The seconds a deferring `verify` asks to be run again after.
pub const VERIFY_RETRY_SECS: u64 = 5;
/// How often a unit is asked whether it is active, in seconds of the daemon's clock: a force's
/// look ([`crate::verbs::force::FORCE_POLL_SECS`]).
pub const POLL_SECS: u64 = crate::verbs::force::FORCE_POLL_SECS;

/// What the built-in steps look at the host through.
#[derive(Clone)]
pub struct BuiltinPorts {
    pub units: Arc<dyn UnitControl + Send + Sync>,
    pub clock: Arc<dyn Clock>,
    pub observer: Arc<Observer>,
    /// The packages directory, as the observer reads it.
    pub packages: PathBuf,
    /// The daemon's binary directory, as the observer reads it.
    pub binaries: PathBuf,
    pub own: HostkeeperVersion,
}

/// The built-in steps over `ports`, for the step runner.
pub fn builtin(ports: BuiltinPorts) -> Builtin {
    Arc::new(move |step: Step, order: &UpdateOrder, bound: Bound| run(&ports, step, order, bound))
}

/// Starts each of `run`, then asks after each until it has been seen active once, every
/// [`POLL_SECS`] on `clock`, within `bound_secs` of the start. An answer that is an error
/// counts as not active. `Err` names what failed to start, or what was not active in time.
pub fn start_and_wait(
    units: &dyn UnitControl,
    clock: &dyn Clock,
    run: &[String],
    bound_secs: u64,
    bound_name: &str,
) -> Result<(), String> {
    for unit in run {
        units.start_unit(unit).map_err(|error| error.to_string())?;
    }
    // The wait is of this process, so of this boot: the monotonic clock alone measures it, and an
    // unreadable boot id never makes it endless.
    let began = clock.mono_secs();
    let mut waiting: Vec<&String> = run.iter().collect();
    loop {
        waiting.retain(|unit| !units.is_active(unit).unwrap_or(false));
        if waiting.is_empty() {
            return Ok(());
        }
        let spent = clock.mono_secs().saturating_sub(began);
        if spent >= bound_secs {
            let names: Vec<&str> = waiting.iter().map(|unit| unit.as_str()).collect();
            return Err(format!(
                "not active within {bound_name} ({bound_secs}): {}",
                names.join(", ")
            ));
        }
        clock.sleep_secs(POLL_SECS.min(bound_secs - spent));
    }
}

/// The exit of a step that failed or deferred for `reason`, with `detail`.
fn exit(code: i32, class: &str, reason: &str, detail: &str, retry: Option<u64>) -> StepExit {
    let mut line = serde_json::json!({"class": class, "reason": reason, "detail": detail});
    if let Some(secs) = retry {
        line["retry_after_secs"] = serde_json::json!(secs);
    }
    StepExit {
        code: Some(code),
        last_line: Some(line.to_string()),
    }
}

/// Whether the version directory `dir` holds `target.json`: a repository version.
fn has_target(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir.join(TARGET_FILE)).is_ok()
}

/// The manifest of the version in `dir` at `version` of `service`: of a repository version, or,
/// when `archive` is set, of an archive version too; `None` for a version without `target.json`
/// otherwise, `Err` for one whose manifest does not read.
fn repository_manifest(
    ports: &BuiltinPorts,
    service: &str,
    version: &str,
    dir: &Path,
    archive: bool,
) -> Option<Result<Manifest, String>> {
    if !(archive | has_target(dir)) {
        return None;
    }
    let expected = || -> Result<Expected, String> {
        Ok(Expected {
            name: ServiceName::new(service).map_err(|e| e.to_string())?,
            version: Version::new(version).map_err(|e| e.to_string())?,
            platform: hostkeeper_config::platform::Platform::current(),
        })
    };
    Some(expected().and_then(|expected| {
        target::manifest(dir, &expected, &ports.own).map_err(|e| e.to_string())
    }))
}

/// One built-in step of `order`.
fn run(ports: &BuiltinPorts, step: Step, order: &UpdateOrder, bound: Bound) -> StepExit {
    if order.service == SELF_SERVICE {
        // hostkeeper's own steps restart nothing: its unit runs this daemon, and its new binary
        // switches once it has started ([`crate::own_package`]).
        return BUILTIN_EXIT;
    }
    let (version, dir) = match (step, &order.previous, order.previous_version.as_deref()) {
        (Step::Rollback, Some(previous), Some(version)) => (version, previous.dir()),
        _ => (order.target_version.as_str(), order.staged.dir()),
    };
    // The rollback of a move (docs/design/packages-native.md, section 10.6) restarts the units of
    // the archive version the switch placed again.
    let moved_back = (step == Step::Rollback) & has_target(order.staged.dir());
    let manifest = match repository_manifest(ports, &order.service, version, dir, moved_back) {
        None => return BUILTIN_EXIT,
        Some(Ok(manifest)) => manifest,
        Some(Err(why)) => return exit(1, "release", "manifest_invalid", &why, None),
    };
    let bound_secs = match bound {
        Bound::Secs(secs) => secs.get(),
        Bound::Unbounded => u64::MAX,
    };
    match step {
        Step::Prepare | Step::Cleanup | Step::Uninstall => BUILTIN_EXIT,
        // A first install has nothing to start again: the switch back removed its package.
        Step::Rollback if order.previous.is_none() => BUILTIN_EXIT,
        Step::Cutover | Step::Rollback => restart(ports, &manifest.run, bound_secs, step),
        Step::Verify => verify(ports, &manifest.name, &manifest.run),
    }
}

/// Stops and starts every unit of `run`, then waits for each to be active.
fn restart(ports: &BuiltinPorts, run: &[String], bound_secs: u64, step: Step) -> StepExit {
    let stopped = run.iter().try_for_each(|unit| {
        ports
            .units
            .stop_unit(unit)
            .map_err(|error| error.to_string())
    });
    let started = stopped.and_then(|()| {
        start_and_wait(
            &*ports.units,
            &*ports.clock,
            run,
            bound_secs,
            &format!("the {} bound", step.name()),
        )
    });
    match started {
        Ok(()) => BUILTIN_EXIT,
        Err(why) => exit(1, "release", REASON_UNIT_INACTIVE, &why, None),
    }
}

/// Whether every unit of `run` is active and the health URL of `service` answers 2xx.
fn verify(ports: &BuiltinPorts, service: &ServiceName, run: &[String]) -> StepExit {
    let inactive: Vec<&str> = run
        .iter()
        .filter(|unit| !ports.units.is_active(unit).unwrap_or(false))
        .map(String::as_str)
        .collect();
    let defer = |detail: &str| {
        exit(
            75,
            "environment",
            REASON_NOT_SERVING,
            detail,
            Some(VERIFY_RETRY_SECS),
        )
    };
    if !inactive.is_empty() {
        return defer(&format!("not active: {}", inactive.join(", ")));
    }
    let health = crate::observer::observe(
        &ports.observer,
        &ports.packages,
        &ports.binaries,
        &ports.own,
        service,
    );
    match health.map(|seen| seen.health) {
        Some(HealthView::Unhealthy { reason }) => {
            defer(&format!("the health URL is {}", reason.token()))
        }
        _ => BUILTIN_EXIT,
    }
}

#[cfg(test)]
#[path = "builtin_tests.rs"]
mod tests;
