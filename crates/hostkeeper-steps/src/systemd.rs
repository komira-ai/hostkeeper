//! The launcher of the host: each step of a hook is a transient systemd unit, run as root, with
//! `RuntimeMaxSec=` and nothing else (docs/design/hostkeeper-design.md, section 5.3). The unit
//! is there for the timeout, for the kill of its whole control group, and for the capture of the
//! exit code, the last line of standard output and the log; it confines nothing.
//!
//! The command lines are pure functions ([`command`], [`additive_command`],
//! [`stop_command`]). Starting a program sits
//! behind [`Spawner`], so [`SystemdLauncher`] runs against a fake in a test.
//! [`SystemdRunProcess`] is the real spawner, and the only code of the crate that starts a
//! process: `systemd-run` or `systemctl`, with an empty environment and no input, both of its
//! output streams read to their end at once into a [`Ring`] each.
//!
//! The line of one step is
//!
//! ```text
//! systemd-run --system --quiet --wait --collect --pipe --unit=updater-<service>-<step>-<generation>
//!     --property=RuntimeMaxSec=<secs|infinity> -- <executable> <step> [<artifact>] --params <file>
//! ```
//!
//! `--wait` makes `systemd-run` wait for the unit and exit with the hook's exit status (a unit
//! ended by `RuntimeMaxSec=` exits with one that is not the contract's: a failed step);
//! `--collect` makes systemd unload the unit once it has ended, failed or not, so its name can
//! be used again without `systemctl reset-failed`; `--pipe` carries the hook's output through.
//!
//! An additive step ([`StepLauncher::run_additive`]: the hook's `uninstall`, or a `[hook] boot`
//! step) is run as a step is, its arguments the hook command's own before the step's name
//! ([`additive_command`]):
//!
//! ```text
//! systemd-run --system --quiet --wait --collect --pipe --unit=updater-<service>-<step>-<generation>
//!     --property=RuntimeMaxSec=<secs|infinity> -- <executable> <hook_args...> <step> --params <file>
//! ```
//!
//! A `timeout_secs` of 0 is no bound (`infinity`), as for a `prepare` whose manifest timeout is 0.
//! Its exit, its last line, its log and a [`StepLauncher::stop`] of its unit while it runs are
//! read as for a step.
//!
//! A step (or an additive step) that [`StepLauncher::stop`] stopped is reported with no exit
//! code, whatever `systemd-run` exited with. systemd counts the end of a service's main process
//! by the `SIGTERM` of a stop as a clean end (result `success`), so `systemd-run --wait` exits `0`
//! for a hook that does not handle the signal; without this, a step the dead-man stopped at its
//! bound would read as a step that succeeded. A step ended by `RuntimeMaxSec=` needs no such rule: its result is
//! `timeout`, and `systemd-run --wait` exits `1`.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::num::NonZeroU64;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use hostkeeper_hook::Step;

use crate::capture::Ring;
use crate::launcher::{AdditiveCall, Bound, LaunchError, StepCall, StepExit, StepLauncher};

/// Where `systemd-run` is.
pub const SYSTEMD_RUN: &str = "/usr/bin/systemd-run";
/// Where `systemctl` is.
pub const SYSTEMCTL: &str = "/usr/bin/systemctl";
/// What `systemctl stop` exits with for a unit that is not loaded: nothing to stop.
pub const EXIT_NOT_LOADED: i32 = 5;

/// The unit that runs `step` of `service` in the update numbered `generation`, without the
/// `.service` systemd adds. The same name for every run of the step in that update, so that a
/// resumed step finds the unit of the run it replaces.
pub fn unit_name(service: &str, step: Step, generation: u64) -> String {
    format!("updater-{service}-{step}-{generation}")
}

/// The value of `RuntimeMaxSec=` for a bound: its seconds, or `infinity` (never `0`).
fn runtime_max_sec(bound: Bound) -> String {
    match bound {
        Bound::Secs(secs) => secs.to_string(),
        Bound::Unbounded => "infinity".to_owned(),
    }
}

/// The arguments of `systemd-run` that run `executable` with `rest` in the transient unit `unit`,
/// with `RuntimeMaxSec=` set to `max_sec`.
fn unit_line(
    unit: &str,
    max_sec: &str,
    executable: &Path,
    rest: impl IntoIterator<Item = OsString>,
) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["--system", "--quiet", "--wait", "--collect", "--pipe"]
        .into_iter()
        .map(OsString::from)
        .collect();
    args.push(format!("--unit={unit}").into());
    args.push(format!("--property=RuntimeMaxSec={max_sec}").into());
    args.push("--".into());
    args.push(executable.into());
    args.extend(rest);
    args
}

/// `Ok` when the hook `executable` and the `params` file (if any) are absolute paths: the unit
/// does not run in the updater's directory.
fn absolute_hook(executable: &Path, params: Option<&Path>) -> Result<(), LaunchError> {
    if !executable.is_absolute() {
        return Err(LaunchError::Launcher(format!(
            "the hook executable {executable:?} is not an absolute path"
        )));
    }
    if let Some(params) = params.filter(|p| !p.is_absolute()) {
        return Err(LaunchError::Launcher(format!(
            "the params file {params:?} is not an absolute path"
        )));
    }
    Ok(())
}

