//! The executor: [`Daemon::order`] takes one order of the coordinator, decodes it into a
//! [`Verb`] and hands it to the verb's module under `verbs/`, which answers with a [`Reply`].
//! [`Daemon::status`] reports what the daemon holds for each service.
//!
//! The executor only dispatches: each verb is one module with one function taking the shared
//! [`Ctx`] and its typed arguments, so that a later verb fills one file and one arm. The one
//! check it makes itself is the lifecycle rule of `force` ([`checks::force_allowed`]), until the
//! `force` verb applies it.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_config::daemon::DaemonConfig;
use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_hook::Verdict;
use hostkeeper_placed::{Fs, PlacedError, Placer, Roots, UnitControl};
use hostkeeper_proto::hostkeeper::agent::v1::Order;
use hostkeeper_steps::record::{Loaded, Record, RecordError, State};
use hostkeeper_steps::{
    Clock, DeadMan, Finished, LaunchError, RecordStore, RunError, StepLauncher, StepRunner,
    UpdateOrder, Watch,
};

use crate::boot::{ALERT_JOURNAL, ALERT_UNSTOPPABLE, ALERT_WATCHER, alert_line};
use crate::checks;
use crate::halt::HaltStore;
use crate::installed::{self, CURRENT, Lifecycle};
use crate::locks::{Held, Package, ServiceGuard, ServiceLocks, StepLock};
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

/// The host's side of placing a package (design section 4.7, "Placed paths"): where units are
/// placed, the filesystem every placed path, `current` and a service's directories are read and
/// written through, and the write side of the service manager. `placed.json` and its lock are
/// in the directory above the packages directory (`/var/lib/hostkeeper` on a host).
#[derive(Clone)]
pub struct Placement {
    /// Where units are placed: `/etc/systemd/system` on a Linux host.
    pub unit_dir: PathBuf,
    /// The host's filesystem with every path spelt as the host spells it (`RealFs` at `/` on a
    /// host), so that the daemon's own directories are spelt the same in it.
    pub fs: Arc<dyn Fs + Send + Sync>,
    /// `daemon-reload` and the stop of a unit.
    pub units: Arc<dyn UnitControl + Send + Sync>,
}

/// What the daemon runs on: the ports of everything privileged, and the clock.
pub struct Ports {
    /// Runs one step of a hook.
    pub launcher: Arc<dyn StepLauncher>,
    /// Fetches, verifies, unpacks and seals packages.
    pub packages: Arc<dyn Packages>,
    /// Places packages into the system, and removes what an uninstall removes.
    pub placement: Placement,
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
    pub placement: Placement,
    pub clock: Arc<dyn Clock>,
    pub locks: ServiceLocks,
    pub step_lock: StepLock,
    pub halts: HaltStore,
    /// The post-success watchers, one per service at most.
    pub watchers: Watchers,
    runners: Mutex<BTreeMap<String, Arc<StepRunner>>>,
    /// How the last uninstall of each service ended, while the daemon runs: what `Status` reports
    /// for a service being removed.
    uninstalls: Mutex<BTreeMap<String, LastUninstall>>,
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

    /// The directories of placed-path linking: `placed.json` and its lock in the directory above
    /// the packages directory, units in the placement's unit directory.
    pub fn roots(&self) -> Roots {
        let packages = &self.dirs.packages;
        let state = packages.parent().unwrap_or(packages.as_path());
        Roots::new(state, &self.placement.unit_dir, packages)
    }

