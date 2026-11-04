//! The loop: one heartbeat per period, and what the reply asks for.
//!
//! Each heartbeat, in this order:
//!
//! 1. The verdict of the order running on the worker, if it ended, becomes the service's
//!    verdict to send, and its deferral is written or cleared.
//! 2. The certificate source is read. With no pair the loop says once what it waits for, and
//!    makes no call; the first call after a pair appears is at the next period.
//! 3. The facts are built from the step records and the observer: every installed service, its
//!    version, health and running update, its verdict not yet acknowledged, and its deferral when
//!    the last reply advertised `deferred-v1`. A service seen for the first time since the loop
//!    started takes the verdict its step record kept, so a restart resends it.
//! 4. [`fit`] keeps the bound; a heartbeat over it is refused and logged, never sent cut short.
//! 5. The heartbeat goes out through [`CoordinatorLink::heartbeat`], the one outbound call.
//! 6. From the reply: the advertised features replace the last ones; a verdict whose order id and
//!    try the reply acknowledges is not sent again; with `halt-v1`, a halt generation newer than
//!    the held one is raised in [`HeldHalts`]; and the order goes to the executor.
//!
//! **Orders.** One at a time, on a worker of the [`Dispatch`], so the loop keeps beating, and
//! reports the running target as `updating`, while a step runs. The same order (id and try) is
//! not started twice. An order that arrives while another runs is not started, and its verdict
//! is `busy`; it takes the place of a verdict of its service not yet acknowledged, because a
//! heartbeat carries one verdict per service. A forced order runs only while the reply advertises `forced-v1`; an update whose
//! target a deferral declines runs only once the retry time has come, by the [`UnixClock`].
//! Everything else about an order (its checks, the try rules) is the executor's.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use hostkeeper_config::names::{ServiceName, Version};

use crate::beat::{ANNOUNCED, Feature, Features, Heartbeat, Observed, Platform, ServiceFacts, fit};
use crate::deferral::{Deferral, Deferrals};
use crate::names::Reason;
use crate::order::{DeferredReport, Order, OrderRef, OrderResult, Outcome, Reply};
use crate::ports::{
    CertSource, CertState, CoordinatorLink, Dispatch, HeldHalts, LinkError, Log, OrderSink,
    RecordedService, ServiceObserver, ServiceRecords, Sleeper, Tick, UnixClock,
};

/// The period when the daemon is given none, in seconds.
pub const DEFAULT_HEARTBEAT_SECS: u64 = 15;

/// The loop's flag on the daemon's command line.
#[derive(Clone, Debug, PartialEq, Eq, clap::Args)]
pub struct LoopFlags {
    /// Seconds between two heartbeats, at least 1.
    #[arg(
        long = "heartbeat-secs",
        default_value_t = DEFAULT_HEARTBEAT_SECS,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    pub heartbeat_secs: u64,
}

/// What the loop is told about the daemon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoopConfig {
    pub platform: Platform,
    /// The daemon's own version.
    pub agent_version: Version,
    /// The directory of the services' step-record directories, where the deferrals live.
    pub state_dir: PathBuf,
    /// Seconds between two heartbeats.
    pub period_secs: u64,
}

/// Everything the loop talks to.
pub struct Ports {
    pub link: Arc<dyn CoordinatorLink>,
    pub certs: Arc<dyn CertSource>,
    pub records: Arc<dyn ServiceRecords>,
    pub observer: Arc<dyn ServiceObserver>,
    pub orders: Arc<dyn OrderSink>,
    pub dispatch: Arc<dyn Dispatch>,
    pub halts: Arc<dyn HeldHalts>,
    pub clock: Arc<dyn UnixClock>,
    pub log: Arc<dyn Log>,
}

/// What one heartbeat came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BeatOutcome {
    /// Sent, and the reply was taken.
    Sent,
    /// No certificate: nothing was sent.
    NotEnrolled,
    /// The step records could not be read: nothing was sent.
    NoFacts,
    /// Not sent, or sent with no reply.
    NotSent(LinkError),
}

/// The order on the worker.
struct Running {
    order: OrderRef,
    service: ServiceName,
    target: Version,
    done: Receiver<OrderResult>,
}

