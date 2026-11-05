//! The scripted `launchctl` every test drives. A handle: clones share one script, so a test keeps
//! a clone to script answers and to read what was asked after it gave the other away.
//!
//! A scripted answer is queued (given once, in order, before the steady one) or steady (given on
//! every call after the queue). Asked something with no answer, the fake fails as a `launchctl`
//! that cannot start does.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::launchctl::{Launchctl, LaunchctlError, LaunchctlOutput};

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
