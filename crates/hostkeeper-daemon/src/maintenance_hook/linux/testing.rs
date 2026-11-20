//! The fake host the stage's tests drive: one handle that is the stage's programs, its disk, its
//! lock waiter and its own packages. Clones share one script, so a test keeps a clone to script
//! answers and to read what the stage did after it gave the others away.
//!
//! A program's answer is scripted per exact argument list: queued (given once, in order, before
//! the steady one) or steady. Asked something with no answer, a program cannot be run.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use hostkeeper_launchd::{Launchctl, LaunchctlError, LaunchctlOutput};
use hostkeeper_proto::hostkeeper::agent::v1::{LinuxScope as WireScope, Maintain, MaintenanceKind};
use hostkeeper_testkit::Scratch;

use super::stage::{Disk, LockWaiter, Programs, Stage};
use crate::line_process::LineTool;
use crate::maintenance_collect::HostRoot;
use crate::maintenance_hook::kind::KindError;
use crate::maintenance_hook::params::MaintainParams;
use crate::maintenance_own::OwnPackages;

/// How the stage asked for a program to be run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Way {
    /// A command: its output kept, its standard error read.
    Command,
    /// A listing: its standard output read line by line.
    Listing,
}

/// One run the stage asked for: the program's path, how, its arguments and its deadline. The
/// environment is the runner's, [`super::stage::HostPrograms`]'s on a host, which gives none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    pub program: PathBuf,
    pub way: Way,
    pub args: Vec<String>,
    pub deadline: Duration,
}

type Answer = Result<LaunchctlOutput, String>;

/// The answer that stands for a command killed at its deadline.
pub const TIMED_OUT: &str = "timed out";

#[derive(Debug)]
struct Script {
    queued: BTreeMap<Vec<String>, VecDeque<Answer>>,
    steady: BTreeMap<Vec<String>, Answer>,
    calls: Vec<Call>,
    waits: Vec<u64>,
    made: Vec<String>,
    measured: Vec<String>,
    make_fails: Option<String>,
    free: Result<u64, String>,
    own: Result<BTreeSet<String>, String>,
}

impl Default for Script {
    fn default() -> Script {
        Script {
            queued: BTreeMap::new(),
            steady: BTreeMap::new(),
            calls: Vec::new(),
            waits: Vec::new(),
            made: Vec::new(),
            measured: Vec::new(),
            make_fails: None,
            free: Ok(u64::MAX),
            own: Ok(BTreeSet::from([
                "hostkeeper".to_owned(),
                "nginx".to_owned(),
            ])),
        }
    }
}

/// The fake host.
#[derive(Clone, Debug, Default)]
pub struct FakeHost(Arc<Mutex<Script>>);

fn lock(shared: &Mutex<Script>) -> MutexGuard<'_, Script> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `list` as owned arguments.
pub fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|arg| (*arg).to_owned()).collect()
}

/// A run that exited `code`, wrote `stdout` and `stderr`.
pub fn ran(code: i32, stdout: &str, stderr: &str) -> Answer {
    Ok(LaunchctlOutput {
        code: Some(code),
        stdout: stdout.as_bytes().to_vec(),
        stderr: stderr.as_bytes().to_vec(),
    })
}

/// A run ended by a signal.
pub fn signalled() -> Answer {
    Ok(LaunchctlOutput {
        code: None,
        stdout: Vec::new(),
        stderr: Vec::new(),
    })
}

/// The deferral of `reason` with `detail`.
pub fn deferred(reason: &'static str, detail: &str) -> Result<(), KindError> {
    Err(KindError::Deferred {
        reason,
        detail: detail.to_owned(),
    })
}

impl FakeHost {
    pub fn new() -> FakeHost {
        FakeHost::default()
    }

    /// The next run with exactly `args` answers `answer`, once.
    pub fn then(&self, args: &[String], answer: Answer) {
        lock(&self.0)
            .queued
            .entry(args.to_vec())
            .or_default()
            .push_back(answer);
    }

    /// Every run with exactly `args` answers `answer` once the queued answers are used up.
    pub fn always(&self, args: &[String], answer: Answer) {
        lock(&self.0).steady.insert(args.to_vec(), answer);
    }

    /// The disk has `bytes` free.
    pub fn free(&self, bytes: Result<u64, String>) {
        lock(&self.0).free = bytes;
    }

    /// Making a directory fails with `why`.
    pub fn make_fails(&self, why: &str) {
        lock(&self.0).make_fails = Some(why.to_owned());
    }

    /// hostkeeper's own packages are `names`, or cannot be read.
    pub fn own(&self, names: Result<&[&str], &str>) {
        lock(&self.0).own = names
            .map(|names| names.iter().map(|name| (*name).to_owned()).collect())
            .map_err(str::to_owned);
    }

    /// Every run asked for, in order.
    pub fn calls(&self) -> Vec<Call> {
        lock(&self.0).calls.clone()
    }

