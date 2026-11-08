//! Shared by the tests of the OpenRC step launcher: a clock that moves only when it is read, a
//! rig (a scratch directory, a launcher over it and the log it fills), shell scripts that are
//! the steps, and reads of `/proc`.
#![allow(dead_code, reason = "each test binary uses part of the shared support")]

use std::fs;
use std::num::NonZeroU64;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use hostkeeper_hook::{Invocation, Step};
use hostkeeper_openrc::OpenrcLauncher;
use hostkeeper_steps::{
    AdditiveCall, BootId, BootIdError, Bound, Captured, Clock, ProbeCall, StepCall,
};
use hostkeeper_testkit::Scratch;

/// The unit of the steps the tests run.
pub const UNIT: &str = "updater-web-cutover-7";
/// The params file every call names.
pub const PARAMS: &str = "/var/lib/updater/state/web/params.toml";

/// A clock that moves `step` seconds each time it is read, and when it is told to sleep, so that
/// a bound or a stop timeout passes by reading it, never by waiting.
#[derive(Debug)]
pub struct StepClock {
    now: AtomicU64,
    step: u64,
    reads: AtomicU64,
}

impl StepClock {
    pub fn new(step: u64) -> StepClock {
        StepClock {
            now: AtomicU64::new(0),
            step,
            reads: AtomicU64::new(0),
        }
    }

    /// How many times the clock was read.
    pub fn reads(&self) -> u64 {
        self.reads.load(Ordering::SeqCst)
    }
}

impl Clock for StepClock {
    fn mono_secs(&self) -> u64 {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.now.fetch_add(self.step, Ordering::SeqCst)
    }

    fn boot_id(&self) -> Result<BootId, BootIdError> {
        BootId::new("the test", "boot")
    }

    fn sleep_secs(&self, secs: u64) {
        self.now.fetch_add(secs, Ordering::SeqCst);
    }
}

/// What the launcher's log was given.
pub type Logged = Arc<Mutex<Vec<(String, Captured)>>>;

/// Held by every rig for as long as it lives, so that the tests of one binary run one after the
/// other. A test writes its scripts and then starts them; a process another test starts in
/// between holds the script's write descriptor from its `fork` until its `exec`, and `exec` of a
/// file that is open for writing fails with `ETXTBSY`. One test at a time cannot interleave.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// A scratch directory, a launcher whose files are in `<scratch>/steps` and whose clock moves
/// `step` seconds a read, and the log it fills.
pub struct Rig {
    pub dir: Scratch,
    pub clock: Arc<StepClock>,
    pub logged: Logged,
    pub launcher: OpenrcLauncher,
    _one_at_a_time: MutexGuard<'static, ()>,
}

/// A scratch directory under the target directory.
pub fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("a scratch directory")
}

impl Rig {
    pub fn new(name: &str, step: u64) -> Rig {
        Rig::with(name, step, |launcher| launcher)
    }

    /// A rig whose launcher is `build` of the rig's launcher.
    pub fn with(
        name: &str,
        step: u64,
        build: impl FnOnce(OpenrcLauncher) -> OpenrcLauncher,
    ) -> Rig {
        let one_at_a_time = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
        let dir = scratch(name);
        let clock = Arc::new(StepClock::new(step));
        let logged = Logged::default();
        let sink = Arc::clone(&logged);
        let launcher = OpenrcLauncher::new(&dir.path().join("steps"), clock.clone()).with_log(
            Arc::new(move |unit: &str, captured: &Captured| {
                sink.lock()
                    .expect("the log lock")
                    .push((unit.to_owned(), captured.clone()));
            }),
        );
        Rig {
            dir,
            clock,
            logged,
            launcher: build(launcher),
            _one_at_a_time: one_at_a_time,
        }
    }

    /// The directory of the launcher's files.
    pub fn steps_dir(&self) -> PathBuf {
        self.dir.path().join("steps")
    }

    /// A second launcher over the same files and clock, reading processes from `proc_root`: a
    /// daemon that was started again, and sees a fake process table.
    pub fn launcher_over(&self, proc_root: &Path) -> OpenrcLauncher {
        OpenrcLauncher::new(&self.steps_dir(), self.clock.clone()).with_proc_root(proc_root)
    }

