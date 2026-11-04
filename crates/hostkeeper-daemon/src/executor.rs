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
use hostkeeper_steps::{
    Clock, DeadMan, Finished, LaunchError, RecordStore, RunError, StepLauncher, StepRunner,
    UpdateOrder, Watch,
};

use crate::boot::{ALERT_JOURNAL, ALERT_UNSTOPPABLE, ALERT_WATCHER, alert_line};
use crate::checks;
use crate::halt::HaltStore;
use crate::installed::{self, CURRENT};
use crate::locks::{Package, ServiceGuard, ServiceLocks, StepLock};
use crate::packages::Packages;
use crate::refusal::Refusal;
use crate::verb::Verb;
use crate::verbs;
use crate::watchers::{STOPPED, SlicedClock, WatchEnd, Watchers, Yielding};

/// A test's way to change every step record store the daemon makes, as it is made: to stop the
/// daemon at a write ([`RecordStore::with_crash_gate`]) or fail one
/// ([`RecordStore::with_write_fault`]). It can only make a store fail.
pub type StoreSeam = Arc<dyn Fn(&ServiceName, RecordStore) -> RecordStore + Send + Sync>;

/// Where the daemon's alerts go, one line each ([`alert_line`]): standard error, unless a test
/// gives another sink ([`Daemon::with_alerts`]).
pub type AlertSink = Arc<dyn Fn(&str) + Send + Sync>;

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
    /// The post-success watchers, one per service at most.
    pub watchers: Watchers,
    runners: Mutex<BTreeMap<String, Arc<StepRunner>>>,
    stores: Option<StoreSeam>,
    alerts: AlertSink,
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
        let store = RecordStore::new(&self.service_dir(service));
        match &self.stores {
            Some(seam) => seam(service, store),
            None => store,
        }
    }

    /// Logs an alert: error level, with its `alert=` field.
    pub fn alert(&self, alert: &str, detail: &str) {
        (self.alerts)(&alert_line(alert, detail));
    }

    /// The step runner of `service`, made with its dead-man on first use. A watcher of the
    /// service is stopped first, and waited for: whoever asks for the runner is about to run an
    /// order, and a new order replaces the watcher (design section 6.4).
    pub fn runner(&self, service: &ServiceName) -> Arc<StepRunner> {
        drop(self.watchers.stop(service));
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

    /// Starts the post-success watcher of `service` for `order` on a thread of its own, with a
    /// runner of its own that a new order can stop ([`crate::watchers`]). `false`, and nothing
    /// started, while a watcher of the service runs.
    ///
    /// How the watcher ends is handled on its thread: a `verify` the dead-man could not stop
    /// locks the service for good, as `package`, unless an order holds its lock (that order's
    /// run stops the unit first), and is an alert (`unstoppable_unit`); a record that cannot be
    /// written (`journal_unavailable`) and any other stop without a verdict
    /// (`watcher_interrupted`) are alerts. A watcher stopped by a new order or the daemon's end is
    /// not.
    pub fn start_watcher(
        &self,
        service: &ServiceName,
        order: UpdateOrder,
        package: Package,
    ) -> bool {
        let stop = Arc::new(AtomicBool::new(false));
        let launcher = Yielding {
            inner: Arc::clone(&self.launcher),
            stop: Arc::clone(&stop),
        };
        let clock = SlicedClock {
            inner: Arc::clone(&self.clock),
            stop: Arc::clone(&stop),
        };
        let runner = StepRunner::new(Arc::new(launcher), Arc::new(clock), self.store(service));
        self.watch(runner.watch());
        let (locks, alerts, name) = (
            self.locks.clone(),
            Arc::clone(&self.alerts),
            service.clone(),
        );
        let supervise = move |end: &WatchEnd| {
            let alert = |kind: &str, error: &dyn std::fmt::Display| {
                alerts(&alert_line(kind, &format!("service={name}: {error}")));
            };
            match end {
                Ok(_) => {}
                Err(RunError::Launch(LaunchError::Launcher(why))) if why == STOPPED => {}
                Err(error @ RunError::Unstopped { .. }) => {
                    locks
                        .take(&name, package)
                        .into_iter()
                        .for_each(ServiceGuard::keep);
                    alert(ALERT_UNSTOPPABLE, error);
                }
                Err(RunError::Record(error)) => alert(ALERT_JOURNAL, error),
                Err(error) => alert(ALERT_WATCHER, error),
            }
        };
        self.watchers
            .start(service, runner, order, stop, Box::new(supervise))
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
    /// The lock is held by a step that could not be stopped: it stays held until `abort`.
    pub stuck: bool,
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
                watchers: Watchers::default(),
                runners: Mutex::default(),
                stores: None,
                alerts: Arc::new(|line: &str| eprintln!("{line}")),
                deadman_poll_secs: ports.deadman_poll_secs,
                stop: Arc::new(AtomicBool::new(false)),
            },
        }
    }

    /// The same daemon, every step record store it makes changed by `seam` as it is made: the
    /// seam of the tests that stop the daemon at a write of a record, or fail one.
    pub fn with_stores(mut self, seam: StoreSeam) -> Daemon {
        self.ctx.stores = Some(seam);
        self
    }

    /// The same daemon, its alerts sent to `sink` in place of standard error: the seam of the
    /// tests that read what the daemon alerted.
    pub fn with_alerts(mut self, sink: AlertSink) -> Daemon {
        self.ctx.alerts = sink;
        self
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

    /// One verb. A step that could not be stopped is an alert (`unstoppable_unit`): its service
    /// stays locked and its record `running`, and every hooked order of the service, the same
    /// update's resend included, is `busy` until `abort` clears it.
    pub fn execute(&self, verb: Verb) -> Reply {
        let hooked = match &verb {
            Verb::Update(order) | Verb::ForceRollback(order) => Some(order.service.clone()),
            _ => None,
        };
        let stuck = hooked
            .and_then(|service| self.ctx.locks.holder(&service).map(|held| (service, held)))
            .filter(|(_, held)| held.stuck);
        if let Some((service, held)) = stuck {
            return Reply::refused(
                Refusal::Busy,
                format!(
                    "a step of {service} that could not be stopped holds it, for an order to {} it to {}",
                    held.package.verb.name(),
                    held.package.version
                ),
            );
        }
        let reply = self.dispatch(verb);
        if let Reply::Interrupted {
            refusal: Refusal::Unstopped,
            detail,
        } = &reply
        {
            self.ctx.alert(ALERT_UNSTOPPABLE, detail);
        }
        reply
    }

    fn dispatch(&self, verb: Verb) -> Reply {
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
            .map(|service| {
                let holder = ctx.locks.holder(&service);
                ServiceStatus {
                    running_version: installed::linked_version(
                        &ctx.dirs.packages,
                        &service,
                        CURRENT,
                    ),
                    held_halt: ctx.halts.get(&service).ok(),
                    locked: holder.is_some(),
                    stuck: holder.is_some_and(|held| held.stuck),
                    record: RecordState::of(ctx.store(&service).load()),
                    service,
                }
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
