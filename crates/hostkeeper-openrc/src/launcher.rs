//! The [`StepLauncher`] of an OpenRC host (placeholder: replaced by the full launcher).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_steps::{
    Clock, LaunchError, ProcBootId, StepCall, StepExit, StepLauncher, StepLog, SystemClock,
};

/// Where the launcher keeps a file per running step: its process group, so that a daemon started
/// again can stop a step an earlier one left running.
pub const STEPS_DIR: &str = "/run/hostkeeper/steps";

/// The [`StepLauncher`] of an OpenRC host.
pub struct OpenrcLauncher {
    steps_dir: PathBuf,
    clock: Arc<dyn Clock>,
    log: Option<StepLog>,
}

impl OpenrcLauncher {
    /// The host's launcher: its files in [`STEPS_DIR`], its time the kernel's monotonic clock.
    pub fn system() -> OpenrcLauncher {
        OpenrcLauncher::new(
            Path::new(STEPS_DIR),
            Arc::new(SystemClock::new(Box::new(ProcBootId::host()))),
        )
    }

    /// A launcher that keeps its files in `steps_dir` and reads time on `clock`.
    pub fn new(steps_dir: &Path, clock: Arc<dyn Clock>) -> OpenrcLauncher {
        OpenrcLauncher {
            steps_dir: steps_dir.to_path_buf(),
            clock,
            log: None,
        }
    }

    /// The same launcher, giving `log` the output of every step it ran.
    pub fn with_log(mut self, log: StepLog) -> OpenrcLauncher {
        self.log = Some(log);
        self
    }
}

impl StepLauncher for OpenrcLauncher {
    fn run(&self, _call: &StepCall<'_>) -> Result<StepExit, LaunchError> {
        let _ = (&self.steps_dir, &self.clock, &self.log);
        Err(LaunchError::Launcher("placeholder".to_owned()))
    }

    fn stop(&self, _unit: &str) -> Result<(), LaunchError> {
        Err(LaunchError::Launcher("placeholder".to_owned()))
    }
}
