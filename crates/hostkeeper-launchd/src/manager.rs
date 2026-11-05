//! The observer's service manager over launchd: one `launchctl print system/<label>` per question.
//!
//! A run unit of a manifest is a launchd label here. A label launchd does not hold is
//! [`ActiveState::NotFound`]; a job that is loaded is read from its `state` and its last exit.
//! Anything that keeps the answer from being read is an error and never a state, so the observer
//! discards the round instead of calling a service absent or stopped.

use hostkeeper_observe::{ActiveState, ObserveError, ServiceManager, UnitState};

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
        JobState::SpawnScheduled => (ActiveState::Activating, None),
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
    })
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
            }),
            JobStatus::Loaded(info) => loaded(&info).map_err(manager_error),
        }
    }
}
