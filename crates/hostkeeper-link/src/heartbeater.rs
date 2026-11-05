//! The loop: one heartbeat per period, and what the reply asks for.
//!
//! Each heartbeat, in this order:
//!
//! 1. A deferral write that failed earlier is tried again. The verdict of the order running on
//!    the worker, if it ended, joins the service's verdicts to send; a deferral is written, and
//!    a hooked verdict on the deferred target ends the deferral.
//! 2. The certificate source is read. With no pair the loop says once what it waits for, and
//!    makes no call; the first call after a pair appears is at the next period.
//! 3. The facts are built from the step records and the observer: every installed service, its
//!    version, health and running update, its newest verdict not yet acknowledged, and its
//!    deferral when the last reply advertised `deferred-v1`. Step records that name a service
//!    twice send nothing. A service listed again after it was not takes the verdict its step
//!    record kept, unless that verdict was acknowledged, so a restart resends it; a kept
//!    deferred verdict with no deferral of its target on record starts one.
//! 4. [`fit`] keeps the bound; a heartbeat over it is refused and logged, never sent cut short.
//! 5. The heartbeat goes out through [`CoordinatorLink::heartbeat`], the one outbound call.
//! 6. From the reply: the advertised features replace the last ones; a verdict whose service,
//!    order id and try the reply acknowledges is not sent again; with `halt-v1`, a halt
//!    generation newer than the held one is raised in [`HeldHalts`]; and the order goes to the
//!    executor.
//!
//! **Verdicts.** A service may have several verdicts not yet acknowledged (an order's, then a
//! `busy` answer, then another order's). A heartbeat carries one per service: the newest. Once
//! it is acknowledged the one before it is sent, so every verdict is sent until counted. To a
//! coordinator that advertises `verdict-acks-v1`, which reads the last verdict it acknowledged
//! as the host's report from then on, they go oldest first instead, so the newest is the last
//! it hears, and an ack also ends every verdict of the service older than the one it names (the
//! newest, sent before the reply that advertised the feature): an older one heard after it
//! would stand in its place. Across
//! a restart only the step record's verdict, the last order's, comes back: the coordinator
//! orders a host only when it has no outstanding order (design 8.1), and after its own restart
//! it rebuilds from heartbeats before it orders (8.2), so an order's verdict has reached it
//! before the next order of the host is issued, and an earlier completed verdict of the
//! service is no longer one the coordinator waits for.
//!
//! **Orders.** One at a time, on a worker of the [`Dispatch`], so the loop keeps beating, and
//! reports the running target as `updating`, while a step runs. An order equal in every field to
//! the one running or the last one started is not started again; any other, even with the same
//! id and try, goes to the executor, which owns the try rules (a resend answered from the
//! record, `try_conflict`, `stale_try`). An order that arrives while another runs is not
//! started, and its verdict is `busy`. An order for a service that is not installed is not run.
//! A forced order runs only while the reply advertises `forced-v1`; an update whose target a
//! deferral declines runs only once the retry time has come, by the [`UnixClock`]. A worker that
//! ended with no verdict leaves no mark: the same order sent again runs.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use hostkeeper_config::names::{ServiceName, Version};

use crate::beat::{ANNOUNCED, Feature, Features, Heartbeat, Observed, Platform, ServiceFacts, fit};
use crate::deferral::{Book, Deferral};
use crate::names::Reason;
use crate::order::{Order, OrderRef, OrderResult, Outcome, Reply};
use crate::ports::{
    CertSource, CertState, CoordinatorLink, Dispatch, HeldHalts, LinkError, Log, OrderSink,
    RecordedService, ServiceObserver, ServiceRecords, Sleeper, Tick, UnixClock,
};

/// The period when the daemon is given none: 15 seconds.
pub const DEFAULT_HEARTBEAT_SECS: NonZeroU64 = NonZeroU64::MIN.saturating_add(14);

/// How many acknowledged verdicts the loop remembers per service. The step record keeps one
/// verdict, the last order's; a service holds at most a few verdicts not yet acknowledged
/// (design 8.1: the coordinator orders a host that has no outstanding order), so the record's
/// verdict is among these whatever order the acknowledgements came in.
pub const ACKED_KEPT: usize = 8;

/// The loop's flag on the daemon's command line.
#[derive(Clone, Debug, PartialEq, Eq, clap::Args)]
pub struct LoopFlags {
    /// Seconds between two heartbeats, at least 1.
    #[arg(long = "heartbeat-secs", default_value_t = DEFAULT_HEARTBEAT_SECS)]
    pub heartbeat_secs: NonZeroU64,
}

/// What the loop is told about the daemon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoopConfig {
    pub platform: Platform,
    /// The daemon's own version.
    pub agent_version: Version,
    /// The directory of the services' step-record directories, where the deferrals live.
    pub state_dir: PathBuf,
    /// Seconds between two heartbeats; never 0, so the loop always waits.
    pub period_secs: NonZeroU64,
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
    /// The step records could not be read, or named a service twice: nothing was sent.
    NoFacts,
    /// Not sent, or sent with no reply.
    NotSent(LinkError),
}