    /// The arguments of every run asked for, in order.
    pub fn arg_lists(&self) -> Vec<Vec<String>> {
        self.calls().into_iter().map(|call| call.args).collect()
    }

    /// Each wait for a held lock, in seconds, in order.
    pub fn waits(&self) -> Vec<u64> {
        lock(&self.0).waits.clone()
    }

    /// Each directory made, in order.
    pub fn made(&self) -> Vec<String> {
        lock(&self.0).made.clone()
    }

    /// Each directory whose free space was read, in order.
    pub fn measured(&self) -> Vec<String> {
        lock(&self.0).measured.clone()
    }

    /// The answer to a run of `program` with `args`, recorded.
    fn answer(&self, program: &Path, way: Way, deadline: Duration, args: &[String]) -> Answer {
        let mut script = lock(&self.0);
        script.calls.push(Call {
            program: program.to_owned(),
            way,
            args: args.to_vec(),
            deadline,
        });
        let queued = script.queued.get_mut(args).and_then(VecDeque::pop_front);
        queued
            .or_else(|| script.steady.get(args).cloned())
            .unwrap_or_else(|| Err("the fake has no answer".to_owned()))
    }

    /// The stage over this host, its files under `root`, the lock waited for `lock_wait_secs`.
    pub fn stage(&self, root: &Scratch, lock_wait_secs: u64) -> Stage {
        Stage::new(
            HostRoot::at(root.path()),
            Box::new(self.clone()),
            Box::new(self.clone()),
            Box::new(self.clone()),
            Box::new(self.clone()),
            lock_wait_secs,
        )
    }
}

/// One program of the fake host.
struct FakeProgram {
    host: FakeHost,
    program: PathBuf,
    deadline: Duration,
}

impl Launchctl for FakeProgram {
    fn run(&self, args: &[String]) -> Result<LaunchctlOutput, LaunchctlError> {
        self.host
            .answer(&self.program, Way::Command, self.deadline, args)
            .map_err(|why| match why.as_str() {
                TIMED_OUT => LaunchctlError::TimedOut {
                    args: args.join(" "),
                },
                _ => LaunchctlError::Spawn {
                    args: args.join(" "),
                    why,
                },
            })
    }
}

impl LineTool for FakeProgram {
    fn run_lines(
        &self,
        args: &[String],
        line: &mut dyn FnMut(&str),
    ) -> Result<Option<i32>, String> {
        let output = self
            .host
            .answer(&self.program, Way::Listing, self.deadline, args)?;
        for text in String::from_utf8_lossy(&output.stdout).lines() {
            line(text);
        }
        Ok(output.code)
    }
}

impl Programs for FakeHost {
    fn command(&self, program: &Path, deadline: Duration) -> Box<dyn Launchctl> {
        Box::new(FakeProgram {
            host: self.clone(),
            program: program.to_owned(),
            deadline,
        })
    }

    fn listing(&self, program: &Path, deadline: Duration) -> Box<dyn LineTool> {
        Box::new(FakeProgram {
            host: self.clone(),
            program: program.to_owned(),
            deadline,
        })
    }
}

impl Disk for FakeHost {
    fn make_dir(&self, dir: &str) -> Result<(), String> {
        let mut script = lock(&self.0);
        script.made.push(dir.to_owned());
        match &script.make_fails {
            Some(why) => Err(why.clone()),
            None => Ok(()),
        }
    }

    fn free_bytes(&self, dir: &str) -> Result<u64, String> {
        let mut script = lock(&self.0);
        script.measured.push(dir.to_owned());
        script.free.clone()
    }
}

impl LockWaiter for FakeHost {
    fn wait(&self, secs: u64) {
        lock(&self.0).waits.push(secs);
    }
}

impl OwnPackages for FakeHost {
    fn names(&self) -> Result<BTreeSet<String>, String> {
        lock(&self.0).own.clone()
    }
}

/// A scratch directory standing for the host's files.
pub fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

/// A host whose files under a new scratch directory hold the program `program` (an absolute
/// path), and so pick its package manager.
pub fn host_with(name: &str, program: &str) -> Scratch {
    let root = scratch(name);
    let file = HostRoot::at(root.path()).path(program);
    std::fs::create_dir_all(file.parent().expect("a parent")).expect("the directory");
    std::fs::write(file, "").expect("the program");
    root
}

/// The parameters of a `linux` run under `scope`.
pub fn params(scope: WireScope) -> MaintainParams {
    params_scoped(scope as i32)
}

/// The parameters of a `linux` run whose `linux_scope` is the wire value `scope`.
pub fn params_scoped(scope: i32) -> MaintainParams {
    MaintainParams::new(Maintain {
        run_id: "run-1".to_owned(),
        kinds: vec![MaintenanceKind::Linux as i32],
        linux_scope: scope,
        ..Maintain::default()
    })
    .expect("valid parameters")
}
