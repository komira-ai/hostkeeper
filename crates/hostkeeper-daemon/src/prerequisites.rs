//! The host's prerequisites (`prerequisites-v1`): the [`PrerequisitesSink`] of the daemon's
//! heartbeat loop. The coordinator sends the set of the host's group whole, and this makes sure of
//! each prerequisite of it that applies to the host's platform: today one, the Command Line Tools
//! of a Mac ([`crate::xcode_clt`]). Linux hosts, under systemd and OpenRC, hold the set and run
//! nothing.
//!
//! **A set** ([`Prerequisites::apply`]) is held when its digest is the digest of its names
//! (`hostkeeper_wire::prerequisites`) and the names keep the rules of a set; otherwise it is
//! *refused*: the report names it and why, and the set held stays. A set that is held clears the
//! refusal; one whose digest is the one held changes nothing else. A name that was in the old set keeps its state, so a set sent again never
//! resets the wait of a failure; a new name of this platform starts `PENDING`, a name of another
//! platform is `NOT_APPLICABLE` and a name this release does not know is `UNKNOWN`; neither of the
//! two is ever looked at.
//!
//! **The work** is started by [`Prerequisites::report`] and [`Prerequisites::apply`], on a worker
//! of the sink's [`Dispatch`], one pass at a time, and neither waits for it. A pass is started when
//! a prerequisite that applies is `PENDING`, is `PRESENT` and was looked at an hour or more ago, or
//! is `FAILED` and its next try has come. The pass looks at each such prerequisite and installs it
//! when it is not there. The state is updated as each step is reached, and never held locked while
//! a program runs, so a report made during an install says `INSTALLING`.
//!
//! **A failure** sets `FAILED` with the reason, counts it, and sets the next try: 30 minutes after
//! the first failure, doubled after each next one, at most 24 hours. A success counts the failures
//! back to 0. The count and the wait are kept in memory only. Every outcome is a line of the
//! daemon's log.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use hostkeeper_launchd::PlatformKind;
use hostkeeper_link::{
    Dispatch, Log, MAX_PREREQUISITE_REASON, MAX_PREREQUISITE_VERSION, PrerequisiteState,
    PrerequisiteStatus, PrerequisitesRefusal, PrerequisitesReport, PrerequisitesSink,
    PushedPrerequisites, ThreadDispatch, UnixClock,
};
use hostkeeper_wire::prerequisites::{self as rules, Prerequisite};

use crate::unix_clock::SystemUnixClock;
use crate::xcode_clt::CommandLineTools;

/// How long a present prerequisite goes unlooked at, in seconds.
pub const RECHECK_SECS: u64 = 60 * 60;

/// The wait after the first failure, in seconds.
pub const FIRST_WAIT_SECS: u64 = 30 * 60;

/// The longest wait after a failure, in seconds.
pub const MAX_WAIT_SECS: u64 = 24 * 60 * 60;

/// One prerequisite of the held set: what the report says of it, and how many tries in a row
/// failed (which the report says only while it is `FAILED`).
#[derive(Clone)]
struct Item {
    state: PrerequisiteState,
    failures: u32,
}

/// What the sink keeps between two calls.
#[derive(Default)]
struct State {
    digest: String,
    refused: Option<PrerequisitesRefusal>,
    items: Vec<Item>,
    /// Whether a pass was started and has not ended.
    running: bool,
}

struct Inner {
    /// The Mac's steps; none on a Linux host.
    tools: Option<Arc<CommandLineTools>>,
    clock: Arc<dyn UnixClock>,
    dispatch: Arc<dyn Dispatch>,
    log: Arc<dyn Log>,
    state: Mutex<State>,
}

/// The daemon's host prerequisites.
pub struct Prerequisites {
    inner: Arc<Inner>,
}

/// A pass that was started; the sink's `running` ends with it, however it ends, and also when its
/// dispatcher drops the job without running it. A prerequisite the pass left `INSTALLING` (the
/// pass ended without an outcome, as a panic ends it) is a failure: counted, with its wait, so
/// that it is neither shown installing forever nor tried again at once.
struct Running(Arc<Inner>);

impl Drop for Running {
    fn drop(&mut self) {
        let unfinished: Vec<String> = {
            let mut state = self.0.state();
            state.running = false;
            state
                .items
                .iter()
                .filter(|item| item.state.status == PrerequisiteStatus::Installing)
                .map(|item| item.state.name.clone())
                .collect()
        };
        for name in unfinished {
            self.0.finish(
                &name,
                Err("the install ended without an outcome".to_owned()),
            );
        }
    }
}

