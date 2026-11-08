//! The [`StepLauncher`] of an OpenRC host: each step of a hook is a child process of the daemon,
//! in a process group of its own, under the same contract as `hostkeeper_steps::SystemdLauncher`
//! (docs/design/two-programs.md, section 4.6). The process group is there for the kill of the
//! step and everything it started, and the pipes for the capture; it confines nothing.
//!
//! systemd gives `SystemdLauncher` three things for free that OpenRC does not have: a transient
//! unit with a name to stop it by, `RuntimeMaxSec=` to end a step at its bound, and the control
//! group of the unit, which is killed whole at the unit's end. This launcher makes up for each:
//!
//! - **The process group.** A step is started with `Command` as `<executable> <args...>` (the
//!   arguments `SystemdLauncher` runs inside its unit), with an empty environment but `PATH`
//!   (what a systemd unit gets by default), `/` as its directory, no input, and both of its
//!   output streams read to their end on a thread each into a [`Ring`]. It is its own process
//!   group leader, so that the whole group can be signalled by the leader's pid.
//! - **The bound.** A [`Bound::Secs`] step still running when that many seconds have passed on
//!   the injected [`Clock`] since it started gets `SIGTERM` to its process group, and `SIGKILL`
//!   to the group if the leader has not ended [`STOP_TIMEOUT_SECS`] later on the clock (systemd's
//!   `DefaultTimeoutStopSec=`). A step ended by its bound reports no exit code, a failed step,
//!   whatever it exited with after the `SIGTERM`. A [`Bound::Unbounded`] step is never ended by
//!   the launcher. The clock is read once every [`POLL`] while a step runs.
//! - **The group's end.** When the leader has ended, however it ended, the rest of its process
//!   group gets `SIGKILL`, as systemd kills the control group of a unit at its end; a group that
//!   is no longer there is not an error. The kill follows the reaping of the leader, so a group
//!   that ended before it is signalled by a number that may have been given to another process;
//!   a process id comes round only after the kernel has used all the others.
//! - **The pipes.** A descendant that left the process group holds a pipe open for as long as it
//!   likes. After the leader ended and its group was killed, the launcher waits for the readers
//!   at most [`READER_GRACE`] (settable with [`OpenrcLauncher::with_reader_grace`]), then takes
//!   what they have kept and leaves them to end with the descendant.
//! - **The name.** The units this launcher runs are kept in memory, and each run writes a file
//!   `<steps_dir>/<unit>` (the directory made `0700` when missing) holding
//!   `<pid> <start time>`, the leader's process id and field 22 of its `/proc/<pid>/stat`, until
//!   it ends. A run that cannot write it is killed and fails with [`LaunchError::Launcher`].
//!
//! [`StepLauncher::run_additive`] (the hook's `uninstall`, or a `[hook] boot` step) runs
//! `<executable> <hook_args...> <step> --params <params>` as a step is run, bounded by its
//! `timeout_secs` (0 is no bound). [`StepLauncher::run_probe`] runs `<executable> <args...>` as
//! one process too, bounded by `timeout_secs` (0 is read as 1: a probe is never unbounded) and
//! read as [`ProbeExit::from_captured`] reads it. The call of each is checked as
//! `SystemdLauncher` checks it: an absolute executable and params file ([`LaunchError::Launcher`]
//! otherwise), then an executable that is a file with an execute bit ([`LaunchError::NoStart`]
//! otherwise); a process that cannot be started is [`LaunchError::NoStart`] too. The log, if any,
//! is given the unit and what was captured after each run.
//!
//! [`StepLauncher::stop`] is called by the dead-man from its own thread while [`run`] waits:
//!
//! - a unit this launcher is running gets `SIGTERM` to its process group, and `SIGKILL` at
//!   [`STOP_TIMEOUT_SECS`] on the clock; `stop` returns once the run has ended. A run that a
//!   stop ended reports no exit code even if it exited 0, and a stop of an earlier run under the
//!   same name says nothing of a later run;
//! - a unit this launcher is not running is looked for in its file, which a daemon started again
//!   after a crash finds written by the daemon before it. If the process is there with the start
//!   time the file holds and is not a zombie, its group gets `SIGTERM`, and `SIGKILL` at
//!   [`STOP_TIMEOUT_SECS`], and `stop` returns once the process is gone or a zombie, after one
//!   more `SIGKILL` to the rest of the group. A missing file, a process that is gone, one that
//!   is a zombie and one whose start time differs (its pid was given to another process) are a
//!   stop already done, and the file is removed. A file that cannot be read or does not hold a
//!   process id and a start time is [`LaunchError::Launcher`].
//!
//! What this launcher cannot do, and a systemd unit can: a descendant that leaves the process
//! group (`setsid`, `setpgid`) is not killed with it. And unlike a systemd unit the step is not
//! bounded by itself: if the daemon is killed while a bounded step runs, the step runs on until
//! the daemon is back, finds the step `running` in its record and stops it by name (resume
//! stops a unit found running before it runs the step again), as on a Mac; or until it next
//! writes to its standard output or error, whose reading end was the daemon's (`SIGPIPE`).
//!
//! The decisions of the launcher are in methods and functions that are not generic, so each is
//! measured once.
//!
//! [`run`]: StepLauncher::run

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::num::NonZeroU64;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use hostkeeper_steps::{
    AdditiveCall, Bound, Captured, Clock, LaunchError, ProbeCall, ProbeExit, ProcBootId, Ring,
    StepCall, StepExit, StepLauncher, StepLog, SystemClock,
};
use rustix::process::{Pid, Signal, kill_process_group};