/// The order on the worker.
struct Running {
    order: Order,
    service: ServiceName,
    done: Receiver<OrderResult>,
}

/// The heartbeat loop of one daemon.
pub struct Heartbeater {
    config: LoopConfig,
    ports: Ports,
    book: Book,
    /// The features of the last reply.
    advertised: Features,
    /// The verdicts not yet acknowledged, by service, oldest first, one per order id and try.
    pending: BTreeMap<String, Vec<OrderResult>>,
    /// The verdicts acknowledged, by service, the last [`ACKED_KEPT`] of them: a step record
    /// that still holds one is not sent again.
    acked: BTreeMap<String, Vec<OrderRef>>,
    /// The last order started, by service, in every field.
    started: BTreeMap<String, Order>,
    /// The services of the last step records whose kept verdict was read.
    seen: BTreeSet<String>,
    /// The services of the last step records.
    installed: BTreeSet<String>,
    running: Option<Running>,
    /// The path the loop said it waits for, so that it says so once.
    waiting_for: Option<PathBuf>,
}

impl Heartbeater {
    pub fn new(config: LoopConfig, ports: Ports) -> Heartbeater {
        let book = Book::new(&config.state_dir);
        Heartbeater {
            config,
            ports,
            book,
            advertised: Features::default(),
            pending: BTreeMap::new(),
            acked: BTreeMap::new(),
            started: BTreeMap::new(),
            seen: BTreeSet::new(),
            installed: BTreeSet::new(),
            running: None,
            waiting_for: None,
        }
    }

    /// Beats now and after every period, until `sleeper` says stop. An order running on a
    /// worker when it stops goes on; its verdict is in its step record.
    pub fn run(mut self, sleeper: &dyn Sleeper) {
        loop {
            self.beat_once();
            if sleeper.sleep(self.config.period_secs.get()) == Tick::Stop {
                return;
            }
        }
    }

