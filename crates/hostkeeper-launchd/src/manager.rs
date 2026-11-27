//! The observer's service manager over launchd: one `launchctl print system/<label>` per question.
//!
//! A run unit of a manifest is a launchd label here. A label launchd does not hold is
//! [`ActiveState::NotFound`]; a job that is loaded is read from its `state` and its last exit.
//! Its restarts ([`UnitRestarts`]) are read from launchd's `runs`, the times launchd spawned the
//! job since it was loaded, less the first: a job that never ran, or ran once, has restarted 0
//! times. Its result is `signal` when a signal ended its last run, `success` when that run exited
//! 0, `exit-code` when it exited otherwise, and empty when it never exited; its exit status is
//! that run's exit code. A job whose print has no `runs` line has no restarts. Unlike systemd's
//! `NRestarts`, `runs` counts a `kickstart` by hand too; a `bootout` and `bootstrap` (how a
//! changed plist is loaded again) start it at 0, so the window of the restarts starts again.
//! Anything that keeps the answer from being read is an error and never a state, so the observer
//! discards the round instead of calling a service absent or stopped.

use hostkeeper_observe::{ActiveState, ObserveError, ServiceManager, UnitRestarts, UnitState};

use crate::label::Label;
use crate::launchctl::{JobInfo, JobState, JobStatus, Launchctl, print};

/// [`ServiceManager`] by `launchctl print`.
#[derive(Clone, Debug)]
pub struct LaunchdServiceManager<L> {
    launchctl: L,
}

impl<L: Launchctl> LaunchdServiceManager<L> {
    /// A manager that asks `launchctl`.
    pub fn new(launchctl: L) -> Self {
        LaunchdServiceManager { launchctl }
    }
}

/// The state of a loaded job, or the launchd state word this code does not know.
fn loaded(info: &JobInfo) -> Result<UnitState, String> {
    let failed =
        info.last_terminating_signal.is_some() | info.last_exit_status.is_some_and(|c| c != 0);
    let (active, main_pid) = match &info.state {
        JobState::Running => (ActiveState::Active, info.pid),
        JobState::SpawnScheduled | JobState::Spawning => (ActiveState::Activating, None),
        // One arm and one `if`: a guard on an or-pattern is a branch per alternative.
        JobState::Waiting | JobState::NotRunning => {
            let active = if failed {
                ActiveState::Failed
            } else {
                ActiveState::Inactive
            };
            (active, None)
        }
        JobState::Other(word) => return Err(format!("the job state {word:?} is not known")),
    };
    Ok(UnitState {
        active,
        sub: info.state.word().to_owned(),
        main_pid,
        restarts: info.runs.map(|runs| UnitRestarts {
            count: runs.saturating_sub(1),
            result: last_result(info).to_owned(),
            exit_status: info.last_exit_status,
        }),
    })
}

/// The last result of a loaded job, in the words of [`UnitRestarts::result`] (the module's doc).
fn last_result(info: &JobInfo) -> &'static str {
    match (info.last_terminating_signal, info.last_exit_status) {
        (Some(_), _) => "signal",
        (None, Some(0)) => "success",
        (None, Some(_)) => "exit-code",
        (None, None) => "",
    }
}

impl<L: Launchctl> ServiceManager for LaunchdServiceManager<L> {
    fn unit_state(&self, unit: &str) -> Result<UnitState, ObserveError> {
        let manager_error = |why: String| ObserveError::Manager {
            unit: unit.to_owned(),
            why,
        };
        let label = Label::parse(unit).map_err(|e| manager_error(e.to_string()))?;
        match print(&self.launchctl, &label).map_err(|e| manager_error(e.to_string()))? {
            JobStatus::NotLoaded => Ok(UnitState {
                active: ActiveState::NotFound,
                sub: "not loaded".to_owned(),
                main_pid: None,
                restarts: None,
            }),
            JobStatus::Loaded(info) => loaded(&info).map_err(manager_error),
        }
    }
}
