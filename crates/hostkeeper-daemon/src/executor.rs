//! The executor: [`Daemon::order`] takes one order of the coordinator, decodes it into a
//! [`Verb`] and hands it to the verb's module under `verbs/`, which answers with a [`Reply`].
//! [`Daemon::status`] reports what the daemon holds for each service; nothing on the host serves
//! it. [`Daemon::abort`] runs what the coordinator's `abort` order runs. A coordinator's
//! `clear_record` order is dispatched before anything reads the service's step record, so a
//! record that does not read is no bar to the order that clears it.
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
use hostkeeper_install::Announcement;
use hostkeeper_install::repository::Drift;
use hostkeeper_observe::{Observation, Observer};
use hostkeeper_placed::{Fs, PlacedError, Placer, Roots, UnitControl};
use hostkeeper_proto::hostkeeper::agent::v1::Order;
use hostkeeper_steps::record::{Loaded, Record, RecordError, State};
use hostkeeper_steps::{
    Clock, DeadMan, Finished, ForceVerdict, LaunchError, RecordStore, RunError, StepLauncher,
    StepRunner, UpdateOrder, Watch,
};

use crate::boot::{ALERT_INTERRUPTED, ALERT_JOURNAL, ALERT_UNSTOPPABLE, ALERT_WATCHER, alert_line};
use crate::builtin::BuiltinPorts;
use crate::checks;
use crate::halt::HaltStore;
use crate::installed::{self, CURRENT, Lifecycle};
use crate::locks::{Held, Package, ServiceGuard, ServiceLocks, StepLock};
use crate::node::NodeState;
use crate::observer;
use crate::packages::Packages;
use crate::prune::{ForceLockPoint, Prune};
use crate::refusal::Refusal;
use crate::repository::{Repository, RepositoryPorts};
use crate::selfupdate::{self, Restart};
use crate::trusted_root::RootReport;
use crate::verb::{HookedVerb, Verb};
use crate::verbs;
use crate::verbs::abort::Aborted;
use crate::verbs::clear_halt::Lifted;
use crate::verbs::force::SELF_SERVICE;
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
    /// The service manager: the units a `force` stops, starts and asks after, and
    /// `daemon-reload`.
    pub units: Arc<dyn UnitControl + Send + Sync>,
    /// Where placed-path linking works, and the filesystem it goes through.
    pub placement: Placement,
    /// The prune of the versions a confirmed switch leaves behind: at the end of an update that
    /// ends `Succeeded`, and at step 18 of a `force` that moved `current`.
    pub prune: Arc<dyn Prune>,
    /// The point of a `force` between its switch of `current` and its prune.
    pub force_lock_point: Arc<dyn ForceLockPoint>,
    /// Looks at how each installed service runs: what `Status` reports of it.
    pub observer: Arc<Observer>,
    /// The installers the daemon announces, decided once at its start
    /// ([`hostkeeper_install::announce`]); the heartbeat lists them, and the receipt check of an
    /// entry judges its installer against them, with the host's native installer once a set the
    /// coordinator pushed gave it an entry ([`Ctx::installers`]).
    pub announcement: Announcement,
    /// What the repository installer works with: the host's repository entries, the packages
    /// directory a repository version is staged in, the work directories and the installer
    /// lock ([`crate::repository`]).
    pub repository: Arc<RepositoryPorts>,
}

