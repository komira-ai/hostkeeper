//! The [`StepLauncher`] of a Mac: each step of a hook is a transient launchd job in the system
//! domain, run as root, under the same contract as `hostkeeper_steps::SystemdLauncher`
//! (docs/design/hostkeeper-design.md, sections 5.3 and 5.9). The job is there for the kill of the
//! step's process group, and its output files for the capture; it confines nothing.
//!
//! One run of a step is:
//!
//! 1. the call is checked (absolute executable and params file, a unit name that is a step
//!    unit, an executable file), as `SystemdLauncher` checks it;
//! 2. a job of the same label left loaded by an earlier run is booted out;
//! 3. `<dir>/<label>/` gets the step's plist ([`step_plist`]) and two empty output files,
//!    `stdout` and `stderr`, which launchd appends to;
//! 4. `launchctl bootstrap system <dir>/<label>/<label>.plist` loads the job, and `RunAtLoad`
//!    starts it;
//! 5. `launchctl print system/<label>` is read every [`POLL`] until the job has ended (it has a
//!    last exit code or a terminating signal and no process) or is no longer loaded (it was
//!    stopped). launchd has no counterpart of `RuntimeMaxSec=`: a [`Bound::Secs`] step still
//!    running at its bound is booted out by the launcher, and reported with no exit code, a
//!    failed step. A [`Bound::Unbounded`] step is never booted out by the launcher;
//! 6. the last [`hostkeeper_steps::RING_BYTES`] of each output file is read into a [`Ring`],
//!    the log (if any) is given them, the job is booted out (unloaded, as `--collect` does for a
//!    systemd unit) and its directory removed.
//!
//! [`StepLauncher::stop`] is [`bootout`]: a label that is not loaded is stopped already, so a
//! second stop, or a stop of a step that has ended, is not an error. `launchctl bootout` waits
//! for the job to end; launchd sends `SIGTERM`, then `SIGKILL` to the job's process group after
//! the job's exit timeout (the plist sets none: launchd's default).
//!
//! Time is read through [`Pace`], so a test drives the polling with a clock that moves only when
//! the launcher waits. The launcher's decisions are in functions that are not generic, run the
//! same for every [`Launchctl`] and [`Pace`].
//!
//! A failure of `launchctl` once the job is written (a load, a read, or a boot-out at the bound
//! that fails) is a [`LaunchError::Launcher`]; before it is returned the job is booted out once
//! more, its result not read, and its directory removed, since nothing else bounds a job the
//! launcher no longer reads.
//!
//! Unlike a systemd unit, the job is not bounded by itself: if the updater dies while a bounded
//! step runs, the job runs until the updater is back, finds the step `running` in its record and
//! stops it by name (resume stops a unit found running before it runs the step again).

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use hostkeeper_steps::{
    Bound, Captured, LaunchError, Ring, StepCall, StepExit, StepLauncher, StepLog,
};

use crate::label::Label;
use crate::launchctl::{
    JobInfo, JobState, JobStatus, Launchctl, LaunchctlError, LaunchctlOutput, bootout,
    bootout_args, bootstrap_args, print,
};
use crate::plist::{StepJob, step_plist};

/// Where the jobs of the steps are kept while they run: one directory per label.
pub const STEP_JOBS_DIR: &str = "/var/run/hostkeeper/steps";
/// How often a running step's job is read.
pub const POLL: Duration = Duration::from_millis(500);
/// The name of the file a job's standard output goes to, in the job's directory.
pub const STDOUT_FILE: &str = "stdout";
/// The name of the file a job's standard error goes to, in the job's directory.
pub const STDERR_FILE: &str = "stderr";

/// Time as an input: the launcher reads how long a step has run, and waits between two reads of
/// its job, through this.
pub trait Pace: Send + Sync {
    /// The time on a monotonic clock since an origin of the pace's own.
    fn elapsed(&self) -> Duration;
    /// Waits for `wait`.
    fn sleep(&self, wait: Duration);
}

/// The [`Launchctl`] the launcher holds, as a sized value for the functions of
/// [`crate::launchctl`].
struct Port<'a>(&'a dyn Launchctl);

impl Launchctl for Port<'_> {
    fn run(&self, args: &[String]) -> Result<LaunchctlOutput, LaunchctlError> {
        self.0.run(args)
    }
}

/// The [`StepLauncher`] of a Mac: one transient launchd job per step, through a [`Launchctl`].
pub struct LaunchdLauncher {
    launchctl: Box<dyn Launchctl>,
    pace: Box<dyn Pace>,
    jobs_dir: PathBuf,
    log: Option<StepLog>,
}

/// The files of one step's job, in `<jobs_dir>/<label>/`.
struct JobFiles {
    dir: PathBuf,
    plist: PathBuf,
    stdout: PathBuf,
    stderr: PathBuf,
}