    /// One heartbeat, and what its reply asks for.
    pub fn beat_once(&mut self) -> BeatOutcome {
        self.book.flush(self.ports.log.as_ref());
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

    /// Takes the verdict of the running order, if it ended. A worker that ended with no verdict
    /// leaves the order unstarted, so the same order sent again runs.
    fn collect(&mut self) {
        let Some(running) = self.running.take() else {
            return;
        };
        match running.done.try_recv() {
            Ok(result) => self.finish(&running.service, result),
            Err(TryRecvError::Empty) => self.running = Some(running),
            Err(TryRecvError::Disconnected) => {
                self.started.remove(running.service.as_str());
                self.log(&format!(
                    "error: order {} try {} of {} ended with no verdict",
                    running.order.id.as_str(),
                    running.order.order_try,
                    running.service
                ));
            }
        }
    }

    /// The verdict of an order that ran: sent until acknowledged. A deferral is the service's
    /// deferral from now on; any other hooked verdict ends a deferral of the same target only,
    /// and a forced one ends none.
    fn finish(&mut self, service: &ServiceName, result: OrderResult) {
        let now = self.ports.clock.unix_secs();
        let log = self.ports.log.as_ref();
        match result.outcome {
            Outcome::Deferred { retry_after_secs } => {
                let deferral = deferral_of(&result, now, retry_after_secs);
                self.book.set(service, deferral, log);
            }
            // A forced update runs no hook, so its verdict says nothing about the moment a
            // deferral was about.
            Outcome::Succeeded | Outcome::Failed if !result.forced => {
                self.book.end(service, &result.target, now, log);
            }
            Outcome::Succeeded | Outcome::Failed => {}
        }
        if !self.installed.contains(service.as_str()) {
            self.log(&format!(
                "the verdict of order {} try {} is kept until {service} is listed again",
                result.order.id.as_str(),
                result.order.order_try
            ));
        }
        self.push(service, result);
    }

    /// Adds `result` to the verdicts to send for `service`, in place of one of the same order
    /// id and try.
    fn push(&mut self, service: &ServiceName, result: OrderResult) {
        let queue = self.pending.entry(service.as_str().to_owned()).or_default();
        queue.retain(|kept| kept.order != result.order);
        queue.push(result);
    }

    /// The verdict a step record kept, for a service listed again: sent unless it was
    /// acknowledged. A deferred verdict with no deferral of its target on record starts one
    /// from now, so a daemon that died between the step and the write of its deferral does not
    /// run the target early; one on record, ended or not, is kept as it is.
    fn recover(&mut self, service: &ServiceName, result: OrderResult, now: u64) {
        let counted = self.acked.get(service.as_str());
        if counted.is_some_and(|refs| refs.contains(&result.order)) {
            return;
        }
        if let Outcome::Deferred { retry_after_secs } = result.outcome {
            let log = self.ports.log.as_ref();
            let on_record = self.book.get(service, now, log);
            // A deferral of the same target on record, ended or not, is this verdict's: its
            // retry time stands, and one that has passed is not started again.
            if !on_record.is_some_and(|deferral| deferral.target == result.target) {
                self.book
                    .set(service, deferral_of(&result, now, retry_after_secs), log);
            }
        }
        self.push(service, result);
    }

    /// The heartbeat: every installed service, from its step record and the observer.
    fn facts(&mut self) -> io::Result<Heartbeat> {
        let installed = self.ports.records.installed()?;
        let mut names = BTreeSet::new();
        for recorded in &installed {
            if !names.insert(recorded.name.as_str().to_owned()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("the step records name {} twice", recorded.name),
                ));
            }
        }
        self.seen.retain(|name| names.contains(name));
        self.installed = names;
        let now = self.ports.clock.unix_secs();
        let mut services = Vec::with_capacity(installed.len());
        for recorded in installed {
            let RecordedService {
                name,
                version,
                updating,
                last_result,
            } = recorded;
            if self.seen.insert(name.as_str().to_owned())
                && let Some(result) = last_result
            {
                self.recover(&name, result, now);
            }
            let observation = self.ports.observer.observe(&name);
            let running = self
                .running
                .as_ref()
                .filter(|running| running.service == name)
                .map(|running| running.order.target().version.clone());
            let log = self.ports.log.as_ref();
            let deferred = self
                .advertised
                .has(Feature::DeferredV1)
                .then(|| self.book.get(&name, now, log))
                .flatten()
                .and_then(|deferral| deferral.report(now));
            services.push(ServiceFacts {
                version,
                health: observation.health,
                updating: running.or(updating),
                last_update: self
                    .pending
                    .get(name.as_str())
                    .and_then(|queue| {
                        if self.advertised.has(Feature::VerdictAcksV1) {
                            queue.first()
                        } else {
                            queue.last()
                        }
                    })
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

    /// What the reply asks for.
    fn take(&mut self, reply: Reply) {
        let Reply {
            order,
            features,
            halt_generations,
            acknowledged,
        } = reply;
        self.advertised = Features::of(&features);
        let in_order = self.advertised.has(Feature::VerdictAcksV1);
        for (service, queue) in &mut self.pending {
            let acks = |result: &OrderResult| {
                acknowledged
                    .iter()
                    .any(|ack| ack.service.as_str() == service && ack.order == result.order)
            };
            // Oldest first: what is older than an acknowledged verdict is ended with it.
            let ended = if in_order {
                queue.iter().rposition(acks).map_or(0, |at| at + 1)
            } else {
                0
            };
            let mut at = 0;
            queue.retain(|result| {
                at += 1;
                let counted = acks(result);
                if counted {
                    let refs = self.acked.entry(service.clone()).or_default();
                    refs.push(result.order.clone());
                    if refs.len() > ACKED_KEPT {
                        refs.remove(0);
                    }
                }
                !counted && at > ended
            });
        }
        self.pending.retain(|_, queue| !queue.is_empty());
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
        let service = order.target().service.clone();
        let named = format!(
            "order {} try {} of {service}",
            order.id.as_str(),
            order.order_try
        );
        if let Some(running) = &self.running {
            if running.order != order {
                self.log(&format!(
                    "{named} is not started: busy with order {} try {} of {}",
                    running.order.id.as_str(),
                    running.order.order_try,
                    running.service
                ));
                self.push(&service, OrderResult::busy(&order));
            }
            return;
        }
        if self.started.get(service.as_str()) == Some(&order) {
            return;
        }
        if !self.installed.contains(service.as_str()) {
            self.log(&format!(
                "{named} is not run: {service} is not installed on this host"
            ));
            return;
        }
        if order.is_forced() & !self.advertised.has(Feature::ForcedV1) {
            self.log(&format!(
                "{named} is not run: it is forced and the coordinator does not advertise forced-v1"
            ));
            return;
        }
        let now = self.ports.clock.unix_secs();
        let log = self.ports.log.as_ref();
        let declined = (!order.is_forced() & self.advertised.has(Feature::DeferredV1))
            .then(|| self.book.get(&service, now, log))
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
        self.started
            .insert(service.as_str().to_owned(), order.clone());
        self.running = Some(Running {
            order,
            service,
            done: receiver,
        });
    }
}

/// The deferral a deferred verdict starts at `now`, under the verdict's reason, or `deferred`
/// when it gave none.
fn deferral_of(result: &OrderResult, now: u64, retry_after_secs: u64) -> Deferral {
    let reason = result.reason.clone().unwrap_or_else(Reason::deferred);
    Deferral::starting(result.target.clone(), reason, now, retry_after_secs)
}