/// The arguments of `systemd-run` that run one step. A relative executable or params file is
/// refused: the unit does not run in the updater's directory.
pub fn command(call: &StepCall<'_>) -> Result<Vec<OsString>, LaunchError> {
    absolute_hook(call.executable, call.invocation.params.as_deref())?;
    Ok(unit_line(
        call.unit,
        &runtime_max_sec(call.bound),
        call.executable,
        call.invocation.to_args(),
    ))
}

/// The arguments of `systemd-run` that run one additive step:
/// `<executable> <hook_args...> <step> --params <params>`, bounded by its `timeout_secs` (0 is no
/// bound: `infinity`). A relative executable or params file is refused, as for [`command`].
pub fn additive_command(call: &AdditiveCall<'_>) -> Result<Vec<OsString>, LaunchError> {
    absolute_hook(call.executable, Some(call.params))?;
    let bound = NonZeroU64::new(call.timeout_secs).map_or(Bound::Unbounded, Bound::Secs);
    let rest = call.hook_args.iter().map(OsString::from).chain([
        OsString::from(call.step),
        OsString::from("--params"),
        call.params.as_os_str().to_owned(),
    ]);
    Ok(unit_line(
        call.unit,
        &runtime_max_sec(bound),
        call.executable,
        rest,
    ))
}

/// The arguments of `systemctl` that stop a unit and wait for it to end.
pub fn stop_command(unit: &str) -> Vec<OsString> {
    ["stop", "--system", unit]
        .into_iter()
        .map(OsString::from)
        .collect()
}

/// A program the launcher starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Program {
    SystemdRun,
    Systemctl,
}

/// How a program ended, and what is kept of its output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Captured {
    /// The exit status; `None` when the program was killed by a signal.
    pub code: Option<i32>,
    /// The last [`crate::capture::RING_BYTES`] of its standard output.
    pub stdout: Ring,
    /// The last [`crate::capture::RING_BYTES`] of its standard error.
    pub stderr: Ring,
}

/// Starts a program with these arguments and waits for it to end.
pub trait Spawner: Send + Sync {
    fn spawn(&self, program: Program, args: &[OsString]) -> io::Result<Captured>;
}

/// The real spawner: runs `systemd-run` and `systemctl` with an empty environment and no input,
/// and reads standard output and standard error at once, on a thread each, so that a hook that
/// writes much to one of them is never blocked on a full pipe while the other is read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemdRunProcess {
    systemd_run: PathBuf,
    systemctl: PathBuf,
}

impl SystemdRunProcess {
    /// The host's programs: [`SYSTEMD_RUN`] and [`SYSTEMCTL`].
    pub fn system() -> SystemdRunProcess {
        SystemdRunProcess::new(Path::new(SYSTEMD_RUN), Path::new(SYSTEMCTL))
    }

    /// Other programs in their place: the tests' stand-in.
    pub fn new(systemd_run: &Path, systemctl: &Path) -> SystemdRunProcess {
        SystemdRunProcess {
            systemd_run: systemd_run.to_owned(),
            systemctl: systemctl.to_owned(),
        }
    }

    /// The file that is run for `program`.
    pub fn program(&self, program: Program) -> &Path {
        match program {
            Program::SystemdRun => &self.systemd_run,
            Program::Systemctl => &self.systemctl,
        }
    }
}

impl Spawner for SystemdRunProcess {
    fn spawn(&self, program: Program, args: &[OsString]) -> io::Result<Captured> {
        let (mut out_pipe, out_end) = io::pipe()?;
        let (mut err_pipe, err_end) = io::pipe()?;
        // The command holds the writing ends; it is dropped with this block, so that the pipes
        // end when the program and what it started have closed theirs.
        let mut child = {
            let mut command = Command::new(self.program(program));
            command
                .args(args)
                .env_clear()
                .stdin(Stdio::null())
                .stdout(out_end)
                .stderr(err_end);
            command.spawn()?
        };
        let (mut stdout, mut stderr) = (Ring::new(), Ring::new());
        let (mut read_out, mut read_err) = (Ok(0), Ok(0));
        let status = std::thread::scope(|scope| {
            scope.spawn(|| read_out = io::copy(&mut out_pipe, &mut stdout));
            scope.spawn(|| read_err = io::copy(&mut err_pipe, &mut stderr));
            child.wait()
        })?;
        read_out?;
        read_err?;
        Ok(Captured {
            code: status.code(),
            stdout,
            stderr,
        })
    }
}

/// What a launcher hands its log after each run of a step: the unit and what was captured.
pub type StepLog = Arc<dyn Fn(&str, &Captured) + Send + Sync>;

/// The [`StepLauncher`] of the host: one transient unit per step, through a [`Spawner`].
pub struct SystemdLauncher<S> {
    spawner: S,
    log: Option<StepLog>,
    /// The units [`StepLauncher::stop`] was asked to stop since the last run of a step under
    /// that name started: the run that is under way reports no exit code.
    stopped_units: Mutex<BTreeSet<String>>,
}

