//! The steps after `prepare`: `cutover`, `verify`, `rollback`, `cleanup` and `uninstall`
//! (module [`super`] says what each does).

use std::fs;

use super::boot::{CURRENT, current_version, is_dir, point_current, remove_file, remove_tree};
use super::lock::Lock;
use super::params::HookParams;
use super::{Failure, Host};

/// The seconds `verify` asks to be run again after, while the unit is not active.
const VERIFY_RETRY_SECS: u64 = 2;
/// `systemctl stop`'s exit code for a unit that is not loaded.
const NOT_LOADED: i32 = 5;

/// Points `current` at `version`.
fn point(params: &HookParams, version: &str) -> Result<(), Failure> {
    point_current(&params.boot_dir, version)
        .map_err(|e| Failure::io(&params.boot_dir.join(CURRENT), &e))
}

/// Runs `systemctl <args>`; an exit code outside `accepted`, and a program that cannot be run,
/// are the release failure `reason`.
fn systemctl(
    host: &Host<'_>,
    args: &[&str],
    accepted: &[i32],
    reason: &str,
) -> Result<(), Failure> {
    let words = args.join(" ");
    match host.systemctl.run(args) {
        Ok(code) if accepted.contains(&code) => Ok(()),
        Ok(code) => Err(Failure::release(
            reason,
            format!("systemctl {words} exited {code}"),
        )),
        Err(e) => Err(Failure::release(
            reason,
            format!("systemctl {words} could not run: {e}"),
        )),
    }
}

/// Points `current` at the target and restarts the unit.
pub(super) fn cutover(host: &Host<'_>, params: &HookParams) -> Result<(), Failure> {
    let _lock = Lock::create(&params.boot_dir)?;
    let dir = params.boot_dir.join(&params.target);
    if !is_dir(&dir) {
        return Err(Failure::release(
            "not_prepared",
            format!(
                "{} is not a directory: the version was not prepared",
                dir.display()
            ),
        ));
    }
    point(params, &params.target)?;
    systemctl(host, &["restart", &params.unit], &[0], "not_started")
}

/// The unit is active, or the step is deferred.
pub(super) fn verify(host: &Host<'_>, params: &HookParams) -> Result<(), Failure> {
    match host.systemctl.run(&["is-active", "--quiet", &params.unit]) {
        Ok(0) => Ok(()),
        _ => Err(Failure::deferred(
            "not_active",
            VERIFY_RETRY_SECS,
            format!("{} is not active", params.unit),
        )),
    }
}

/// Puts the previous boot set back (pointing `current` at it and restarting the unit only when
/// it does not already serve it), or stops the unit when there is none; removes the target's.
pub(super) fn rollback(host: &Host<'_>, params: &HookParams) -> Result<(), Failure> {
    let _lock = Lock::create(&params.boot_dir)?;
    let previous = params
        .previous
        .as_deref()
        .filter(|version| is_dir(&params.boot_dir.join(version)));
    match previous {
        Some(version) => {
            // A failed `prepare` never moved `current`: the live server stays as it is.
            if current_version(&params.boot_dir).as_deref() != Some(version) {
                point(params, version)?;
                systemctl(host, &["restart", &params.unit], &[0], "not_restarted")?;
            }
        }
        None => {
            systemctl(
                host,
                &["stop", &params.unit],
                &[0, NOT_LOADED],
                "not_stopped",
            )?;
            let current = params.boot_dir.join(CURRENT);
            remove_file(&current).map_err(|e| Failure::io(&current, &e))?;
        }
    }
    if previous != Some(params.target.as_str()) {
        let dir = params.boot_dir.join(&params.target);
        remove_tree(&dir).map_err(|e| Failure::io(&dir, &e))?;
    }
    Ok(())
}

/// Removes every entry of the boot directory but `current` and the target.
pub(super) fn cleanup(params: &HookParams) -> Result<(), Failure> {
    let _lock = Lock::create(&params.boot_dir)?;
    let listed = fs::read_dir(&params.boot_dir).map_err(|e| Failure::io(&params.boot_dir, &e))?;
    for entry in listed {
        let entry = entry.map_err(|e| Failure::io(&params.boot_dir, &e))?;
        let name = entry.file_name();
        if name == CURRENT || name.to_str() == Some(params.target.as_str()) {
            continue;
        }
        let path = entry.path();
        let removed = match entry.file_type() {
            Ok(kind) if kind.is_dir() => remove_tree(&path),
            _ => remove_file(&path),
        };
        removed.map_err(|e| Failure::io(&path, &e))?;
    }
    Ok(())
}

/// Removes the boot directory whole; one that is not there is done.
pub(super) fn uninstall(params: &HookParams) -> Result<(), Failure> {
    if !is_dir(&params.boot_dir) {
        return Ok(());
    }
    let _lock = Lock::open(&params.boot_dir)?;
    remove_tree(&params.boot_dir).map_err(|e| Failure::io(&params.boot_dir, &e))
}
