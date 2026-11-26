//! The real [`ServiceManager`] on Linux: one `systemctl show` per unit, run through a
//! [`CommandRunner`], and its properties read.
//!
//! `systemctl show` of [`SHOW_PROPERTIES`] (`ActiveState`, `SubState`, `MainPID`, `LoadState`,
//! `NRestarts`, `Result`, `ExecMainCode`, `ExecMainStatus`) `-- <unit>` prints one `Key=Value`
//! line per property it has. `LoadState=not-found` is
//! [`ActiveState::NotFound`] whatever the other properties say; otherwise `ActiveState` decides
//! (`active`, `reloading` and `refreshing` are running). `MainPID=0` is no main process. A failure
//! to run, a deadline passed, an exit other than 0, output past the runner's cap, a missing one of
//! the first four properties, an `ActiveState` this module does not know or a `MainPID` that is
//! not a number is `Err`, never a state.
//!
//! The unit's restarts ([`UnitRestarts`]) are read from the other four: `NRestarts`, the count;
//! `Result`, the last result (empty when it is not printed); and the exit code `ExecMainStatus`
//! when `ExecMainCode=1` (the main process exited by itself; 2 and 3 are a signal, 0 no exit
//! yet). A unit with no `NRestarts` (a systemd before 235, a unit type that is not a service) or
//! not found has no restarts. An `NRestarts`, or with `ExecMainCode=1` an `ExecMainStatus`, that
//! is not a number is `Err`, like a `MainPID` that is not.
//!
//! This is the only module that knows systemd; launchd gets its own adapter of the same port.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::error::ObserveError;
use crate::ports::{ActiveState, CommandRunner, ServiceManager, UnitRestarts, UnitState};

/// The properties asked for.
pub const SHOW_PROPERTIES: &str = "--property=ActiveState,SubState,MainPID,LoadState,NRestarts,Result,ExecMainCode,ExecMainStatus";

/// The service manager over `systemctl`.
#[derive(Clone, Debug)]
pub struct SystemdManager<R> {
    runner: R,
    deadline: Duration,
}

impl<R: CommandRunner> SystemdManager<R> {
    /// Asks systemd through `runner`, waiting at most `deadline` for each answer.
    pub fn new(runner: R, deadline: Duration) -> SystemdManager<R> {
        SystemdManager { runner, deadline }
    }
}

impl<R: CommandRunner> ServiceManager for SystemdManager<R> {
    fn unit_state(&self, unit: &str) -> Result<UnitState, ObserveError> {
        let failed = |why: String| ObserveError::Manager {
            unit: unit.to_owned(),
            why,
        };
        let argv = ["systemctl", "show", SHOW_PROPERTIES, "--", unit].map(str::to_owned);
        let out = self
            .runner
            .run(&argv, self.deadline)
            .map_err(|e| failed(e.to_string()))?;
        match (out.exit_code, out.truncated) {
            (Some(0), false) => parse_show(&out.stdout).map_err(failed),
            (Some(0), true) => Err(failed(
                "systemctl show printed more than its cap".to_owned(),
            )),
            (code, _) => Err(failed(format!(
                "systemctl show exited with {}",
                code.map_or("a signal".to_owned(), |code| format!("code {code}"))
            ))),
        }
    }
}

/// The unit's state from the output of `systemctl show`.
fn parse_show(stdout: &[u8]) -> Result<UnitState, String> {
    let text = std::str::from_utf8(stdout)
        .map_err(|_| "systemctl show printed something that is not UTF-8".to_owned())?;
    let properties: BTreeMap<&str, &str> = text
        .lines()
        .filter_map(|line| line.split_once('='))
        .collect();
    let property = |key: &str| {
        properties
            .get(key)
            .copied()
            .ok_or_else(|| format!("systemctl show printed no {key}"))
    };
    let load = property("LoadState")?;
    let active = property("ActiveState")?;
    let sub = property("SubState")?;
    let pid = property("MainPID")?;
    let main_pid = pid
        .parse::<u32>()
        .map_err(|_| format!("MainPID {pid:?} is not a number"))?;
    let active = match (load, active) {
        ("not-found", _) => ActiveState::NotFound,
        (_, "active" | "reloading" | "refreshing") => ActiveState::Active,
        (_, "inactive") => ActiveState::Inactive,
        (_, "failed") => ActiveState::Failed,
        (_, "activating") => ActiveState::Activating,
        (_, "deactivating") => ActiveState::Deactivating,
        (_, other) => {
            return Err(format!(
                "ActiveState {other:?} is not one this daemon knows"
            ));
        }
    };
    let restarts = match (active, properties.get("NRestarts")) {
        (ActiveState::NotFound, _) | (_, None) => None,
        (_, Some(count)) => Some(UnitRestarts {
            count: count
                .parse::<u32>()
                .map_err(|_| format!("NRestarts {count:?} is not a number"))?,
            result: properties
                .get("Result")
                .copied()
                .unwrap_or_default()
                .to_owned(),
            exit_status: exit_status(&properties)?,
        }),
    };
    Ok(UnitState {
        active,
        sub: sub.to_owned(),
        main_pid: (main_pid != 0).then_some(main_pid),
        restarts,
    })
}

/// The main process's exit code: `ExecMainStatus` when `ExecMainCode` is 1 (`CLD_EXITED`), and
/// `None` for any other code (a signal, or no exit yet) or none printed.
fn exit_status(properties: &BTreeMap<&str, &str>) -> Result<Option<i32>, String> {
    if properties.get("ExecMainCode").copied() != Some("1") {
        return Ok(None);
    }
    let status = properties
        .get("ExecMainStatus")
        .copied()
        .unwrap_or_default();
    status
        .parse::<i32>()
        .map(Some)
        .map_err(|_| format!("ExecMainStatus {status:?} is not a number"))
}