/// Where the launcher keeps a file per running step: its process group, so that a daemon started
/// again can stop a step an earlier one left running.
pub const STEPS_DIR: &str = "/run/hostkeeper/steps";
/// How long after a `SIGTERM` the launcher waits for the process to end, on its clock, before it
/// sends `SIGKILL`: systemd's `DefaultTimeoutStopSec=`.
pub const STOP_TIMEOUT_SECS: u64 = 90;
/// How long the launcher waits for the output of a step to end, after the step's process group
/// ended, before it takes what was read and leaves the readers.
pub const READER_GRACE: Duration = Duration::from_secs(5);
/// How often the launcher looks at a running step, and at a process it stops, in real time.
pub const POLL: Duration = Duration::from_millis(50);
/// The `PATH` of a step: what a systemd unit gets by default.
const STEP_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
/// Where the kernel keeps what it knows of the processes.
const PROC: &str = "/proc";

/// The [`StepLauncher`] of an OpenRC host.
pub struct OpenrcLauncher {
    steps_dir: PathBuf,
    proc_root: PathBuf,
    clock: Arc<dyn Clock>,
    log: Option<StepLog>,
    reader_grace: Duration,
    runs: Mutex<Runs>,
    /// Told whenever a run ends, for the stop that waits for it.
    ended: Condvar,
}

/// The runs of a launcher.
#[derive(Default)]
struct Runs {
    /// The units being run, and the process group of each.
    running: BTreeMap<String, Pid>,
    /// The units of `running` that [`StepLauncher::stop`] was asked to stop: the run that is
    /// under way reports no exit code. A run takes its unit out of both sets when it ends, so a
    /// stop of one run says nothing of a later run under the same name.
    stopped: BTreeSet<String>,
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
            proc_root: PathBuf::from(PROC),
            clock,
            log: None,
            reader_grace: READER_GRACE,
            runs: Mutex::default(),
            ended: Condvar::new(),
        }
    }

    /// The same launcher, giving `log` the output of every step it ran.
    pub fn with_log(mut self, log: StepLog) -> OpenrcLauncher {
        self.log = Some(log);
        self
    }

    /// The same launcher, waiting `grace` instead of [`READER_GRACE`] for the output of a step
    /// whose process group has ended.
    pub fn with_reader_grace(mut self, grace: Duration) -> OpenrcLauncher {
        self.reader_grace = grace;
        self
    }

    /// The same launcher, reading what it knows of a process from `<proc_root>/<pid>/stat`
    /// instead of `/proc`.
    pub fn with_proc_root(mut self, proc_root: &Path) -> OpenrcLauncher {
        self.proc_root = proc_root.to_path_buf();
        self
    }
}