impl Prerequisites {
    /// The sink of a Mac when `tools` is given, of a Linux host when not, over `clock` and `log`,
    /// whose passes run on `dispatch`.
    #[must_use]
    pub fn new(
        tools: Option<CommandLineTools>,
        clock: Arc<dyn UnixClock>,
        dispatch: Arc<dyn Dispatch>,
        log: Arc<dyn Log>,
    ) -> Prerequisites {
        Prerequisites {
            inner: Arc::new(Inner {
                tools: tools.map(Arc::new),
                clock,
                dispatch,
                log,
                state: Mutex::new(State::default()),
            }),
        }
    }

    /// The sink of a host of `kind`: on Linux, under systemd or OpenRC, one that runs nothing; on a
    /// Mac, one over the Mac's own programs. Its clock is the host's, and each pass runs on a
    /// thread of its own.
    #[must_use]
    pub fn at_host(kind: PlatformKind, log: Arc<dyn Log>) -> Prerequisites {
        let tools = match kind {
            PlatformKind::Linux | PlatformKind::Openrc => None,
            PlatformKind::Macos => Some(CommandLineTools::at_host()),
        };
        Prerequisites::new(
            tools,
            Arc::new(SystemUnixClock),
            Arc::new(ThreadDispatch),
            log,
        )
    }

    /// Starts a pass when one is due and none is running.
    fn start_if_due(&self) {
        let inner = &self.inner;
        let Some(tools) = inner.tools.clone() else {
            return;
        };
        let now = inner.clock.unix_secs();
        {
            let mut state = inner.state();
            if state.running || due(&state.items, now).is_none() {
                return;
            }
            state.running = true;
        }
        let running = Running(Arc::clone(inner));
        let worker = Arc::clone(inner);
        inner.dispatch.start(Box::new(move || {
            worker.pass(&tools);
            drop(running);
        }));
    }
}

impl Inner {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether a prerequisite of this host's platform applies here.
    fn applies(&self, prerequisite: Prerequisite) -> bool {
        if self.tools.is_some() {
            prerequisite.on_macos()
        } else {
            prerequisite.on_linux()
        }
    }

    /// The state a name starts in.
    fn first_status(&self, name: &str) -> PrerequisiteStatus {
        match Prerequisite::parse(name) {
            None => PrerequisiteStatus::Unknown,
            Some(prerequisite) if self.applies(prerequisite) => PrerequisiteStatus::Pending,
            Some(_) => PrerequisiteStatus::NotApplicable,
        }
    }

    /// Holds `set`, or refuses it, as the module says.
    fn hold(&self, set: &PushedPrerequisites) {
        let short: String = set.digest.chars().take(12).collect();
        let judged = if set.digest == rules::digest(&set.names) {
            rules::check_set(&set.names).map_err(|error| error.to_string())
        } else {
            Err("the coordinator's digest is not the digest of its names".to_owned())
        };
        if let Err(reason) = judged {
            let reason = clip(&reason, MAX_PREREQUISITE_REASON);
            self.log.line(&format!(
                "error: host prerequisites {short} are refused: {reason}"
            ));
            self.state().refused = Some(PrerequisitesRefusal {
                digest: set.digest.clone(),
                reason,
            });
            return;
        }
        let mut state = self.state();
        state.refused = None;
        if state.digest == set.digest {
            return;
        }
        let items: Vec<Item> = set
            .names
            .iter()
            .map(|name| {
                state
                    .items
                    .iter()
                    .find(|item| item.state.name == *name)
                    .cloned()
                    .unwrap_or_else(|| Item {
                        state: PrerequisiteState {
                            name: name.clone(),
                            status: self.first_status(name),
                            ..PrerequisiteState::default()
                        },
                        failures: 0,
                    })
            })
            .collect();
        let count = items.len();
        let noun = if count == 1 {
            "prerequisite"
        } else {
            "prerequisites"
        };
        state.items = items;
        state.digest.clone_from(&set.digest);
        drop(state);
        self.log
            .line(&format!("host prerequisites {short}: {count} {noun} held"));
    }

    /// Looks at each prerequisite that is due, one after the other, and installs what is missing.
    fn pass(&self, tools: &CommandLineTools) {
        while let Some(name) = self.next_due() {
            let outcome = self.make_sure(tools, &name);
            self.finish(&name, outcome);
        }
    }

