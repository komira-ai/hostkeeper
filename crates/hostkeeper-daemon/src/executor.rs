//! The executor: [`Daemon::order`] takes one order of the coordinator, decodes it into a
//! [`Verb`] and hands it to the verb's module under `verbs/`, which answers with a [`Reply`].
//! [`Daemon::status`] reports what the daemon holds for each service.
//!
//! The executor only dispatches: each verb is one module with one function taking the shared
//! [`Ctx`] and its typed arguments, so that a later verb fills one file and one arm.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_config::daemon::DaemonConfig;
use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_hook::Verdict;
use hostkeeper_proto::hostkeeper::agent::v1::Order;
use hostkeeper_steps::record::{Loaded, Record, RecordError, State};
use hostkeeper_steps::{Clock, DeadMan, Finished, RecordStore, StepLauncher, StepRunner, Watch};

use crate::checks;
use crate::halt::HaltStore;
use crate::installed::{self, CURRENT};
use crate::locks::{ServiceLocks, StepLock};
use crate::packages::Packages;
use crate::refusal::Refusal;
use crate::verb::Verb;
use crate::verbs;

/// What the daemon runs on: the ports of everything privileged, and the clock.
pub struct Ports {
    /// Runs one step of a hook.
    pub launcher: Arc<dyn StepLauncher>,
    /// Fetches, verifies, unpacks and seals packages.
    pub packages: Arc<dyn Packages>,
    /// Monotonic seconds and the boot id; nothing the daemon decides reads the wall clock.
    pub clock: Arc<dyn Clock>,
    /// How often each service's dead-man looks at its running step, in seconds; `None` starts
    /// none (a test that drives the bounds itself).
    pub deadman_poll_secs: Option<u64>,
}

/// Where the daemon keeps and finds things.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dirs {
    /// A directory per service with its step record, params file and held halt generation; the
    /// lock file.
    pub state: PathBuf,
    /// A directory per service with its version directories and the `current` and `previous`
    /// links.
    pub packages: PathBuf,
}

/// What every verb works with.
pub struct Ctx {
    pub config: DaemonConfig,
    pub dirs: Dirs,
    /// The version of this daemon, which a manifest's `min_hostkeeper` is compared with.
    pub own_version: HostkeeperVersion,
    pub launcher: Arc<dyn StepLauncher>,
    pub packages: Arc<dyn Packages>,
    pub clock: Arc<dyn Clock>,
    pub locks: ServiceLocks,
    pub step_lock: StepLock,
    pub halts: HaltStore,
    runners: Mutex<BTreeMap<String, Arc<StepRunner>>>,
    deadman_poll_secs: Option<u64>,
    /// Ends the dead-men when the daemon is dropped.
    stop: Arc<AtomicBool>,
}

impl Ctx {
    /// The state directory of `service`.
    pub fn service_dir(&self, service: &ServiceName) -> PathBuf {
        self.dirs.state.join(service.as_str())
    }

    /// The step record store of `service`.
    pub fn store(&self, service: &ServiceName) -> RecordStore {
        RecordStore::new(&self.service_dir(service))
    }

    /// The step runner of `service`, made with its dead-man on first use.
    pub fn runner(&self, service: &ServiceName) -> Arc<StepRunner> {
        let mut runners = self.runners.lock().unwrap_or_else(PoisonError::into_inner);
        let made = runners.entry(service.to_string()).or_insert_with(|| {
            let runner = StepRunner::new(
                Arc::clone(&self.launcher),
                Arc::clone(&self.clock),
                self.store(service),
            );
            self.watch(runner.watch());
            Arc::new(runner)
        });
        Arc::clone(made)
    }

    /// Starts the dead-man of a runner's watch, when the daemon has dead-men.
    fn watch(&self, watch: Arc<Watch>) {
        if let Some(poll) = self.deadman_poll_secs {
            let deadman = DeadMan::new(watch, Arc::clone(&self.launcher), Arc::clone(&self.clock));
            // Detached: it ends at its next tick after the daemon is dropped.
            drop(deadman.spawn(Arc::clone(&self.stop), poll));
        }
    }
}

/// How the daemon answered an order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    /// The order ran to a verdict, now (`resent: false`) or before, and its step record answers
    /// for it (`resent: true`).
    Finished {
        verdict: Verdict,
        reason: Option<String>,
        resent: bool,
    },
    /// The same order is running now: a resend while it runs. Nothing more was started.
    Running,
    /// The order was refused: nothing ran and nothing was written.
    Refused { refusal: Refusal, detail: String },
    /// The runner stopped without a verdict; the step record says where, and a resend of the
    /// order resumes it.
    Interrupted { refusal: Refusal, detail: String },
}

impl Reply {
    /// The refusal of a [`Reply::Refused`] or [`Reply::Interrupted`].
    pub fn refusal(&self) -> Option<Refusal> {
        match self {
            Reply::Refused { refusal, .. } | Reply::Interrupted { refusal, .. } => Some(*refusal),
            Reply::Finished { .. } | Reply::Running => None,
        }
    }

    /// A refusal.
    pub fn refused(refusal: Refusal, detail: impl ToString) -> Reply {
        Reply::Refused {
            refusal,
            detail: detail.to_string(),
        }
    }
}