    /// A path in the scratch directory.
    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// An executable shell script `name` with `body`.
    pub fn script(&self, name: &str, body: &str) -> PathBuf {
        let path = self.path(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write the script");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("make it executable");
        path
    }

    /// What the log was given so far.
    pub fn log(&self) -> Vec<(String, Captured)> {
        self.logged.lock().expect("the log lock").clone()
    }
}

/// A bound of `n` seconds.
pub fn secs(n: u64) -> Bound {
    Bound::Secs(NonZeroU64::new(n).expect("a bound is not zero"))
}

/// A `cutover` of `web` through `executable` in `unit`.
pub fn step_call<'a>(executable: &'a Path, unit: &'a str, bound: Bound) -> StepCall<'a> {
    StepCall {
        service: "web",
        invocation: Invocation {
            step: Step::Cutover,
            artifact: None,
            params: Some(PathBuf::from(PARAMS)),
        },
        executable,
        unit,
        bound,
    }
}

/// An `uninstall` of `web` through `executable` in `unit`, with the hook argument `update-hook`.
pub fn additive_call<'a>(
    executable: &'a Path,
    hook_args: &'a [String],
    params: &'a Path,
    timeout_secs: u64,
) -> AdditiveCall<'a> {
    AdditiveCall {
        service: "web",
        step: "uninstall",
        hook_args,
        params,
        executable,
        unit: UNIT,
        timeout_secs,
    }
}

/// A probe of `executable` with `args` in [`UNIT`].
pub fn probe_call<'a>(
    executable: &'a Path,
    args: &'a [&'a str],
    timeout_secs: u64,
    output_bytes: usize,
) -> ProbeCall<'a> {
    ProbeCall {
        service: "web",
        executable,
        args,
        unit: UNIT,
        timeout_secs,
        output_bytes,
    }
}

/// The text of what a captured stream holds.
pub fn text(ring: &hostkeeper_steps::Ring) -> String {
    String::from_utf8(ring.bytes()).expect("UTF-8 output")
}

/// Waits until `path` exists. The step that makes it is under way, so this ends; it waits for an
/// event and does not race a time.
pub fn wait_for(path: &Path) {
    while !path.exists() {
        std::thread::yield_now();
    }
}

/// The state and start time in the `/proc/<pid>/stat` text `stat`.
pub fn parse_stat(stat: &str) -> (char, u64) {
    let rest = &stat[stat.rfind(')').expect("a closing parenthesis") + 1..];
    let mut fields = rest.split_whitespace();
    let state = fields
        .next()
        .expect("a state")
        .chars()
        .next()
        .expect("a letter");
    let start = fields
        .nth(18)
        .expect("a start time")
        .parse()
        .expect("a number");
    (state, start)
}

/// The state and start time of process `pid` in the real `/proc`; `None` when it is not there.
pub fn proc_stat(pid: i32) -> Option<(char, u64)> {
    fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .map(|stat| parse_stat(&stat))
}

/// Whether process `pid` is gone or a zombie (not yet reaped by its parent).
pub fn gone(pid: i32) -> bool {
    proc_stat(pid).is_none_or(|(state, _)| state == 'Z')
}

/// Waits until process `pid` is gone or a zombie: a process killed with `SIGKILL` is, in the
/// moments after the kill call returns, still on its way out.
pub fn wait_gone(pid: i32) {
    while !gone(pid) {
        std::thread::yield_now();
    }
}

/// The process id a script wrote to `path`.
pub fn pid_in(path: &Path) -> i32 {
    fs::read_to_string(path)
        .expect("the pid file")
        .trim()
        .parse()
        .expect("a pid")
}

/// A stat line of process `pid` named `comm` in state `state` that started at `start`.
pub fn stat_line(pid: i32, comm: &str, state: char, start: u64) -> String {
    let between = ["0"; 18].join(" ");
    format!("{pid} ({comm}) {state} {between} {start} 0 0 0\n")
}