    /// The name of the first prerequisite that is due now.
    fn next_due(&self) -> Option<String> {
        let now = self.clock.unix_secs();
        let state = self.state();
        due(&state.items, now).map(|index| state.items[index].state.name.clone())
    }

    /// The version of the prerequisite `name` once it is on the host, or why it could not be made
    /// so. Only the prerequisites that apply to a Mac are asked for.
    fn make_sure(&self, tools: &CommandLineTools, name: &str) -> Result<String, String> {
        if let Some(version) = tools.check().map_err(|error| error.to_string())? {
            return Ok(version);
        }
        self.log.line(&format!(
            "host prerequisites: {name} is not on the host, installing it"
        ));
        self.set_installing(name);
        tools.install(&*self.log).map_err(|error| error.to_string())
    }

    /// Shows the prerequisite `name` as being installed.
    fn set_installing(&self, name: &str) {
        let mut state = self.state();
        for item in state
            .items
            .iter_mut()
            .filter(|item| item.state.name == name)
        {
            item.state.status = PrerequisiteStatus::Installing;
            item.state.version.clear();
            item.state.reason.clear();
            item.state.failures = 0;
            item.state.next_try_unix = 0;
        }
    }

    /// Records how the look or install of `name` ended, and logs it. A name that is no longer in
    /// the held set is left out.
    fn finish(&self, name: &str, outcome: Result<String, String>) {
        let now = self.clock.unix_secs();
        let mut state = self.state();
        let Some(item) = state.items.iter_mut().find(|item| item.state.name == name) else {
            return;
        };
        item.state.seen_unix = now;
        let line = match outcome {
            Ok(version) => {
                let version = clip(&version, MAX_PREREQUISITE_VERSION);
                item.failures = 0;
                item.state.status = PrerequisiteStatus::Present;
                item.state.reason.clear();
                item.state.failures = 0;
                item.state.next_try_unix = 0;
                let line = format!("host prerequisites: {name} is present, version {version}");
                item.state.version = version;
                line
            }
            Err(reason) => {
                let reason = clip(&reason, MAX_PREREQUISITE_REASON);
                item.failures = item.failures.saturating_add(1);
                let wait = wait_secs(item.failures);
                item.state.status = PrerequisiteStatus::Failed;
                item.state.version.clear();
                item.state.failures = item.failures;
                item.state.next_try_unix = now.saturating_add(wait);
                let line = format!(
                    "error: host prerequisites: {name} failed ({} in a row), next try in {wait} s: {reason}",
                    item.failures
                );
                item.state.reason = reason;
                line
            }
        };
        drop(state);
        self.log.line(&line);
    }
}

/// The index of the first prerequisite of `items` that is due at `now`.
fn due(items: &[Item], now: u64) -> Option<usize> {
    items.iter().position(|item| match item.state.status {
        PrerequisiteStatus::Pending => true,
        PrerequisiteStatus::Present => now >= item.state.seen_unix.saturating_add(RECHECK_SECS),
        PrerequisiteStatus::Failed => now >= item.state.next_try_unix,
        PrerequisiteStatus::Installing
        | PrerequisiteStatus::NotApplicable
        | PrerequisiteStatus::Unknown => false,
    })
}

/// The wait after the `failures`th failure in a row: [`FIRST_WAIT_SECS`], doubled after each
/// next one, at most [`MAX_WAIT_SECS`].
fn wait_secs(failures: u32) -> u64 {
    let doublings = failures.saturating_sub(1).min(32);
    FIRST_WAIT_SECS
        .saturating_mul(1_u64 << doublings)
        .min(MAX_WAIT_SECS)
}

/// `text` cut to `max` bytes at a character boundary.
fn clip(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

impl PrerequisitesSink for Prerequisites {
    fn report(&self) -> PrerequisitesReport {
        self.start_if_due();
        let state = self.inner.state();
        PrerequisitesReport {
            digest: state.digest.clone(),
            refused: state.refused.clone(),
            states: state.items.iter().map(|item| item.state.clone()).collect(),
        }
    }

    fn apply(&self, set: &PushedPrerequisites) {
        self.inner.hold(set);
        self.start_if_due();
    }
}

#[cfg(test)]
#[path = "prerequisites_tests.rs"]
mod tests;
