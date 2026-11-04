//! What the runner needs from whatever runs one step of a hook. The runner decides which step
//! runs next, with which executable, from which release and by when; the launcher runs it and
//! reports how it ended. [`crate::systemd::SystemdLauncher`] is the launcher of the host: a
//! transient systemd unit per step, as root, with `RuntimeMaxSec=` and nothing else.

use std::io;
use std::num::NonZeroU64;
use std::path::Path;

use hostkeeper_hook::Invocation;

/// How long a step may run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bound {
    /// At most this many seconds.
    Secs(NonZeroU64),
    /// No bound: only `prepare`, declared with `prepare = 0`
    /// (docs/design/hostkeeper-design.md, sections 4.7 and 8.5). Nothing but the hook's own end
    /// ends it: the dead-man is not armed and the unit has `RuntimeMaxSec=infinity`.
    Unbounded,
}

/// One run of one step.
#[derive(Clone, Debug)]
pub struct StepCall<'a> {
    /// The service the step belongs to.
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
    /// The bound of this step: the declared bound of the step, or the time left of the verify
    /// window when that is shorter; [`Bound::Unbounded`] only for a `prepare` declared with 0.
    pub bound: Bound,
}

/// How a step ended, as the launcher saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepExit {
    /// The exit code; `None` when the step was killed by a signal, or by its bound where the
    /// launcher reports that as no code. Either way a code that is not `0`, `75` or `76`, and
    /// no code, are a failed step.
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
///   resume). systemd refuses to start a unit under a name that is still loaded, and keeps a
///   unit that failed loaded; [`crate::systemd::SystemdLauncher`] starts every unit with
///   `--collect`, so that systemd unloads it once it has ended, failed or not, and makes no
///   `systemctl reset-failed` call.
/// - **The unit is bounded by itself.** The runner arms the dead-man with the step's bound, but
///   the dead-man is a thread of the root part: if the root part is killed (`SIGKILL`, the OOM
///   killer, a crash), the unit it started runs without a bound until the updater is back and
///   resumes. The launcher sets `RuntimeMaxSec=` from [`StepCall::bound`] on the unit (the
///   seconds, or `infinity` for [`Bound::Unbounded`]), so that systemd ends the step at its
///   bound whatever becomes of the updater.
pub trait StepLauncher: Send + Sync {
    /// Runs the step and waits for it to end. A step killed by its bound is a [`StepExit`] of a
    /// failed step: no code, or (from `systemd-run --wait`) a code that is not one of the
    /// contract's.
    fn run(&self, call: &StepCall<'_>) -> Result<StepExit, LaunchError>;

    /// Stops the unit of that name and waits until it has ended; a unit that is not running is
    /// not an error.
    ///
    /// The dead-man calls it from its thread and waits in it, enforcing nothing else meanwhile,
    /// so it must end within a bound of its own and escalate. [`crate::systemd::SystemdLauncher`]
    /// runs `systemctl stop --system <unit>`, which waits for the unit to end: systemd sends
    /// `SIGTERM`, then `SIGKILL` to the whole control group (the default `KillMode=`) after the
    /// stop timeout. The unit sets no `TimeoutStopSec=` (`RuntimeMaxSec=` is its one property),
    /// so that timeout is systemd's default (`DefaultTimeoutStopSec=`). An error is kept by the
    /// dead-man, the stop is tried again at its next tick, and a step that ends before any stop
    /// worked stops the runner with [`crate::RunError::Unstopped`]. A step whose [`Self::run`]
    /// returns while this is under way is not read until this has returned, so the time this
    /// takes is also the time the runner waits.
    fn stop(&self, unit: &str) -> Result<(), LaunchError>;
}