/// Where placed-path linking works (design section 4.7, "Placed paths"): the directories as the
/// host names them, and the filesystem every path is read and written through. The packages
/// directory of `roots`, as `fs` reaches it, is [`Dirs::packages`].
pub struct Placement {
    pub roots: Roots,
    pub fs: Arc<dyn Fs + Send + Sync>,
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
    pub units: Arc<dyn UnitControl + Send + Sync>,
    pub placement: Placement,
    pub prune: Arc<dyn Prune>,
    pub force_lock_point: Arc<dyn ForceLockPoint>,
    /// Observes the installed services; it takes none of the daemon's locks.
    pub observer: Arc<Observer>,
    /// The installers the daemon announced at its start ([`Ports::announcement`]);
    /// [`Ctx::installers`] adds the native one a pushed set may have made possible since.
    pub announcement: Announcement,
    /// What the repository installer works with ([`Ports::repository`]).
    pub repository_ports: Arc<RepositoryPorts>,
    /// The drift each service's last repository `activate` repaired, until its verdict takes it.
    repaired: Arc<Mutex<BTreeMap<String, Drift>>>,
    /// The counter of the repository installer's tool units ([`Repository::tool_runs`]).
    tool_runs: Arc<std::sync::atomic::AtomicU64>,
    pub locks: ServiceLocks,
    pub step_lock: StepLock,
    pub halts: HaltStore,
    /// The post-success watchers, one per service at most.
    pub watchers: Watchers,
    /// The host's enrollment and the heartbeat loop's link, as the loops last wrote them: what
    /// `Status` reports besides the services.
    pub node: NodeState,
    runners: Mutex<BTreeMap<String, Arc<StepRunner>>>,
    /// How the last uninstall of each service ended, while the daemon runs: what `Status` reports
    /// for a service being removed.
    uninstalls: Mutex<BTreeMap<String, LastUninstall>>,
    /// The record lock of each service's state directory, shared by every store made for it.
    record_locks: Mutex<BTreeMap<String, Arc<Mutex<()>>>>,
    stores: Option<StoreSeam>,
    alerts: AlertSink,
    deadman_poll_secs: Option<u64>,
    /// Ends the dead-men when the daemon is dropped.
    stop: Arc<AtomicBool>,
    /// The daemon's request to exit so that its unit starts a new binary.
    restart: Restart,
}

impl Ctx {
    /// The daemon's request to exit with status 0 after the follow-on of its own update.
    pub fn restart(&self) -> &Restart {
        &self.restart
    }

    /// The state directory of `service`.
    pub fn service_dir(&self, service: &ServiceName) -> PathBuf {
        self.dirs.state.join(service.as_str())
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

    /// The step record store of `service`, with the record lock every store of the service
    /// shares: the runner's writes and a `force`'s flag on the record take turns.
    pub fn store(&self, service: &ServiceName) -> RecordStore {
        let lock = {
            let mut locks = self
                .record_locks
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            Arc::clone(locks.entry(service.to_string()).or_default())
        };
        let store = RecordStore::new(&self.service_dir(service)).with_lock(lock);
        match &self.stores {
            Some(seam) => seam(service, store),
            None => store,
        }
    }

    /// The placed-path linker of this host.
    /// The repository installer with this daemon's ports ([`crate::repository`]).
    pub fn repository(&self) -> Repository {
        Repository {
            launcher: Arc::clone(&self.launcher),
            ports: Arc::clone(&self.repository_ports),
            packages: Arc::clone(&self.packages),
            clock: Arc::clone(&self.clock),
            roots: self.placement.roots.clone(),
            fs: Arc::clone(&self.placement.fs),
            units: Arc::clone(&self.units),
            max_package_bytes: self.config.bounds.max_package_bytes,
            repaired: Arc::clone(&self.repaired),
            tool_runs: Arc::clone(&self.tool_runs),
            log: Arc::clone(&self.alerts),
        }
    }

    /// The built-in steps over this daemon's ports ([`crate::builtin`]).
    pub fn builtin_ports(&self) -> BuiltinPorts {
        BuiltinPorts {
            units: Arc::clone(&self.units),
            clock: Arc::clone(&self.clock),
            observer: Arc::clone(&self.observer),
            packages: self.dirs.packages.clone(),
            binaries: crate::selfupdate::binary_dir(&self.dirs.state),
            own: self.own_version,
        }
    }

    /// The installers the daemon announces now: those decided at its start, and the host's
    /// native one once it has an entry in use ([`crate::repository_push::installers`]).
    pub fn installers(&self) -> Vec<hostkeeper_release::Installer> {
        crate::repository_push::installers(&self.announcement, &self.repository_ports)
    }

    pub fn placer(&self) -> Placer<'_> {
        Placer::new(
            self.placement.roots.clone(),
            &*self.placement.fs,
            &*self.units,
        )
    }