    /// The placer of this host.
    pub fn placer(&self) -> Placer<'_> {
        Placer::new(self.roots(), &*self.placement.fs, &*self.placement.units)
    }

    /// Where `service` is in its life (design section 6.1); `Err` when `placed.json` cannot be
    /// read, which says nothing of its tombstone.
    pub fn lifecycle(&self, service: &ServiceName) -> Result<Lifecycle, PlacedError> {
        let tombstoned = crate::uninstall::tombstoned(self, service)?;
        let record = RecordState::of(self.store(service).load());
        Ok(self.lifecycle_of(
            service,
            tombstoned,
            self.locks.holder(service).as_ref(),
            &record,
        ))
    }

    fn lifecycle_of(
        &self,
        service: &ServiceName,
        tombstoned: bool,
        holder: Option<&Held>,
        record: &RecordState,
    ) -> Lifecycle {
        let installed = installed::linked_version(&self.dirs.packages, service, CURRENT).is_some();
        let busy = holder.is_some() | matches!(record, RecordState::Unfinished(_));
        Lifecycle::of(tombstoned, installed, busy)
    }

    /// Keeps how the uninstall of `service`, the order's try `order_try` (0 for one the start
    /// resumed), ended, for `Status`; a reply without a verdict changes nothing, and an uninstall
    /// that succeeded is forgotten with the service.
    pub fn record_uninstall(&self, service: &ServiceName, reply: &Reply, order_try: u64) {
        let mut uninstalls = self
            .uninstalls
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match reply {
            Reply::Finished {
                verdict: Verdict::Succeeded { .. },
                ..
            } => {
                uninstalls.remove(service.as_str());
            }
            Reply::Finished {
                verdict, reason, ..
            } => {
                let finished = Finished {
                    verdict: verdict.clone(),
                    reason: reason.clone(),
                };
                uninstalls.insert(
                    service.to_string(),
                    LastUninstall {
                        finished,
                        order_try,
                    },
                );
            }
            Reply::Running | Reply::Refused { .. } | Reply::Interrupted { .. } => {}
        }
    }

    /// How the last uninstall of `service` ended, while the daemon runs.
    pub fn last_uninstall(&self, service: &ServiceName) -> Option<LastUninstall> {
        let uninstalls = self
            .uninstalls
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        uninstalls.get(service.as_str()).cloned()
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
    /// order, and a new order replaces the watcher (design section 6.4). Its switch places a
    /// first install ([`crate::place::first_install`]).
    pub fn runner(&self, service: &ServiceName) -> Arc<StepRunner> {
        drop(self.watchers.stop(service));
        let mut runners = self.runners.lock().unwrap_or_else(PoisonError::into_inner);
        let made = runners.entry(service.to_string()).or_insert_with(|| {
            let switch =
                crate::place::first_install(self.placement.clone(), self.roots(), self.own_version);
            let runner = StepRunner::new(
                Arc::clone(&self.launcher),
                Arc::clone(&self.clock),
                self.store(service),
            )
            .with_switch(switch);
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

/// How the last uninstall of a service ended, and the try of its order (0 for one the daemon's
/// start resumed).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LastUninstall {
    pub finished: Finished,
    pub order_try: u64,
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
    /// Where it is in its life.
    pub lifecycle: Lifecycle,
    /// How its last uninstall ended, while the daemon runs; `None` when none ran or it succeeded.
    pub uninstall: Option<LastUninstall>,
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
    /// Every service with a package directory, a state directory or a tombstone, by name.
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
                placement: ports.placement,
                clock: ports.clock,
                locks: ServiceLocks::default(),
                step_lock: StepLock::default(),
                halts,
                watchers: Watchers::default(),
                runners: Mutex::default(),
                uninstalls: Mutex::default(),
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
            Verb::Update(order) | Verb::ForceRollback(order) | Verb::Install(order) => {
                Some(order.service.clone())
            }
            Verb::Uninstall(order) => Some(order.service.clone()),
            _ => None,
        };
        let stuck = hooked
            .and_then(|service| self.ctx.locks.holder(&service).map(|held| (service, held)))
            .filter(|(_, held)| held.stuck);
        if let Some((service, held)) = stuck {
            return Reply::refused(
                Refusal::Busy,
                format!(
                    "a step of {service} that could not be stopped holds it, for {}",
                    held.holder
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
            Verb::Uninstall(order) => verbs::uninstall::run(ctx, order),
            Verb::Force(order) => match force_check(ctx, &order.package.service) {
                Ok(()) => verbs::force::run(ctx, order),
                Err(refused) => refused,
            },
            Verb::Abort(service) => verbs::abort::run(ctx, service),
            Verb::ClearHalt {
                service,
                halt_generation,
            } => verbs::clear_halt::run(ctx, service, halt_generation),
            Verb::ClearRecord(service) => verbs::clear_record::run(ctx, service),
        }
    }

    /// What the daemon holds about every service with a package directory, a state directory or
    /// a tombstone. A `placed.json` that cannot be read shows no tombstone (every order of a
    /// service is refused while it cannot be read).
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
        let tombstones = ctx
            .placer()
            .store()
            .load()
            .map(|record| record.uninstalling)
            .unwrap_or_default();
        names.extend(
            tombstones
                .keys()
                .filter_map(|name| ServiceName::new(name).ok().map(|s| (name.clone(), s))),
        );
        let services = names
            .into_values()
            .map(|service| {
                let holder = ctx.locks.holder(&service);
                let record = RecordState::of(ctx.store(&service).load());
                let tombstoned = tombstones.contains_key(service.as_str());
                ServiceStatus {
                    running_version: installed::linked_version(
                        &ctx.dirs.packages,
                        &service,
                        CURRENT,
                    ),
                    held_halt: ctx.halts.get(&service).ok(),
                    locked: holder.is_some(),
                    stuck: holder.as_ref().is_some_and(|held| held.stuck),
                    lifecycle: ctx.lifecycle_of(&service, tombstoned, holder.as_ref(), &record),
                    uninstall: ctx.last_uninstall(&service),
                    record,
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

/// The lifecycle rule of `force` ([`checks::force_allowed`]), applied before the verb: a force
/// of a service with no release installed, one being installed or one being uninstalled is
/// refused, and nothing changes. A `placed.json` that cannot be read refuses it too.
fn force_check(ctx: &Ctx, service: &ServiceName) -> Result<(), Reply> {
    let lifecycle = ctx
        .lifecycle(service)
        .map_err(|e| Reply::refused(Refusal::from(&e), e))?;
    checks::force_allowed(lifecycle).map_err(|refusal| {
        Reply::refused(refusal, format!("force of {service}: it is {lifecycle:?}"))
    })
}
