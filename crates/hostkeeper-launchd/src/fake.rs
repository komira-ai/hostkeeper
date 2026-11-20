//! The scripted `launchctl` every test drives, and the scripted runner of a program with input
//! ([`FakeStdinRun`]). Each is a handle: clones share one script, so a test keeps a clone to
//! script answers and to read what was asked after it gave the other away.
//!
//! A scripted answer is queued (given once, in order, before the steady one) or steady (given on
//! every call after the queue). Asked something with no answer, the fake fails as a `launchctl`
//! that cannot start does.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::launchctl::{Launchctl, LaunchctlError, LaunchctlOutput};
use crate::stdin_process::{StdinError, StdinRun};

type Answer = Result<LaunchctlOutput, LaunchctlError>;

#[derive(Debug, Default)]
struct Script {
    queued: BTreeMap<Vec<String>, VecDeque<Answer>>,
    steady: BTreeMap<Vec<String>, Answer>,
    calls: Vec<Vec<String>>,
}

fn lock(shared: &Mutex<Script>) -> MutexGuard<'_, Script> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A `launchctl` that answers what it is told, per exact argument list.
#[derive(Clone, Debug, Default)]
pub struct FakeLaunchctl(Arc<Mutex<Script>>);

impl FakeLaunchctl {
    /// A fake with nothing scripted.
    pub fn new() -> FakeLaunchctl {
        FakeLaunchctl::default()
    }

    /// The next call with exactly `args` answers `answer`, once; queued answers are given in the
    /// order they were queued.
    pub fn then(&self, args: &[String], answer: Answer) {
        lock(&self.0)
            .queued
            .entry(args.to_vec())
            .or_default()
            .push_back(answer);
    }

    /// Every call with exactly `args` answers `answer` once the queued answers are used up.
    pub fn always(&self, args: &[String], answer: Answer) {
        lock(&self.0).steady.insert(args.to_vec(), answer);
    }

    /// The arguments of every call, in order.
    pub fn calls(&self) -> Vec<Vec<String>> {
        lock(&self.0).calls.clone()
    }
}

impl Launchctl for FakeLaunchctl {
    fn run(&self, args: &[String]) -> Answer {
        let mut script = lock(&self.0);
        script.calls.push(args.to_vec());
        let queued = script.queued.get_mut(args).and_then(VecDeque::pop_front);
        queued
            .or_else(|| script.steady.get(args).cloned())
            .unwrap_or_else(|| {
                Err(LaunchctlError::Spawn {
                    args: args.join(" "),
                    why: "the fake has no answer".to_owned(),
                })
            })
    }
}

type StdinAnswer = Result<LaunchctlOutput, StdinError>;

/// One call of a [`FakeStdinRun`]: the step, the arguments and the input it was given. Its
/// `Debug` prints the input's length alone, since the input is a password.
#[derive(Clone, PartialEq, Eq)]
pub struct StdinCall {
    /// The step the caller named.
    pub step: String,
    /// The arguments.
    pub args: Vec<String>,
    /// The bytes given as standard input.
    pub input: Vec<u8>,
}

impl std::fmt::Debug for StdinCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StdinCall")
            .field("step", &self.step)
            .field("args", &self.args)
            .field(
                "input",
                &format_args!("<{} bytes withheld>", self.input.len()),
            )
            .finish()
    }
}

#[derive(Debug, Default)]
struct StdinScript {
    queued: BTreeMap<Vec<String>, VecDeque<StdinAnswer>>,
    calls: Vec<StdinCall>,
}

/// A runner of a program with input that answers what it is told, per exact argument list, once
/// per answer and in the order the answers were queued. Asked something with no answer left, it
/// fails as a program that cannot start does.
#[derive(Clone, Debug, Default)]
pub struct FakeStdinRun(Arc<Mutex<StdinScript>>);

impl FakeStdinRun {
    /// A fake with nothing scripted.
    pub fn new() -> FakeStdinRun {
        FakeStdinRun::default()
    }

    /// The next call with exactly `args` answers `answer`, once.
    pub fn then(&self, args: &[String], answer: StdinAnswer) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .queued
            .entry(args.to_vec())
            .or_default()
            .push_back(answer);
    }

    /// Every call, in order.
    pub fn calls(&self) -> Vec<StdinCall> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .calls
            .clone()
    }
}

impl StdinRun for FakeStdinRun {
    fn run(&self, step: &str, args: &[String], input: &[u8]) -> StdinAnswer {
        let mut script = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        script.calls.push(StdinCall {
            step: step.to_owned(),
            args: args.to_vec(),
            input: input.to_vec(),
        });
        let queued = script.queued.get_mut(args).and_then(VecDeque::pop_front);
        queued.unwrap_or_else(|| {
            Err(StdinError::Spawn {
                step: step.to_owned(),
                why: "the fake has no answer".to_owned(),
            })
        })
    }
}