impl JobFiles {
    fn of(jobs_dir: &Path, label: &Label) -> JobFiles {
        let dir = jobs_dir.join(label.as_str());
        JobFiles {
            plist: dir.join(format!("{label}.plist")),
            stdout: dir.join(STDOUT_FILE),
            stderr: dir.join(STDERR_FILE),
            dir,
        }
    }
}

impl LaunchdLauncher {
    /// A launcher that runs `launchctl` through `launchctl`, reads time through `pace`, and keeps
    /// the jobs of the steps in `jobs_dir` (made when missing).
    pub fn new(
        launchctl: impl Launchctl + 'static,
        pace: impl Pace + 'static,
        jobs_dir: &Path,
    ) -> LaunchdLauncher {
        LaunchdLauncher {
            launchctl: Box::new(launchctl),
            pace: Box::new(pace),
            jobs_dir: jobs_dir.to_owned(),
            log: None,
        }
    }

    /// The same launcher, giving `log` the output of every step it ran (the kept standard error
    /// is the hook's log), as `SystemdLauncher::with_log` does.
    pub fn with_log(mut self, log: StepLog) -> LaunchdLauncher {
        self.log = Some(log);
        self
    }

    fn port(&self) -> Port<'_> {
        Port(self.launchctl.as_ref())
    }

    /// Boots `label` out; a label that is not loaded is done.
    fn boot_out(&self, label: &Label) -> Result<(), LaunchError> {
        bootout(&self.port(), label)
            .map_err(|e| LaunchError::Launcher(format!("cannot stop the job {label}: {e}")))
    }

    /// Loads the job of `plist`.
    fn bootstrap(&self, label: &Label, plist: &Path) -> Result<(), LaunchError> {
        let args = bootstrap_args(plist);
        let out = self
            .launchctl
            .run(&args)
            .map_err(|e| LaunchError::Launcher(format!("cannot load the job {label}: {e}")))?;
        bootstrapped(label, &out)
    }

    /// Reads the job until it has ended or is gone, booting it out at the bound. The exit code
    /// of the step: `None` when it was stopped, by the bound or by [`StepLauncher::stop`], or
    /// ended by a signal.
    fn wait(&self, label: &Label, bound: Bound) -> Result<Option<i32>, LaunchError> {
        let started = self.pace.elapsed();
        loop {
            let status = print(&self.port(), label)
                .map_err(|e| LaunchError::Launcher(format!("cannot read the job {label}: {e}")))?;
            let ran = self.pace.elapsed().saturating_sub(started);
            match poll(&status, bound, ran) {
                Poll::Ended(code) => return Ok(code),
                Poll::Overran => {
                    self.boot_out(label)?;
                    return Ok(None);
                }
                Poll::Running => self.pace.sleep(POLL),
            }
        }
    }
}

/// What one read of a running step's job decides.
enum Poll {
    /// The step has ended, with this exit code.
    Ended(Option<i32>),
    /// The step is still running past its bound: it is to be booted out.
    Overran,
    /// The step is running within its bound: read again after [`POLL`].
    Running,
}

/// What `status`, read after the step has run for `ran`, decides. A job that is no longer loaded
/// was stopped: no code. A job that has no process and has a last exit code or a terminating
/// signal has ended: its code, or none for a signal.
fn poll(status: &JobStatus, bound: Bound, ran: Duration) -> Poll {
    match status {
        JobStatus::NotLoaded => Poll::Ended(None),
        JobStatus::Loaded(info) if ended(info) => Poll::Ended(
            info.last_exit_status
                .filter(|_| info.last_terminating_signal.is_none()),
        ),
        JobStatus::Loaded(_) => match bound {
            Bound::Secs(secs) if ran >= Duration::from_secs(secs.get()) => Poll::Overran,
            _ => Poll::Running,
        },
    }
}

/// Whether a loaded job has run and ended: no process, and an exit code or a signal.
fn ended(info: &JobInfo) -> bool {
    let busy = matches!(
        info.state,
        JobState::Running | JobState::SpawnScheduled | JobState::Spawning
    );
    let done = info.last_exit_status.is_some() | info.last_terminating_signal.is_some();
    !busy & done
}

/// `Ok` when `launchctl bootstrap` exited 0.
fn bootstrapped(label: &Label, out: &LaunchctlOutput) -> Result<(), LaunchError> {
    match out.code {
        Some(0) => Ok(()),
        code => Err(LaunchError::Launcher(format!(
            "launchctl bootstrap of the job {label} exited with {code:?}: {}",
            out.stderr_text().trim()
        ))),
    }
}