impl SystemdLauncher<SystemdRunProcess> {
    /// The launcher over the host's `systemd-run` and `systemctl`.
    pub fn system() -> SystemdLauncher<SystemdRunProcess> {
        SystemdLauncher::new(SystemdRunProcess::system())
    }
}

impl<S: Spawner> SystemdLauncher<S> {
    pub fn new(spawner: S) -> SystemdLauncher<S> {
        SystemdLauncher {
            spawner,
            log: None,
            stopped_units: Mutex::default(),
        }
    }

    /// The same launcher, giving `log` the output of every step it ran (the kept standard error
    /// is the hook's log).
    pub fn with_log(mut self, log: StepLog) -> SystemdLauncher<S> {
        self.log = Some(log);
        self
    }

    fn spawn(&self, program: Program, args: &[OsString]) -> Result<Captured, LaunchError> {
        spawned(program, self.spawner.spawn(program, args))
    }

    /// Runs `systemd-run` with `args`, the line of a step or an additive step in the unit
    /// `unit`, and reads how it ended. A stop of an earlier run under this name (a resume, or an
    /// uninstall, stops the unit it may find running before it runs it again) says nothing of
    /// this run; a stop made while it runs does.
    fn run_unit(&self, unit: &str, args: &[OsString]) -> Result<StepExit, LaunchError> {
        stops(&self.stopped_units).remove(unit);
        let captured = self.spawn(Program::SystemdRun, args)?;
        let was_stopped = stops(&self.stopped_units).remove(unit);
        Ok(step_exit(self.log.as_ref(), unit, &captured, was_stopped))
    }
}

// The decisions of the launcher are in the functions below, which are not generic: every
// spawner (the host's and a test's) runs the same code, measured as one function.

/// A spawn that failed is the launcher's failure, not the hook's.
fn spawned(program: Program, spawn: io::Result<Captured>) -> Result<Captured, LaunchError> {
    spawn.map_err(|e| LaunchError::Launcher(format!("{program:?} could not be run: {e}")))
}

/// The units a launcher stopped. A poisoned lock is still read: the set is only names.
fn stops(stopped: &Mutex<BTreeSet<String>>) -> MutexGuard<'_, BTreeSet<String>> {
    stopped.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How a step ended, after `log` (if any) was given what was captured. A step that `stop`
/// stopped while it ran has no exit code: `systemd-run` exits `0` for a hook that the stop's
/// `SIGTERM` ended (see the module's documentation), and a step stopped at its bound failed.
fn step_exit(
    log: Option<&StepLog>,
    unit: &str,
    captured: &Captured,
    was_stopped: bool,
) -> StepExit {
    if let Some(log) = log {
        log(unit, captured);
    }
    StepExit {
        code: if was_stopped { None } else { captured.code },
        last_line: captured.stdout.last_line(),
    }
}

/// Whether `systemctl stop` stopped the unit: exit `0`, or [`EXIT_NOT_LOADED`] (nothing to stop).
fn stopped(unit: &str, captured: &Captured) -> Result<(), LaunchError> {
    match captured.code {
        Some(0 | EXIT_NOT_LOADED) => Ok(()),
        code => Err(LaunchError::Launcher(format!(
            "systemctl stop {unit} exited with {code:?}: {}",
            String::from_utf8_lossy(&captured.stderr.bytes()).trim()
        ))),
    }
}

/// `Ok` when `path` is a file that can be executed: what is missing, or is not an executable
/// file, is a hook that cannot start.
fn executable(path: &Path) -> Result<(), LaunchError> {
    let meta =
        fs::metadata(path).map_err(|e| LaunchError::NoStart(format!("{}: {e}", path.display())))?;
    if meta.is_file() && meta.permissions().mode() & 0o111 != 0 {
        return Ok(());
    }
    Err(LaunchError::NoStart(format!(
        "{} is not an executable file",
        path.display()
    )))
}

impl<S: Spawner> StepLauncher for SystemdLauncher<S> {
    fn run(&self, call: &StepCall<'_>) -> Result<StepExit, LaunchError> {
        let args = command(call)?;
        executable(call.executable)?;
        self.run_unit(call.unit, &args)
    }

    /// Runs `<executable> <hook_args...> <step> --params <params>` ([`additive_command`]) as
    /// [`StepLauncher::run`] runs a step.
    fn run_additive(&self, call: &AdditiveCall<'_>) -> Result<StepExit, LaunchError> {
        let args = additive_command(call)?;
        executable(call.executable)?;
        self.run_unit(call.unit, &args)
    }

    fn stop(&self, unit: &str) -> Result<(), LaunchError> {
        // Marked before the stop starts, so that the run it ends finds the mark when
        // `systemd-run` returns.
        stops(&self.stopped_units).insert(unit.to_owned());
        let captured = self.spawn(Program::Systemctl, &stop_command(unit))?;
        stopped(unit, &captured)
    }
}