/// The heartbeat loop of one daemon.
pub struct Heartbeater {
    config: LoopConfig,
    ports: Ports,
    deferrals: Deferrals,
    /// The features of the last reply.
    advertised: Features,
    /// The verdict to send, by service, until it is acknowledged.
    resend: BTreeMap<String, OrderResult>,
    /// The last order started, by service: the same id and try is not started again.
    started: BTreeMap<String, OrderRef>,
    /// The services whose step record was read since the loop started.
    seen: BTreeSet<String>,
    running: Option<Running>,
    /// The path the loop said it waits for, so that it says so once.
    waiting_for: Option<PathBuf>,
}

impl Heartbeater {
    pub fn new(config: LoopConfig, ports: Ports) -> Heartbeater {
        let deferrals = Deferrals::new(&config.state_dir);
        Heartbeater {
            config,
            ports,
            deferrals,
            advertised: Features::default(),
            resend: BTreeMap::new(),
            started: BTreeMap::new(),
            seen: BTreeSet::new(),
            running: None,
            waiting_for: None,
        }
    }

    /// Beats now and after every period, until `sleeper` says stop. An order running on a
    /// worker when it stops goes on; its verdict is in its step record.
    pub fn run(mut self, sleeper: &dyn Sleeper) {
        loop {
            self.beat_once();
            if sleeper.sleep(self.config.period_secs) == Tick::Stop {
                return;
            }
        }
    }

    /// One heartbeat, and what its reply asks for.
    pub fn beat_once(&mut self) -> BeatOutcome {
        self.collect();
        let identity = match self.ports.certs.current() {
            CertState::Enrolled(identity) => {
                self.waiting_for = None;
                identity
            }
            CertState::NotEnrolled { waiting_for } => {
                if self.waiting_for.as_ref() != Some(&waiting_for) {
                    self.log(&format!(
                        "not enrolled: waiting for {}",
                        waiting_for.display()
                    ));
                    self.waiting_for = Some(waiting_for);
                }
                return BeatOutcome::NotEnrolled;
            }
        };
        let beat = match self.facts() {
            Ok(beat) => beat,
            Err(e) => {
                self.log(&format!(
                    "error: no heartbeat: the installed services cannot be read: {e}"
                ));
                return BeatOutcome::NoFacts;
            }
        };
        let answered = fit(beat)
            .map_err(LinkError::TooLarge)
            .and_then(|fitted| self.ports.link.heartbeat(&identity, &fitted.beat));
        match answered {
            Ok(reply) => {
                self.take(reply);
                BeatOutcome::Sent
            }
            Err(e) => {
                self.log(&format!("error: {e}"));
                BeatOutcome::NotSent(e)
            }
        }
    }

    fn log(&self, line: &str) {
        self.ports.log.line(line);
    }

    /// Takes the verdict of the running order, if it ended.
    fn collect(&mut self) {
        let Some(running) = self.running.take() else {
            return;
        };
        match running.done.try_recv() {
            Ok(result) => self.finish(&running.service, result),
            Err(TryRecvError::Empty) => self.running = Some(running),
            Err(TryRecvError::Disconnected) => self.log(&format!(
                "error: order {} try {} of {} ended with no verdict",
                running.order.id.as_str(),
                running.order.order_try,
                running.service
            )),
        }
    }

    /// The verdict of an order that ran: sent until acknowledged; a deferral is written, any
    /// other outcome clears it.
    fn finish(&mut self, service: &ServiceName, result: OrderResult) {
        let now = self.ports.clock.unix_secs();
        let kept = match result.outcome {
            Outcome::Deferred { retry_after_secs } => {
                let reason = result.reason.clone().unwrap_or_else(Reason::deferred);
                let deferral =
                    Deferral::starting(result.target.clone(), reason, now, retry_after_secs);
                self.deferrals.save(service, &deferral)
            }
            Outcome::Succeeded | Outcome::Failed => self.deferrals.clear(service),
        };
        if let Err(e) = kept {
            self.log(&format!(
                "error: the deferral of {service} is not written: {e}"
            ));
        }
        self.resend.insert(service.as_str().to_owned(), result);
    }