/// The locked value of `mutex`. A poisoned lock is still read: the runs hold only names and ids,
/// and a ring only bytes.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Sends `signal` to the process group `pgid`. A group that is gone (`ESRCH`) has nothing left to
/// signal, and a group the launcher may not signal is one it cannot do more for: either way the
/// caller goes on to wait for the leader, so the result is not read.
fn signal(pgid: Pid, signal: Signal) {
    let _ = kill_process_group(pgid, signal);
}

/// What a running process group is owed, as time passes on the clock.
#[derive(Clone, Copy, Debug)]
enum Watch {
    /// Not signalled: at this second on the clock the bound is over, `None` for no bound.
    Running(Option<u64>),
    /// Sent `SIGTERM`: at this second `SIGKILL` follows.
    Term(u64),
    /// Sent `SIGKILL`: nothing more to send.
    Killed,
}

impl Watch {
    /// The watch of a step that started at second `started`.
    fn new(started: u64, bound: Bound) -> Watch {
        Watch::Running(match bound {
            Bound::Secs(secs) => Some(started.saturating_add(secs.get())),
            Bound::Unbounded => None,
        })
    }

    /// The watch of a process that was sent `SIGTERM` at second `now`.
    fn stopping(now: u64) -> Watch {
        Watch::Term(now.saturating_add(STOP_TIMEOUT_SECS))
    }

    /// The signal owed at second `now`, if any; it counts as sent.
    fn due(&mut self, now: u64) -> Option<Signal> {
        match *self {
            Watch::Running(Some(at)) if now >= at => {
                *self = Watch::stopping(now);
                Some(Signal::TERM)
            }
            Watch::Term(at) if now >= at => {
                *self = Watch::Killed;
                Some(Signal::KILL)
            }
            _ => None,
        }
    }

    /// Whether the bound was reached: the step was sent `SIGTERM`, and maybe `SIGKILL` after.
    fn bounded(&self) -> bool {
        !matches!(self, Watch::Running(_))
    }
}

/// `Ok` when `unit` can be a file name in the steps directory: not empty, no `/`, not starting
/// with `.`.
fn unit_file(unit: &str) -> Result<(), LaunchError> {
    if unit.is_empty() || unit.contains('/') || unit.starts_with('.') {
        return Err(LaunchError::Launcher(format!(
            "{unit:?} is not a unit name: it is empty, holds a `/` or starts with `.`"
        )));
    }
    Ok(())
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

/// What the executable of a step or an additive step is, as a refusal names it.
const HOOK: &str = "the hook executable";

/// One run of a program: what [`StepCall`], [`AdditiveCall`] and [`ProbeCall`] have in common.
struct Program<'a> {
    /// The unit the run is named after.
    unit: &'a str,
    /// What the executable is, as a refusal names it.
    what: &'static str,
    executable: &'a Path,
    /// The arguments after it.
    args: Vec<OsString>,
    /// The params file the arguments name, if any.
    params: Option<&'a Path>,
    bound: Bound,
}

impl<'a> Program<'a> {
    fn step(call: &'a StepCall<'a>) -> Program<'a> {
        Program {
            unit: call.unit,
            what: HOOK,
            executable: call.executable,
            args: call.invocation.to_args(),
            params: call.invocation.params.as_deref(),
            bound: call.bound,
        }
    }

    /// `<hook_args...> <step> --params <params>`, bounded by `timeout_secs` (0 is no bound).
    fn additive(call: &'a AdditiveCall<'a>) -> Program<'a> {
        let mut args: Vec<OsString> = call.hook_args.iter().map(OsString::from).collect();
        args.push(OsString::from(call.step));
        args.push(OsString::from("--params"));
        args.push(call.params.as_os_str().to_owned());
        Program {
            unit: call.unit,
            what: HOOK,
            executable: call.executable,
            args,
            params: Some(call.params),
            bound: NonZeroU64::new(call.timeout_secs).map_or(Bound::Unbounded, Bound::Secs),
        }
    }

    /// `<args...>`, bounded by `timeout_secs`, `0` read as `1` (a probe is never unbounded).
    fn probe(call: &'a ProbeCall<'a>) -> Program<'a> {
        Program {
            unit: call.unit,
            what: "the probed binary",
            executable: call.executable,
            args: call.args.iter().map(OsString::from).collect(),
            params: None,
            bound: Bound::Secs(NonZeroU64::MIN.saturating_add(call.timeout_secs.saturating_sub(1))),
        }
    }

    /// `Ok` when the run can be started: the executable and params file are absolute paths (the
    /// process does not run in the daemon's directory), the unit can be a file name, and the
    /// executable is an executable file.
    fn check(&self) -> Result<(), LaunchError> {
        if !self.executable.is_absolute() {
            return Err(LaunchError::Launcher(format!(
                "{} {:?} is not an absolute path",
                self.what, self.executable
            )));
        }
        if let Some(params) = self.params.filter(|p| !p.is_absolute()) {
            return Err(LaunchError::Launcher(format!(
                "the params file {params:?} is not an absolute path"
            )));
        }
        unit_file(self.unit)?;
        executable(self.executable)
    }
}