    /// Logs an alert: error level, with its `alert=` field.
    pub fn alert(&self, alert: &str, detail: &str) {
        (self.alerts)(&alert_line(alert, detail));
    }

    /// Logs a line of the enrollment or heartbeat loop ([`crate::links`]) through the same sink
    /// as the alerts, as `hostkeeper daemon: <line>`: the loops say themselves which of their
    /// lines are errors.
    pub fn log(&self, line: &str) {
        (self.alerts)(&format!("hostkeeper daemon: {line}"));
    }

    /// The step runner of `service` when one was made, without stopping a watcher: the runner an
    /// `abort` asks to end its update ([`hostkeeper_steps::StepRunner::abort`]).
    pub fn made_runner(&self, service: &ServiceName) -> Option<Arc<StepRunner>> {
        let runners = self.runners.lock().unwrap_or_else(PoisonError::into_inner);
        runners.get(service.as_str()).cloned()
    }

    /// The step runner of `service`, made with its dead-man on first use. A watcher of the
    /// service is stopped first, and waited for: whoever asks for the runner is about to run an
    /// order, and a new order replaces the watcher (design section 6.4). Its switch places every
    /// hooked order ([`crate::place::hooked_switch`]).
    pub fn runner(&self, service: &ServiceName) -> Arc<StepRunner> {
        drop(self.watchers.stop(service));
        let mut runners = self.runners.lock().unwrap_or_else(PoisonError::into_inner);
        let made = runners.entry(service.to_string()).or_insert_with(|| {
            let switch = crate::place::retried(
                crate::place::hooked_switch(
                    self.placement.roots.clone(),
                    Arc::clone(&self.placement.fs),
                    Arc::clone(&self.units),
                    self.own_version,
                    self.repository(),
                ),
                Arc::clone(&self.clock),
                Arc::clone(&self.alerts),
            );
            let runner = StepRunner::new(
                Arc::clone(&self.launcher),
                Arc::clone(&self.clock),
                self.store(service),
            )
            .with_switch(switch)
            .with_builtin(crate::builtin::builtin(self.builtin_ports()));
            let runner = selfupdate::runner_for(runner, service);
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
        let runner = selfupdate::runner_for(runner, service);
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

/// A refusal of `abort`, `clear_halt` or `clear_record`: its token, and a text for the log.
/// Nothing was written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refused {
    pub refusal: Refusal,
    pub detail: String,
}

impl Refused {
    pub fn new(refusal: Refusal, detail: impl ToString) -> Refused {
        Refused {
            refusal,
            detail: detail.to_string(),
        }
    }
}

impl From<Refused> for Reply {
    fn from(refused: Refused) -> Reply {
        Reply::Refused {
            refusal: refused.refusal,
            detail: refused.detail,
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

/// How a `force` ended, with the order it answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForcedResult {
    /// The coordinator's id of the order, when it gave one.
    pub order_id: Option<String>,
    pub order_try: u64,
    /// The version the force switched to.
    pub target: String,
    pub verdict: ForceVerdict,
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
    /// How the service runs now, as the service manager, the running executable and its health
    /// check say ([`crate::observer`]); `None` when it has no `current`, or the manifest of the
    /// version `current` names does not load.
    pub observation: Option<Observation>,
}

impl ServiceStatus {
    /// How the last order ended, `None` when none has or one is under way.
    pub fn last_result(&self) -> Option<Finished> {
        match &self.record {
            RecordState::Finished(rec) => checks::finished_of(rec),
            _ => None,
        }
    }

    /// The verdict of the last order, when it was a `force` that ended (design section 5.6, step
    /// 17): what the heartbeat reports as a forced verdict (`UpdateResult.forced`), with the
    /// order's id and try, beside [`ServiceStatus::last_result`]. The record keeps it until the
    /// next order of the service, so every read until then returns it again.
    pub fn forced(&self) -> Option<ForcedResult> {
        let RecordState::Finished(rec) = &self.record else {
            return None;
        };
        let force = rec.force.as_ref()?;
        Some(ForcedResult {
            order_id: force.order_id.clone(),
            order_try: force.order_try,
            target: force.target_version.clone(),
            verdict: force.verdict?,
        })
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
    /// The Sigstore trusted root the package store checks attestations with: its age and the
    /// reason it needs attention ([`Packages::trusted_root`]); `None` from a store that reports
    /// none.
    pub trusted_root: Option<RootReport>,
}

/// Why [`Daemon::status`] could not list the services.
#[derive(Debug, thiserror::Error)]
#[error("cannot list the services under {}: {source}", dir.display())]
pub struct StatusError {
    pub dir: PathBuf,
    #[source]
    pub source: std::io::Error,
}

/// The answer to a coordinator's `abort` of one order and try; `None` while that abort is under
/// way.
struct AbortTry {
    order_id: String,
    order_try: u64,
    answer: Option<Reply>,
}

/// The daemon: one per host.
pub struct Daemon {
    ctx: Ctx,
    /// The last coordinator's `abort` of each service, by try: a resend is answered from it.
    abort_tries: Mutex<BTreeMap<String, AbortTry>>,
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
                units: ports.units,
                placement: ports.placement,
                prune: ports.prune,
                force_lock_point: ports.force_lock_point,
                observer: ports.observer,
                announcement: ports.announcement,
                repository_ports: ports.repository,
                repaired: Arc::default(),
                tool_runs: Arc::new(std::sync::atomic::AtomicU64::new(
                    crate::repository::first_tool_run(),
                )),
                locks: ServiceLocks::default(),
                step_lock: StepLock::default(),
                halts,
                watchers: Watchers::default(),
                node: NodeState::default(),
                runners: Mutex::default(),
                uninstalls: Mutex::default(),
                record_locks: Mutex::default(),
                stores: None,
                alerts: Arc::new(|line: &str| eprintln!("{line}")),
                deadman_poll_secs: ports.deadman_poll_secs,
                stop: Arc::new(AtomicBool::new(false)),
                restart: Restart::default(),
            },
            abort_tries: Mutex::default(),
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

    /// The observer of the installed services.
    pub fn observer(&self) -> &Arc<Observer> {
        &self.ctx.observer
    }

    /// One order of the coordinator. A malformed order is refused naming its field, before any
    /// lock is taken.
    pub fn order(&self, order: &Order) -> Reply {
        match Verb::try_from(order) {
            Ok(Verb::Abort(service)) => self.abort_order(&service, &order.order_id, order.r#try),
            Ok(verb) => self.execute(verb),
            Err(bad) => Reply::refused(Refusal::InvalidOrder, bad),
        }
    }

    /// A coordinator's `abort`, the order `order_id`'s try `order_try`: a resend of that order and
    /// try is answered from the first answer (`Running` while it is under way) and stops nothing
    /// more; another order, or another try, is a new abort.
    fn abort_order(&self, service: &ServiceName, order_id: &str, order_try: u64) -> Reply {
        let tries = || {
            self.abort_tries
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
        };
        {
            let mut tries = tries();
            let first = tries
                .get(service.as_str())
                .filter(|last| (last.order_id == order_id) & (last.order_try == order_try));
            if let Some(last) = first {
                return last.answer.clone().map_or(Reply::Running, resent);
            }
            let under_way = AbortTry {
                order_id: order_id.to_owned(),
                order_try,
                answer: None,
            };
            tries.insert(service.to_string(), under_way);
        }
        let reply = self.execute(Verb::Abort(service.clone()));
        let answered = AbortTry {
            order_id: order_id.to_owned(),
            order_try,
            answer: Some(reply.clone()),
        };
        tries().insert(service.to_string(), answered);
        reply
    }

    /// `abort` (design sections 6.3 and 8.5): stops the service's running step and ends its
    /// update `Finished{Halt}`, taking neither the service's lock nor the step lock, as the
    /// coordinator's order does.
    pub fn abort(&self, service: &ServiceName) -> Result<Aborted, Refused> {
        verbs::abort::abort(&self.ctx, service)
    }

    /// `clear_halt` (design sections 5.4 and 6.2): lifts the service's halt for a halt generation
    /// strictly above the one it is held under.
    pub fn clear_halt(&self, service: &ServiceName, generation: u64) -> Result<Lifted, Refused> {
        verbs::clear_halt::clear_halt(&self.ctx, service, generation)
    }

    /// One verb. A step that could not be stopped is an alert (`unstoppable_unit`): its service
    /// stays locked and its record `running`, and every hooked order of the service, the same
    /// update's resend included, is `busy` until `abort` clears it. Any other hooked order that
    /// stops without a verdict, whatever its reason, is an alert too (`order_interrupted`), with
    /// its service, its reason and what failed: the coordinator keeps only the reason, so this
    /// line is where the host says why (a switch that could not relink, a launcher that failed).
    pub fn execute(&self, verb: Verb) -> Reply {
        let hooked = match &verb {
            Verb::Update(order) | Verb::ForceRollback(order) | Verb::Install(order) => {
                Some(order.service.clone())
            }
            Verb::Uninstall(order) => Some(order.service.clone()),
            _ => None,
        };
        let stuck = hooked
            .clone()
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
        match (&reply, &hooked) {
            (
                Reply::Interrupted {
                    refusal: Refusal::Unstopped,
                    detail,
                },
                _,
            ) => self.ctx.alert(ALERT_UNSTOPPABLE, detail),
            (Reply::Interrupted { refusal, detail }, Some(service)) => self.ctx.alert(
                ALERT_INTERRUPTED,
                &format!("service={service}: {}: {detail}", refusal.token()),
            ),
            _ => {}
        }
        reply
    }

    fn dispatch(&self, verb: Verb) -> Reply {
        let ctx = &self.ctx;
        match verb {
            Verb::Update(order) if order.service.as_str() == SELF_SERVICE => {
                selfupdate::run(ctx, HookedVerb::Update, order)
            }
            Verb::Install(order) if order.service.as_str() == SELF_SERVICE => {
                selfupdate::run(ctx, HookedVerb::Install, order)
            }
            Verb::ForceRollback(order) if order.service.as_str() == SELF_SERVICE => {
                selfupdate::run(ctx, HookedVerb::ForceRollback, order)
            }
            Verb::Update(order) => verbs::update::run(ctx, order),
            Verb::ForceRollback(order) => verbs::force_rollback::run(ctx, order),
            Verb::Install(order) => verbs::install::run(ctx, order),
            Verb::Uninstall(order) => verbs::uninstall::run(ctx, order),
            Verb::Force(order) => verbs::force::run(ctx, order),
            Verb::Abort(service) => verbs::abort::run(ctx, service),
            Verb::ClearHalt {
                service,
                halt_generation,
            } => verbs::clear_halt::run(ctx, service, halt_generation),
            Verb::ClearRecord(service) => verbs::clear_record::run(ctx, &service),
        }
    }

    /// What the daemon holds about every service with a package directory, a state directory or
    /// a tombstone. A `placed.json` that cannot be read shows no tombstone (every order of a
    /// service is refused while it cannot be read). Each service with a `current` is observed
    /// ([`observer::observe`]); the read takes no service lock and no step lock, so it does not
    /// wait for a running step.
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
                    observation: observer::observe(
                        &ctx.observer,
                        &ctx.dirs.packages,
                        &selfupdate::binary_dir(&ctx.dirs.state),
                        &ctx.own_version,
                        &service,
                    ),
                    record,
                    service,
                }
            })
            .collect();
        Ok(DaemonStatus {
            version: ctx.own_version,
            services,
            trusted_root: ctx.packages.trusted_root(),
        })
    }
}

/// An answer given again for a resend: a verdict says so.
fn resent(reply: Reply) -> Reply {
    match reply {
        Reply::Finished {
            verdict, reason, ..
        } => Reply::Finished {
            verdict,
            reason,
            resent: true,
        },
        other => other,
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.ctx.stop.store(true, Ordering::Release);
    }
}