    /// The heartbeat: every installed service, from its step record and the observer.
    fn facts(&mut self) -> io::Result<Heartbeat> {
        let installed = self.ports.records.installed()?;
        let now = self.ports.clock.unix_secs();
        let mut services = Vec::with_capacity(installed.len());
        for recorded in installed {
            let RecordedService {
                name,
                version,
                updating,
                last_result,
            } = recorded;
            let key = name.as_str().to_owned();
            if self.seen.insert(key.clone())
                && let Some(result) = last_result
            {
                self.started.insert(key.clone(), result.order.clone());
                self.resend.insert(key.clone(), result);
            }
            let observation = self.ports.observer.observe(&name);
            let running = self
                .running
                .as_ref()
                .filter(|running| running.service == name)
                .map(|running| running.target.clone());
            let deferred = self
                .advertised
                .has(Feature::DeferredV1)
                .then(|| self.deferral_report(&name, now))
                .flatten();
            services.push(ServiceFacts {
                version,
                health: observation.health,
                updating: running.or(updating),
                last_update: self
                    .resend
                    .get(&key)
                    .map(|result| result.shaped(&self.advertised)),
                deferred,
                health_detail: observation.health_detail,
                observed: Some(Observed {
                    liveness: observation.liveness,
                    version: observation.version,
                }),
                name,
            });
        }
        Ok(Heartbeat {
            platform: self.config.platform,
            agent_version: self.config.agent_version.clone(),
            features: ANNOUNCED.to_vec(),
            services,
        })
    }

    fn deferral_report(&self, service: &ServiceName, now: u64) -> Option<DeferredReport> {
        self.deferral(service, now)
            .and_then(|deferral| deferral.report(now))
    }

    /// The deferral of `service`. A file that cannot be used is logged and removed: a deferral
    /// only holds a target back for a while, so going on without it is safe.
    fn deferral(&self, service: &ServiceName, now: u64) -> Option<Deferral> {
        match self.deferrals.load(service, now) {
            Ok(found) => found,
            Err(e) => {
                self.log(&format!(
                    "error: the deferral of {service} is not used and is removed: {e}"
                ));
                if let Err(e) = self.deferrals.clear(service) {
                    self.log(&format!(
                        "error: the deferral of {service} is not removed: {e}"
                    ));
                }
                None
            }
        }
    }

    /// What the reply asks for.
    fn take(&mut self, reply: Reply) {
        let Reply {
            order,
            features,
            halt_generations,
            acknowledged,
        } = reply;
        self.advertised = Features::of(&features);
        self.resend
            .retain(|_, result| !acknowledged.contains(&result.order));
        if self.advertised.has(Feature::HaltV1) {
            for (service, generation) in &halt_generations {
                self.raise_halt(service, *generation);
            }
        }
        if let Some(order) = order {
            self.dispatch(order);
        }
    }

    /// Raises the held halt generation of `service` to `generation` when it is newer: an equal
    /// or older one lifts nothing. The held value is read from the store each time, never kept.
    fn raise_halt(&self, service: &ServiceName, generation: u64) {
        if generation > self.ports.halts.held(service)
            && let Err(e) = self.ports.halts.raise(service, generation)
        {
            self.log(&format!(
                "error: halt generation {generation} of {service} is not written: {e}"
            ));
        }
    }

    /// Hands `order` to the executor, on a worker, unless it must not run now.
    fn dispatch(&mut self, order: Order) {
        let reference = order.reference();
        let service = order.target().service.clone();
        let key = service.as_str().to_owned();
        let named = format!(
            "order {} try {} of {service}",
            reference.id.as_str(),
            reference.order_try
        );
        if let Some(running) = &self.running {
            if running.order != reference {
                self.log(&format!(
                    "{named} is not started: busy with order {} try {} of {}",
                    running.order.id.as_str(),
                    running.order.order_try,
                    running.service
                ));
                self.resend.insert(key, OrderResult::busy(&order));
            }
            return;
        }
        if self.started.get(&key) == Some(&reference) {
            return;
        }
        if order.is_forced() & !self.advertised.has(Feature::ForcedV1) {
            self.log(&format!(
                "{named} is not run: it is forced and the coordinator does not advertise forced-v1"
            ));
            return;
        }
        let now = self.ports.clock.unix_secs();
        let declined = (!order.is_forced() & self.advertised.has(Feature::DeferredV1))
            .then(|| self.deferral(&service, now))
            .flatten()
            .filter(|deferral| deferral.declines(&order.target().version, now));
        if let Some(deferral) = declined {
            self.log(&format!(
                "{named} is not run: deferred until unix second {}",
                deferral.retry_after_unix
            ));
            return;
        }
        let (sender, receiver) = mpsc::channel();
        let orders = Arc::clone(&self.ports.orders);
        let job_order = order.clone();
        self.ports.dispatch.start(Box::new(move || {
            let _ = sender.send(orders.execute(&job_order));
        }));
        self.started.insert(key, reference.clone());
        self.running = Some(Running {
            order: reference,
            service,
            target: order.target().version.clone(),
            done: receiver,
        });
    }
}