/// The output of a process, read on two threads into the rings they share.
struct Readers {
    done: mpsc::Receiver<()>,
    stdout: Arc<Mutex<Ring>>,
    stderr: Arc<Mutex<Ring>>,
}

/// A ring several threads write to.
struct Shared(Arc<Mutex<Ring>>);

impl Write for Shared {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        lock(&self.0).push(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Readers {
    /// Reads `stdout` and `stderr` to their end, on a thread each.
    fn start(stdout: io::PipeReader, stderr: io::PipeReader) -> Readers {
        let (sender, done) = mpsc::channel();
        let (kept_out, kept_err) = (Arc::default(), Arc::default());
        for (mut pipe, ring) in [(stdout, &kept_out), (stderr, &kept_err)] {
            let (sender, mut sink) = (sender.clone(), Shared(Arc::clone(ring)));
            std::thread::spawn(move || {
                // What was read before a read error is kept.
                let _ = io::copy(&mut pipe, &mut sink);
                // `io::copy` does not flush; a sink is flushed once its stream is done.
                let _ = sink.flush();
                // The launcher may have stopped waiting.
                let _ = sender.send(());
            });
        }
        Readers {
            done,
            stdout: kept_out,
            stderr: kept_err,
        }
    }

    /// What the readers have kept once both have reached the end of their pipe, or `grace` has
    /// passed.
    fn collect(self, grace: Duration) -> (Ring, Ring) {
        let deadline = Instant::now() + grace;
        for _ in 0..2 {
            let left = deadline.saturating_duration_since(Instant::now());
            if self.done.recv_timeout(left).is_err() {
                break;
            }
        }
        (lock(&self.stdout).clone(), lock(&self.stderr).clone())
    }
}

/// Starts `executable` with `args` in a process group of its own, as the module documentation
/// says, and the readers of its output.
fn spawn(executable: &Path, args: &[OsString]) -> io::Result<(Child, Readers)> {
    let (out_pipe, out_end) = io::pipe()?;
    let (err_pipe, err_end) = io::pipe()?;
    // The command holds the writing ends; it is dropped with this block, so that the pipes end
    // when the process and what it started have closed theirs.
    let child = {
        let mut command = Command::new(executable);
        command
            .args(args)
            .env_clear()
            .env("PATH", STEP_PATH)
            .current_dir("/")
            .stdin(Stdio::null())
            .stdout(out_end)
            .stderr(err_end)
            .process_group(0);
        command.spawn()?
    };
    Ok((child, Readers::start(out_pipe, err_pipe)))
}

/// What a `/proc/<pid>/stat` says of a process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Proc {
    /// Field 3: `R`, `S`, `Z` for a zombie, and so on.
    state: char,
    /// Field 22: when the process started, in clock ticks after the boot.
    start: u64,
}

/// The state and start time in the text of a `/proc/<pid>/stat`. The second field, the name of
/// the program, is in parentheses and may hold anything, so the fields are counted from the last
/// `)`.
fn parse_stat(text: &str) -> Option<Proc> {
    let rest = text.get(text.rfind(')')? + 1..)?;
    let mut fields = rest.split_whitespace();
    let state = fields.next()?.chars().next()?;
    // Fields 4 to 21 lie between the state and the start time.
    let start = fields.nth(18)?.parse().ok()?;
    Some(Proc { state, start })
}

/// The process id and start time a step's file holds: `<pid> <start time>`.
fn parse_record(text: &str) -> Option<(Pid, u64)> {
    let mut words = text.split_whitespace();
    // `Pid::from_raw` panics on a negative number, so the number is read unsigned first.
    let pid: u32 = words.next()?.parse().ok()?;
    let pid = Pid::from_raw(i32::try_from(pid).ok()?)?;
    let start = words.next()?.parse().ok()?;
    words.next().is_none().then_some((pid, start))
}

/// Whether `found` is the process that started at `start` and has not ended: it exists, was not
/// given the id of another process, and is not a zombie.
fn alive(found: Option<Proc>, start: u64) -> bool {
    matches!(found, Some(p) if p.start == start && p.state != 'Z')
}

impl OpenrcLauncher {
    /// What `/proc` says of the process `pid`; `None` when it is not there.
    fn read_proc(&self, pid: Pid) -> Result<Option<Proc>, LaunchError> {
        let path = self
            .proc_root
            .join(pid.as_raw_pid().to_string())
            .join("stat");
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(LaunchError::Launcher(format!(
                    "cannot read {}: {e}",
                    path.display()
                )));
            }
        };
        parse_stat(&text).map(Some).ok_or_else(|| {
            LaunchError::Launcher(format!("{} is not a process's stat", path.display()))
        })
    }

    /// Writes the file of the run of `unit` whose leader is `pgid`.
    fn record(&self, unit: &str, pgid: Pid) -> Result<(), LaunchError> {
        let file = self.steps_dir.join(unit);
        let failed =
            |e: io::Error| LaunchError::Launcher(format!("cannot write {}: {e}", file.display()));
        let process = self.read_proc(pgid)?.ok_or_else(|| {
            LaunchError::Launcher(format!(
                "the step's process {} is not in {}",
                pgid.as_raw_pid(),
                self.proc_root.display()
            ))
        })?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.steps_dir)
            .map_err(&failed)?;
        fs::write(&file, format!("{} {}\n", pgid.as_raw_pid(), process.start)).map_err(&failed)
    }

    /// Looks at the leader every [`POLL`] until it has ended, sending the signals the bound
    /// owes. Whether the bound was reached.
    fn supervise(&self, child: &mut Child, unit: &str, bound: Bound) -> Result<bool, LaunchError> {
        let pgid = Pid::from_child(child);
        self.record(unit, pgid)?;
        let mut watch = Watch::new(self.clock.mono_secs(), bound);
        while child.try_wait()?.is_none() {
            if let Some(owed) = watch.due(self.clock.mono_secs()) {
                signal(pgid, owed);
            }
            std::thread::sleep(POLL);
        }
        Ok(watch.bounded())
    }

    /// Runs `program` as one process and waits for the end of its group and its output (the
    /// module documentation). Whether a stop ended the run is the second of the result. The log
    /// (if any) is given what was captured.
    fn launch(&self, program: &Program<'_>) -> Result<(Captured, bool), LaunchError> {
        program.check()?;
        let unit = program.unit;
        let (mut child, readers) = {
            let mut runs = lock(&self.runs);
            if runs.running.contains_key(unit) {
                return Err(LaunchError::Launcher(format!("{unit} is already running")));
            }
            let started = spawn(program.executable, &program.args).map_err(|e| {
                LaunchError::NoStart(format!("{}: {e}", program.executable.display()))
            })?;
            runs.running
                .insert(unit.to_owned(), Pid::from_child(&started.0));
            started
        };
        let pgid = Pid::from_child(&child);
        let outcome = self.supervise(&mut child, unit, program.bound);
        // The leader has ended or the run failed: what is left of its group goes, and the
        // leader, when the run failed, is reaped.
        signal(pgid, Signal::KILL);
        let status = child.wait();
        let (stdout, stderr) = readers.collect(self.reader_grace);
        // The file is removed whatever happened; one left behind is harmless, as its start time
        // names a process that is gone.
        let _ = fs::remove_file(self.steps_dir.join(unit));
        let was_stopped = {
            let mut runs = lock(&self.runs);
            runs.running.remove(unit);
            runs.stopped.remove(unit)
        };
        self.ended.notify_all();
        let bounded = outcome?;
        let captured = Captured {
            code: status?.code().filter(|_| !bounded),
            stdout,
            stderr,
        };
        if let Some(log) = self.log.as_ref() {
            log(unit, &captured);
        }
        Ok((captured, was_stopped))
    }

    /// Runs a step or an additive step.
    fn run_step(&self, program: &Program<'_>) -> Result<StepExit, LaunchError> {
        let (captured, was_stopped) = self.launch(program)?;
        Ok(StepExit {
            // A step that `stop` ended has no exit code, whatever it exited with (the module
            // documentation); a step ended by its bound has none already.
            code: captured.code.filter(|_| !was_stopped),
            last_line: captured.stdout.last_line(),
        })
    }

    /// Stops the run of `unit` whose process group is `pgid`: `SIGTERM`, and `SIGKILL` at the
    /// stop timeout, until the run has ended.
    fn stop_running(&self, unit: &str, pgid: Pid) {
        signal(pgid, Signal::TERM);
        let mut watch = Watch::stopping(self.clock.mono_secs());
        let mut runs = lock(&self.runs);
        // A later run under the same name has another process group.
        while runs.running.get(unit) == Some(&pgid) {
            if let Some(owed) = watch.due(self.clock.mono_secs()) {
                signal(pgid, owed);
            }
            runs = self
                .ended
                .wait_timeout(runs, POLL)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Stops the process the file of `unit` names, if it is still running.
    fn stop_recorded(&self, unit: &str) -> Result<(), LaunchError> {
        let file = self.steps_dir.join(unit);
        let text = match fs::read_to_string(&file) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                return Err(LaunchError::Launcher(format!(
                    "cannot read {}: {e}",
                    file.display()
                )));
            }
        };
        let (pid, start) = parse_record(&text).ok_or_else(|| {
            LaunchError::Launcher(format!(
                "{} does not hold a process id and a start time",
                file.display()
            ))
        })?;
        if alive(self.read_proc(pid)?, start) {
            signal(pid, Signal::TERM);
            let mut watch = Watch::stopping(self.clock.mono_secs());
            while alive(self.read_proc(pid)?, start) {
                if let Some(owed) = watch.due(self.clock.mono_secs()) {
                    signal(pid, owed);
                }
                std::thread::sleep(POLL);
            }
            // The leader is gone; what it started in its group goes with it.
            signal(pid, Signal::KILL);
        }
        // A file that cannot be removed is read again by the next stop, and names a process
        // that is gone.
        let _ = fs::remove_file(&file);
        Ok(())
    }
}

