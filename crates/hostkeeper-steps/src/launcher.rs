//! What the runner needs from whatever runs one step of a hook with the rights of its
//! declaration (`hostkeeper-rights` has the launcher of transient systemd units). The runner
//! decides which step runs next, with which executable, from which release and by when; the
//! launcher decides how: the user, the capabilities, the paths and the units of the declaration.

use std::io;
use std::path::Path;

use hostkeeper_hook::Invocation;

/// One run of one step.
#[derive(Clone, Debug)]
pub struct StepCall<'a> {
    /// The service, whose declaration the launcher runs the step under.
    pub service: &'a str,
    /// The step and its arguments, in the form `Invocation::to_args` prints: `prepare` carries
    /// the staged release as its `<artifact>`, every step carries `--params <file>`.
    pub invocation: Invocation,
    /// The hook executable: a file of the verified release the runner checked with
    /// `Staged::check` just before (see its guarantees), the previous release's only for the
    /// `rollback` fallback. The launcher executes this path as it is.
    pub executable: &'a Path,
    /// The deterministic name of the transient unit: `updater-<service>-<step>-<generation>`.
    /// A launcher that runs the step as a unit uses it; it is stopped by name on resume.
    pub unit: &'a str,
    /// The bound of this step, in seconds: the declaration's `timeout_secs` for the step, or
    /// the time left of the verify window when that is shorter.
    pub timeout_secs: u64,
}

/// One run of an additive step: a step of the hook that is not one of the five of an update, named
/// by the package's manifest (the `[hook] boot` step, which the daemon runs from `current` at its
/// start). The hook is run as `<executable> <hook_args...> <step> --params <params>`.
#[derive(Clone, Debug)]
pub struct AdditiveCall<'a> {
    /// The service the step belongs to.
    pub service: &'a str,
    /// The step's name, as the manifest gives it: a token that is none of the five steps.
    pub step: &'a str,
    /// The arguments of the manifest's hook command after its program, given before the step
    /// (`update-hook` in `["bin/tierstore", "update-hook"]`).
    pub hook_args: &'a [String],
    /// The params file of the run.
    pub params: &'a Path,
    /// The hook executable, a file of a verified release (see [`StepCall::executable`]).
    pub executable: &'a Path,
    /// The deterministic name of the transient unit: `updater-<service>-<step>-<generation>`.
    pub unit: &'a str,
    /// The bound of the run, in seconds.
    pub timeout_secs: u64,
}

/// How a step ended, as the launcher saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepExit {
    /// The exit code; `None` when the step was killed by its bound or by a signal.
    pub code: Option<i32>,
    /// The last line the hook wrote to standard output, `None` when it wrote none.
    pub last_line: Option<String>,
}

/// Why a step could not be run to an end.
#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    /// The hook could not be started (the executable is missing or cannot be executed). The
    /// runner reads it as a failed step, and as the one reason to try the previous release's
    /// hook for `rollback`.
    #[error("the hook did not start: {0}")]
    NoStart(String),
    /// The launcher itself failed (no unit could be made, the unit could not be read): the
    /// step is left `running` in the record and the runner stops with this error. Resume stops
    /// the unit by name and runs the step again.
    #[error("the launcher failed: {0}")]
    Launcher(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Runs steps. One launcher is shared by the runner and the dead-man, which stops a step from
/// its own thread.
///
/// What a launcher must do that the runner cannot check (a launcher of transient units is
/// held to it):
///
/// - **A unit name is used again.** The runner starts a step again under the same
///   [`StepCall::unit`] when the hook asks for another try (`Next::Again`: a deferred
///   `rollback` or `cleanup`, a deferred `verify`, the post-success watcher) and after a resume.
///   It does not stop or reset the unit in between (it does stop a unit found `running` on
///   resume). systemd keeps a transient unit that failed loaded under its name, and refuses to
///   start another unit of that name, so the launcher clears it before every start
///   (`systemctl reset-failed <unit>`, a unit that is not loaded being no error) or starts the
///   unit with `CollectMode=inactive-or-failed`, so that systemd drops it once it has ended,
///   failed or not.
/// - **The unit is bounded by itself.** The runner arms the dead-man with the step's bound, but
///   the dead-man is a thread of the root part: if the root part is killed (`SIGKILL`, the OOM
///   killer, a crash), the unit it started runs without a bound until the updater is back and
///   resumes. The launcher sets `RuntimeMaxSec=` to [`StepCall::timeout_secs`] on the unit, so
///   that systemd ends the step at its bound whatever becomes of the updater. `RuntimeMaxSec=`
///   has no effect on a `Type=oneshot` service: a launcher that starts the step as one bounds
///   it with `TimeoutStartSec=` instead.
pub trait StepLauncher: Send + Sync {
    /// Runs the step and waits for it to end. A step killed by its bound is a [`StepExit`] with
    /// no code.
    fn run(&self, call: &StepCall<'_>) -> Result<StepExit, LaunchError>;

    /// Stops the unit of that name and waits until it has ended; a unit that is not running is
    /// not an error.
    ///
    /// The dead-man calls it from its thread and waits in it, enforcing nothing else meanwhile,
    /// so it must end within a bound of its own and escalate: systemd's stop sends `SIGTERM`, then
    /// `SIGKILL` to the whole unit after `TimeoutStopSec=` (with the default `KillMode=` of the
    /// control group), and the launcher sets that bound on the unit. An error is kept by the
    /// dead-man, the stop is tried again at its next tick, and a step that ends before any stop
    /// worked stops the runner with [`crate::RunError::Unstopped`]. A step whose [`Self::run`]
    /// returns while this is under way is not read until this has returned, so the time this
    /// takes is also the time the runner waits.
    fn stop(&self, unit: &str) -> Result<(), LaunchError>;

    /// Runs an additive step and waits for it to end, under the same rules as [`Self::run`]. A
    /// launcher that does not run additive steps keeps this default, which starts nothing and
    /// answers [`LaunchError::NoStart`] naming the step: the caller reports that step as failed.
    fn run_additive(&self, call: &AdditiveCall<'_>) -> Result<StepExit, LaunchError> {
        Err(LaunchError::NoStart(format!(
            "this launcher runs no additive step, so `{}` of {} did not start",
            call.step, call.service
        )))
    }
}
