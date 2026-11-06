//! The post-success watchers the daemon runs (design section 6.4): `verify` again every
//! `retry_after_secs` (30 seconds when the hook gives none, at most 3600) until it succeeds
//! without `promotion_pending`, the promotion ceiling passes, or `verify` fails. The watcher is the
//! step runner's ([`hostkeeper_steps::StepRunner::run`] of an update whose record is in it): it
//! never runs `rollback`, and its ceiling is measured from the record's `watching` stamp.
//!
//! Each watcher runs on a thread the daemon owns, one per service at most. It holds no lock
//! between its runs of `verify`. A new order of the service stops it before the order's runner
//! reads the record ([`crate::Ctx::runner`]): the watcher's sleep and its next launch see the stop
//! at once, a `verify` under way ends first, and the order's run then stops that `verify`'s unit
//! by name and goes on (the runner's own behaviour for a watcher a new order replaces). The
//! daemon stops and joins every watcher when it ends.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};

use hostkeeper_config::names::ServiceName;
use hostkeeper_steps::{
    BootId, BootIdError, Clock, Finished, LaunchError, RunError, StepCall, StepExit, StepLauncher,
    StepRunner, UpdateOrder,
};

/// What a stopped watcher's launcher answers: the run is not started.
pub const STOPPED: &str =
    "the post-success watcher was stopped by a new order or by the daemon's end";

/// What a watcher's thread ends with: the runner's verdict, or why it stopped without one.
pub type WatchEnd = Result<Finished, RunError>;

/// What the daemon does on the watcher's thread with how it ended, before the thread ends.
pub type Supervise = Box<dyn FnOnce(&WatchEnd) + Send>;

struct Watcher {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<WatchEnd>,
}

/// The daemon's watchers, by service.
#[derive(Default)]
pub struct Watchers {
    running: Mutex<BTreeMap<String, Watcher>>,
}

impl Watchers {
    fn map(&self) -> MutexGuard<'_, BTreeMap<String, Watcher>> {
        self.running.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Starts the watcher of `service`, `order` run by `runner` on a thread of its own, and
    /// `supervise` given how it ended; `stop` must be the flag `runner`'s launcher and clock were
    /// made with ([`Yielding`], [`SlicedClock`]). `false`, and nothing started, while a watcher of
    /// the service runs. Only the start pass starts watchers, before any order is taken.
    pub(crate) fn start(
        &self,
        service: &ServiceName,
        runner: StepRunner,
        order: UpdateOrder,
        stop: Arc<AtomicBool>,
        supervise: Supervise,
    ) -> bool {
        let mut map = self.map();
        let alive = map
            .get(service.as_str())
            .is_some_and(|watcher| !watcher.thread.is_finished());
        if alive {
            return false;
        }
        let thread = thread::spawn(move || {
            let end = runner.run(&order);
            supervise(&end);
            end
        });
        // A watcher that had ended is replaced; what it ended with was already reported.
        map.insert(service.to_string(), Watcher { stop, thread });
        true
    }

    /// Whether a watcher of `service` is running.
    pub fn running(&self, service: &ServiceName) -> bool {
        self.map()
            .get(service.as_str())
            .is_some_and(|watcher| !watcher.thread.is_finished())
    }

    /// Stops the watcher of `service` and waits for it to end: what it ended with, `None` when
    /// there was none (or its thread panicked).
    pub fn stop(&self, service: &ServiceName) -> Option<WatchEnd> {
        let watcher = {
            let mut map = self.map();
            let watcher = map.remove(service.as_str())?;
            // Set while the map is held: once [`Watchers::running`] says no, the watcher is told.
            watcher.stop.store(true, Ordering::Release);
            watcher
        };
        watcher.thread.join().ok()
    }

    /// Waits for the watcher of `service` to end by itself: what it ended with, `None` when there
    /// was none (or its thread panicked).
    pub fn wait(&self, service: &ServiceName) -> Option<WatchEnd> {
        let watcher = self.map().remove(service.as_str())?;
        watcher.thread.join().ok()
    }
}

impl Drop for Watchers {
    /// The daemon ends: every watcher is stopped, and waited for.
    fn drop(&mut self) {
        let all = std::mem::take(&mut *self.map());
        for watcher in all.values() {
            watcher.stop.store(true, Ordering::Release);
        }
        for watcher in all.into_values() {
            drop(watcher.thread.join());
        }
    }
}

/// The launcher of a watcher: the daemon's, except that once the watcher is stopped no run
/// starts. A stop of a unit is always passed on.
pub(crate) struct Yielding {
    pub inner: Arc<dyn StepLauncher>,
    pub stop: Arc<AtomicBool>,
}

impl StepLauncher for Yielding {
    fn run(&self, call: &StepCall<'_>) -> Result<StepExit, LaunchError> {
        match self.stop.load(Ordering::Acquire) {
            true => Err(LaunchError::Launcher(STOPPED.to_owned())),
            false => self.inner.run(call),
        }
    }

    fn stop(&self, unit: &str) -> Result<(), LaunchError> {
        self.inner.stop(unit)
    }
}

/// The clock of a watcher: the daemon's, except that a wait is made of one-second waits and ends
/// as soon as the watcher is stopped, so a stop never waits out a poll of up to an hour.
pub(crate) struct SlicedClock {
    pub inner: Arc<dyn Clock>,
    pub stop: Arc<AtomicBool>,
}

impl Clock for SlicedClock {
    fn mono_secs(&self) -> u64 {
        self.inner.mono_secs()
    }

    fn boot_id(&self) -> Result<BootId, BootIdError> {
        self.inner.boot_id()
    }

    fn sleep_secs(&self, secs: u64) {
        let mut left = secs;
        while (left > 0) & !self.stop.load(Ordering::Acquire) {
            self.inner.sleep_secs(1);
            left -= 1;
        }
    }
}