/// The job that runs `call` under `label`, its output in `files`. A relative executable or
/// params file is refused: the job does not run in the updater's directory.
fn step_job(call: &StepCall<'_>, label: Label, files: &JobFiles) -> Result<StepJob, LaunchError> {
    if !call.executable.is_absolute() {
        return Err(LaunchError::Launcher(format!(
            "the hook executable {:?} is not an absolute path",
            call.executable
        )));
    }
    if let Some(params) = call.invocation.params.as_ref().filter(|p| !p.is_absolute()) {
        return Err(LaunchError::Launcher(format!(
            "the params file {params:?} is not an absolute path"
        )));
    }
    Ok(StepJob {
        label,
        program: call.executable.to_owned(),
        args: call.invocation.to_args(),
        stdout: files.stdout.clone(),
        stderr: files.stderr.clone(),
    })
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

/// A new file at `path`, empty, readable and writable by its owner only.
fn fresh(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
}

/// Writes the job's directory: the plist and the two empty output files, so that nothing an
/// earlier run of the label wrote is read as this run's.
fn write_job(files: &JobFiles, plist: &str) -> io::Result<()> {
    fs::create_dir_all(&files.dir)?;
    fs::set_permissions(&files.dir, fs::Permissions::from_mode(0o700))?;
    fresh(&files.stdout)?;
    fresh(&files.stderr)?;
    io::Write::write_all(&mut fresh(&files.plist)?, plist.as_bytes())
}

/// The last [`hostkeeper_steps::RING_BYTES`] of the file at `path`. A file that cannot be read
/// keeps nothing: the step's report is then `{}`, and its exit code decides.
fn tail(path: &Path) -> Ring {
    let mut ring = Ring::new();
    if let Ok(mut file) = File::open(path) {
        // What was read before an error is kept.
        let _ = io::copy(&mut file, &mut ring);
    }
    ring
}

/// How a step ended, after `log` (if any) was given what was captured.
fn step_exit(log: Option<&StepLog>, unit: &str, captured: &Captured) -> StepExit {
    if let Some(log) = log {
        log(unit, captured);
    }
    StepExit {
        code: captured.code,
        last_line: captured.stdout.last_line(),
    }
}

impl StepLauncher for LaunchdLauncher {
    fn run(&self, call: &StepCall<'_>) -> Result<StepExit, LaunchError> {
        let label =
            Label::from_unit(call.unit).map_err(|e| LaunchError::Launcher(e.to_string()))?;
        let files = JobFiles::of(&self.jobs_dir, &label);
        let job = step_job(call, label.clone(), &files)?;
        executable(call.executable)?;
        let plist = step_plist(&job).map_err(|e| LaunchError::Launcher(e.to_string()))?;
        self.boot_out(&label)?;
        write_job(&files, &plist).map_err(|e| {
            LaunchError::Launcher(format!("cannot write the job {}: {e}", files.dir.display()))
        })?;
        let code = self
            .bootstrap(&label, &files.plist)
            .and_then(|()| self.wait(&label, call.bound))
            .inspect_err(|_| {
                // The step's end is unknown, and nothing else would bound a job the launcher
                // can no longer read: it is booted out, once, as is, and its files go. What
                // this does not stop, resume stops by name.
                let _ = self.launchctl.run(&bootout_args(&label));
                let _ = fs::remove_dir_all(&files.dir);
            })?;
        let captured = Captured {
            code,
            stdout: tail(&files.stdout),
            stderr: tail(&files.stderr),
        };
        // The step has ended and what it wrote is read: the job is unloaded so that its label
        // can be loaded again, and its files go. Neither changes how the step ended; a job that
        // could not be unloaded is booted out by the next run of its label.
        let _ = self.launchctl.run(&bootout_args(&label));
        let _ = fs::remove_dir_all(&files.dir);
        Ok(step_exit(self.log.as_ref(), call.unit, &captured))
    }

    fn stop(&self, unit: &str) -> Result<(), LaunchError> {
        let label = Label::from_unit(unit).map_err(|e| LaunchError::Launcher(e.to_string()))?;
        self.boot_out(&label)
    }
}

/// The host's adapters: `/bin/launchctl`, the monotonic clock, and [`STEP_JOBS_DIR`].
#[cfg(target_os = "macos")]
mod host {
    use std::path::Path;
    use std::time::{Duration, Instant};

    use super::{LaunchdLauncher, Pace, STEP_JOBS_DIR};
    use crate::launchctl::LaunchctlProcess;

    /// The deadline of one `launchctl` command the launcher runs. `bootout` waits for the job
    /// to end, which takes up to launchd's exit timeout before its `SIGKILL`.
    const LAUNCHCTL_DEADLINE: Duration = Duration::from_secs(60);

    /// The host's monotonic clock and `thread::sleep`.
    #[derive(Debug)]
    pub struct SystemPace(Instant);

    impl Pace for SystemPace {
        fn elapsed(&self) -> Duration {
            self.0.elapsed()
        }

        fn sleep(&self, wait: Duration) {
            std::thread::sleep(wait);
        }
    }

    impl LaunchdLauncher {
        /// The launcher of the host: the system's `launchctl`, its clock, and
        /// [`STEP_JOBS_DIR`].
        pub fn system() -> LaunchdLauncher {
            LaunchdLauncher::new(
                LaunchctlProcess::system(LAUNCHCTL_DEADLINE),
                SystemPace(Instant::now()),
                Path::new(STEP_JOBS_DIR),
            )
        }
    }
}

#[cfg(target_os = "macos")]
pub use host::SystemPace;
