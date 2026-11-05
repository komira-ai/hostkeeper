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
    /// No bound: only `prepare`, whose manifest timeout is 0
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
    /// The bound of this step: the manifest's bound of the step, or the time left of the verify
    /// window when that is shorter; [`Bound::Unbounded`] only for a `prepare` whose manifest timeout is
    /// 0.
    pub bound: Bound,
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

/// One run of a probe: a verb of a binary that is not a hook, run once with nothing but its
/// arguments, bounded, its standard output kept (`<executable> <args...>`). The daemon asks a new
/// hostkeeper binary for its self-update protocol this way before it stages it.
#[derive(Clone, Debug)]
pub struct ProbeCall<'a> {
    /// The service the binary belongs to.
    pub service: &'a str,
    /// The binary: a file of a verified release (see [`StepCall::executable`]).
    pub executable: &'a Path,
    /// Its arguments.
    pub args: &'a [&'a str],
    /// The deterministic name of the transient unit it runs in.
    pub unit: &'a str,
    /// The bound of the run, in seconds: the launcher sets `RuntimeMaxSec=` to it.
    pub timeout_secs: u64,
    /// The most bytes of standard output the launcher keeps; what passes it is dropped and
    /// reported ([`ProbeExit::truncated`]).
    pub output_bytes: usize,
}

/// How a probe ended, as the launcher saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeExit {
    /// The exit code; `None` when the probe was killed by its bound or by a signal.
    pub code: Option<i32>,
    /// Its standard output, at most [`ProbeCall::output_bytes`] of it.
    pub output: Vec<u8>,
    /// It wrote more than [`ProbeCall::output_bytes`].
    pub truncated: bool,
}

/// How a step ended, as the launcher saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepExit {
    /// The exit code; `None` when the step was killed by a signal, by its bound where the
    /// launcher reports that as no code, or ended by a [`StepLauncher::stop`] of its unit
    /// whatever it exited with. Either way a code that is not `0`, `75` or `76`, and no code,
    /// are a failed step.
    pub code: Option<i32>,
    /// The last line the hook wrote to standard output that is not blank, `None` when it wrote
    /// none. [`crate::systemd::SystemdLauncher`] also gives `None` for a last line longer than
    /// [`crate::LAST_LINE_MAX`] or one whose beginning fell out of the kept
    /// [`crate::RING_BYTES`]: the report is then `{}` and the exit code decides.
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
///   the dead-man is a thread of the daemon: if the daemon is killed (`SIGKILL`, the OOM
///   killer, a crash), the unit it started runs without a bound until the updater is back and
///   resumes. The launcher sets `RuntimeMaxSec=` from [`StepCall::bound`] on the unit (the
///   seconds, or `infinity` for [`Bound::Unbounded`]), so that systemd ends the step at its
///   bound whatever becomes of the updater.
pub trait StepLauncher: Send + Sync {
    /// Runs the step and waits for it to end. A step killed by its bound is a [`StepExit`] of a
    /// failed step: no code, or (from `systemd-run --wait`) a code that is not one of the
    /// contract's. A run that a [`Self::stop`] of its unit ended has no code, even when the
    /// process exited `0` (the stop's `SIGTERM` reads as a clean end of a service to systemd):
    /// the dead-man's stop at the bound is a failed step.
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

    /// Runs an additive step and waits for it to end, under the same rules as [`Self::run`]. A
    /// launcher that does not run additive steps keeps this default, which starts nothing and
    /// answers [`LaunchError::NoStart`] naming the step: the caller reports that step as failed.
    fn run_additive(&self, call: &AdditiveCall<'_>) -> Result<StepExit, LaunchError> {
        Err(LaunchError::NoStart(format!(
            "this launcher runs no additive step, so `{}` of {} did not start",
            call.step, call.service
        )))
    }

    /// Runs a probe and waits for it to end, bounded by [`ProbeCall::timeout_secs`], keeping at
    /// most [`ProbeCall::output_bytes`] of its standard output. A launcher that does not run
    /// probes keeps this default, which starts nothing and answers [`LaunchError::NoStart`]
    /// naming the unit: the caller reads it as a probe that could not run.
    fn run_probe(&self, call: &ProbeCall<'_>) -> Result<ProbeExit, LaunchError> {
        Err(LaunchError::NoStart(format!(
            "this launcher runs no probe, so `{}` of {} did not start",
            call.unit, call.service
        )))
    }
}