impl StepLauncher for OpenrcLauncher {
    fn run(&self, call: &StepCall<'_>) -> Result<StepExit, LaunchError> {
        self.run_step(&Program::step(call))
    }

    /// Runs `<executable> <hook_args...> <step> --params <params>` as [`StepLauncher::run`]
    /// runs a step, bounded by `timeout_secs`.
    fn run_additive(&self, call: &AdditiveCall<'_>) -> Result<StepExit, LaunchError> {
        self.run_step(&Program::additive(call))
    }

    /// Runs `<executable> <args...>` as [`StepLauncher::run`] runs a step, bounded by
    /// `timeout_secs` (`0` is 1 second), and reads its exit as `SystemdLauncher` does
    /// ([`ProbeExit::from_captured`]).
    fn run_probe(&self, call: &ProbeCall<'_>) -> Result<ProbeExit, LaunchError> {
        let (captured, _) = self.launch(&Program::probe(call))?;
        Ok(ProbeExit::from_captured(&captured, call.output_bytes))
    }

    fn stop(&self, unit: &str) -> Result<(), LaunchError> {
        unit_file(unit)?;
        let running = {
            let mut runs = lock(&self.runs);
            let pgid = runs.running.get(unit).copied();
            // Marked in the same hold of the lock that found the run, before the signal is
            // sent, so that the run finds the mark when it ends.
            if pgid.is_some() {
                runs.stopped.insert(unit.to_owned());
            }
            pgid
        };
        match running {
            Some(pgid) => {
                self.stop_running(unit, pgid);
                Ok(())
            }
            None => self.stop_recorded(unit),
        }
    }
}