/// What a service's step record says, as [`Daemon::status`] reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordState {
    /// No order has run for the service.
    Absent,
    /// An order is under way, or stopped without a verdict.
    Unfinished(Box<Record>),
    /// The last order ended.
    Finished(Box<Record>),
    /// The record cannot be read; no order runs for the service until it can.
    Unreadable(String),
    /// The record was written by a newer daemon.
    Newer(u64),
}

impl RecordState {
    fn of(loaded: Result<Loaded, RecordError>) -> RecordState {
        match loaded {
            Ok(Loaded::Absent) => RecordState::Absent,
            Ok(Loaded::Current(rec)) => match rec.state {
                State::Finished { .. } => RecordState::Finished(rec),
                _ => RecordState::Unfinished(rec),
            },
            Ok(Loaded::Newer { schema }) => RecordState::Newer(schema),
            Err(e) => RecordState::Unreadable(e.to_string()),
        }
    }
}

/// What the daemon holds about one service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceStatus {
    pub service: ServiceName,
    /// The version `current` names; `None` when the service runs none.
    pub running_version: Option<Version>,
    /// The generation of halt clears the service is held under; `None` when it cannot be read.
    pub held_halt: Option<u64>,
    /// An order holds the service's lock now (or a step that could not be stopped holds it).
    pub locked: bool,
    pub record: RecordState,
}

impl ServiceStatus {
    /// How the last order ended, `None` when none has or one is under way.
    pub fn last_result(&self) -> Option<Finished> {
        match &self.record {
            RecordState::Finished(rec) => checks::finished_of(rec),
            _ => None,
        }
    }

    /// Whether a step of the service is running: an order holds its lock, or its record holds
    /// an unfinished order.
    pub fn step_running(&self) -> bool {
        self.locked | matches!(self.record, RecordState::Unfinished(_))
    }
}

/// What the daemon holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DaemonStatus {
    /// This daemon's version.
    pub version: HostkeeperVersion,
    /// Every service with a package directory or a state directory, by name.
    pub services: Vec<ServiceStatus>,
}

/// Why [`Daemon::status`] could not list the services.
#[derive(Debug, thiserror::Error)]
#[error("cannot list the services under {}: {source}", dir.display())]
pub struct StatusError {
    pub dir: PathBuf,
    #[source]
    pub source: std::io::Error,
}

/// The daemon: one per host.
pub struct Daemon {
    ctx: Ctx,
}

impl Daemon {
    pub fn new(
        config: DaemonConfig,
        dirs: Dirs,
        ports: Ports,
        own_version: HostkeeperVersion,
    ) -> Daemon {
        let halts = HaltStore::new(&dirs.state);
        Daemon {
            ctx: Ctx {
                config,
                dirs,
                own_version,
                launcher: ports.launcher,
                packages: ports.packages,
                clock: ports.clock,
                locks: ServiceLocks::default(),
                step_lock: StepLock::default(),
                halts,
                runners: Mutex::default(),
                deadman_poll_secs: ports.deadman_poll_secs,
                stop: Arc::new(AtomicBool::new(false)),
            },
        }
    }

    /// What every verb works with.
    pub fn ctx(&self) -> &Ctx {
        &self.ctx
    }

    /// One order of the coordinator. A malformed order is refused naming its field, before any
    /// lock is taken.
    pub fn order(&self, order: &Order) -> Reply {
        match Verb::try_from(order) {
            Ok(verb) => self.execute(verb),
            Err(bad) => Reply::refused(Refusal::InvalidOrder, bad),
        }
    }

    /// One verb.
    pub fn execute(&self, verb: Verb) -> Reply {
        let ctx = &self.ctx;
        match verb {
            Verb::Update(order) => verbs::update::run(ctx, order),
            Verb::ForceRollback(order) => verbs::force_rollback::run(ctx, order),
            Verb::Install(order) => verbs::install::run(ctx, order),
            Verb::Uninstall(service) => verbs::uninstall::run(ctx, service),
            Verb::Force(order) => verbs::force::run(ctx, order),
            Verb::Abort(service) => verbs::abort::run(ctx, service),
            Verb::ClearHalt {
                service,
                halt_generation,
            } => verbs::clear_halt::run(ctx, service, halt_generation),
            Verb::ClearRecord(service) => verbs::clear_record::run(ctx, service),
        }
    }

    /// What the daemon holds about every service with a package directory or a state directory.
    pub fn status(&self) -> Result<DaemonStatus, StatusError> {
        let ctx = &self.ctx;
        let listed = |dir: &PathBuf| {
            installed::service_dirs(dir).map_err(|source| StatusError {
                dir: dir.clone(),
                source,
            })
        };
        let mut names = listed(&ctx.dirs.packages)?;
        names.extend(listed(&ctx.dirs.state)?);
        let services = names
            .into_values()
            .map(|service| ServiceStatus {
                running_version: installed::linked_version(&ctx.dirs.packages, &service, CURRENT),
                held_halt: ctx.halts.get(&service).ok(),
                locked: ctx.locks.holder(&service).is_some(),
                record: RecordState::of(ctx.store(&service).load()),
                service,
            })
            .collect();
        Ok(DaemonStatus {
            version: ctx.own_version,
            services,
        })
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.ctx.stop.store(true, Ordering::Release);
    }
}
