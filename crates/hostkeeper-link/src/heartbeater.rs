//! The loop: one heartbeat per period, one more as soon as a running order ends, and what the
//! reply asks for.
//!
//! **Waits.** After a heartbeat the loop waits one period. While an order runs on a worker it
//! waits the same period a second at a time, and once the order has ended it beats at once,
//! with the verdict (and, for an install, the service now listed), then waits a whole period
//! again: a coordinator learns of an order's end within a second of it, not at the end of the
//! period, and a host never beats more often than once per period plus one beat after each order that ends.
//!
//! Each heartbeat, in this order:
//!
//! 1. A deferral write that failed earlier is tried again. The verdict of the order running on
//!    the worker, if it ended, joins the service's verdicts to send; a deferral is written, and
//!    a hooked verdict on the deferred target ends the deferral.
//! 2. The certificate source is read. With no pair the loop says once what it waits for, and
//!    makes no call; the first call after a pair appears is at the next period.
//! 3. The facts are built from the step records and the observer: every installed service, its
//!    version, health and running update, the try its step record holds (whether or not its
//!    verdict is sent, so a coordinator that never heard the verdict numbers past it), its
//!    newest verdict not yet acknowledged, and its deferral when the last reply advertised
//!    `deferred-v1`. Step records that name a service
//!    twice send nothing. A service listed again after it was not takes the verdict its step
//!    record kept, unless that verdict was acknowledged, so a restart resends it; a kept
//!    deferred verdict with no deferral of its target on record starts one, and any other
//!    hooked one ends a deferral of its target, as an order that ran here does. A service the
//!    records do not list that has a verdict to send (an install that ended before it placed
//!    anything, as a refusal does, or a service that left the records while its order ran) is
//!    listed too, absent at the verdict's target, with its units absent and that verdict: the
//!    coordinator reads it as not installed, and hears how the order ended. Once the verdict is
//!    acknowledged the entry stays, with no verdict, which a coordinator that advertises
//!    `verdict-acks-v1` reads as still reporting it, so it does not order the same target again;
//!    it goes once the records list the service or an uninstall of it succeeds. The loop keeps
//!    this in memory only: after a restart of the daemon, a service the records do not list is
//!    not listed until its next order ends.
//! 4. [`fit`] keeps the bound; a heartbeat over it is refused and logged, never sent cut short.
//! 5. The heartbeat goes out through [`CoordinatorLink::heartbeat`], the one outbound call.
//! 6. From the reply: the advertised features replace the last ones; a verdict whose service,
//!    order id and try the reply acknowledges is not sent again, and the step records are told
//!    ([`ServiceRecords::acknowledged`]), so a record that keeps it to report stops; with
//!    `halt-v1`, a halt
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
//! a restart only the verdict the step records give back comes back (the last order's, or the
//! settled update of the daemon itself, which the binary that ran it never sent): the coordinator
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
//! started, and its verdict is `busy`. An order for a service that is not installed is not run,
//! but for an `install`, which is for a service the host does not run. A forced order runs only
//! while the reply advertises `forced-v1`; an update, an install or an uninstall whose version a
//! deferral names runs only once the retry time has come, by the [`UnixClock`] (for an
//! uninstall, the version it names is the one the service runs, so a deferred uninstall sent
//! again waits out its own deferral). A worker that ended with no verdict leaves no mark: the
//! same order sent again runs.
//!
//! The verdict of an order names its target version; an `uninstall` names none, and its verdict
//! names the version the service runs, as the last step records listed it. While an order runs,
//! its service's `updating` is that version: an uninstall reports the version it removes, and an
//! install reports nothing until its service is listed, or until it ends: its verdict is sent
//! whether or not the service is listed then (step 3). An uninstall that succeeded leaves no
//! verdict to send: the service is gone from the next heartbeat, which is the report, and the
//! verdicts of it not yet acknowledged go with it.
//!
//! **Abort.** An `abort` is not one of the orders above: it goes to the executor at once, on a
//! worker of its own, beside the order it stops, whatever runs. The order it stops reports how
//! it ended (in the halt phase); the abort's own answer is logged, never sent as a verdict. Each
//! abort that arrives is run, a resend too: the executor answers a resend of the same order and
//! try from its first answer and stops nothing more. An
//! abort of a service the loop runs no order of and does not list is not run: the abort that
//! stops a step the daemon left running before a restart is the admin socket's.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_wire::wire::clip_renew_error;

use crate::beat::{
    ANNOUNCED, Feature, Features, Health, Heartbeat, Liveness, Observed, Platform, Renewal,
    ServiceFacts, fit,
};
use crate::deferral::{Book, Deferral};
use crate::names::Reason;
use crate::order::{Order, OrderRef, OrderResult, Outcome, Reply, Verb};
use crate::ports::{
    CertSource, CertState, CoordinatorLink, Dispatch, HeldHalts, LinkError, Log, OrderSink,
    RecordedService, RenewalSource, ServiceObserver, ServiceRecords, Sleeper, Tick, UnixClock,
};

/// The period when the daemon is given none: 15 seconds.
pub const DEFAULT_HEARTBEAT_SECS: NonZeroU64 = NonZeroU64::MIN.saturating_add(14);

/// The slice, in seconds, the wait after a heartbeat is cut into while an order runs: how long
/// the verdict of an order that ended waits for its heartbeat, at most.
const ORDER_POLL_SECS: u64 = 1;

/// How many acknowledged verdicts the loop remembers per service. The step record keeps one
/// verdict, the last order's; a service holds at most a few verdicts not yet acknowledged
/// (design 8.1: the coordinator orders a host that has no outstanding order), so the record's
/// verdict is among these whatever order the acknowledgements came in.
pub const ACKED_KEPT: usize = 8;

/// The loop's flag on the daemon's command line.
#[derive(Clone, Debug, PartialEq, Eq, clap::Args)]
pub struct LoopFlags {
    /// Seconds between two heartbeats while no order runs, at least 1; the end of an order is
    /// reported within a second.
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
    /// Seconds between two heartbeats while no order runs; never 0, so the loop always waits.
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
    pub renewal: Arc<dyn RenewalSource>,
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
    /// The version its verdict names.
    target: Version,
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
    /// The services of the last step records, each at the version they listed.
    installed: BTreeMap<String, Version>,
    /// The services the step records do not list whose last order ended, each at that order's
    /// target: each is listed absent there, its verdict while one is to be sent, until the
    /// records list it or an uninstall of it succeeds.
    absent: BTreeMap<String, Version>,
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
            installed: BTreeMap::new(),
            absent: BTreeMap::new(),
            running: None,
            waiting_for: None,
        }
    }

    /// Beats now and after every wait ([`Heartbeater::wait`]), until `sleeper` says stop, giving
    /// `seen` what each heartbeat came to before the wait that follows it. An order running on a
    /// worker when it stops goes on; its verdict is in its step record.
    pub fn run(mut self, sleeper: &dyn Sleeper, seen: &mut dyn FnMut(&BeatOutcome)) {
        loop {
            seen(&self.beat_once());
            if self.wait(sleeper) == Tick::Stop {
                return;
            }
        }
    }

    /// The wait after a heartbeat: one period on `sleeper`. While an order runs, the period is
    /// waited [`ORDER_POLL_SECS`] at a time, and the wait ends after the slice in which the
    /// order ended, so its verdict, and the service an install lists, go out at once rather
    /// than at the end of the period. [`Tick::Stop`] from any slice ends the wait.
    fn wait(&mut self, sleeper: &dyn Sleeper) -> Tick {
        let period = self.config.period_secs.get();
        if self.running.is_none() {
            return sleeper.sleep(period);
        }
        let mut left = period;
        while left > 0 {
            let slice = left.min(ORDER_POLL_SECS);
            if sleeper.sleep(slice) == Tick::Stop {
                return Tick::Stop;
            }
            left -= slice;
            self.collect();
            if self.running.is_none() {
                break;
            }
        }
        Tick::Beat
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
            Ok(result) => self.finish(&running.order, &running.service, result),
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
    /// and a forced one ends none. An uninstall that succeeded is not sent: the service is gone,
    /// and so are its verdicts not yet acknowledged.
    fn finish(&mut self, order: &Order, service: &ServiceName, result: OrderResult) {
        if matches!(order.verb, Verb::Uninstall(_)) & (result.outcome == Outcome::Succeeded) {
            self.pending.remove(service.as_str());
            self.absent.remove(service.as_str());
            self.log(&format!(
                "order {} try {} uninstalled {service}",
                result.order.id.as_str(),
                result.order.order_try
            ));
            return;
        }
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
        if !self.installed.contains_key(service.as_str()) {
            self.log(&format!(
                "order {} try {} of {service}, which is not installed, ended {}: it is sent with \
                 {service} listed absent",
                result.order.id.as_str(),
                result.order.order_try,
                answered(&result)
            ));
        }
        self.push(service, result);
    }

    /// Adds `result` to the verdicts to send for `service`, in place of one of the same order
    /// id and try; a service the step records do not list is listed absent at its target from
    /// now on.
    fn push(&mut self, service: &ServiceName, result: OrderResult) {
        if !self.installed.contains_key(service.as_str()) {
            self.absent
                .insert(service.as_str().to_owned(), result.target.clone());
        }
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
        let log = self.ports.log.as_ref();
        match result.outcome {
            Outcome::Deferred { retry_after_secs } => {
                let on_record = self.book.get(service, now, log);
                // A deferral of the same target on record, ended or not, is this verdict's: its
                // retry time stands, and one that has passed is not started again.
                if !on_record.is_some_and(|deferral| deferral.target == result.target) {
                    self.book
                        .set(service, deferral_of(&result, now, retry_after_secs), log);
                }
            }
            // As when the order ran in this process ([`Self::finish`]): a hooked verdict ends a
            // deferral of its target, such as one the binary before a restart wrote for it.
            Outcome::Succeeded | Outcome::Failed if !result.forced => {
                self.book.end(service, &result.target, now, log);
            }
            Outcome::Succeeded | Outcome::Failed => {}
        }
        self.push(service, result);
    }

    /// The heartbeat: every installed service, from its step record and the observer.
    fn facts(&mut self) -> io::Result<Heartbeat> {
        let installed = self.ports.records.installed()?;
        let mut names = BTreeMap::new();
        for recorded in &installed {
            let named = names.insert(recorded.name.as_str().to_owned(), recorded.version.clone());
            if named.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("the step records name {} twice", recorded.name),
                ));
            }
        }
        self.seen.retain(|name| names.contains_key(name));
        self.installed = names;
        let listed = &self.installed;
        self.absent.retain(|name, _| !listed.contains_key(name));
        let now = self.ports.clock.unix_secs();
        let mut services = Vec::with_capacity(installed.len());
        for recorded in installed {
            let RecordedService {
                name,
                version,
                updating,
                last_result,
                record_try,
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
                .map(|running| running.target.clone());
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
                last_update: self.reported(&name),
                deferred,
                health_detail: observation.health_detail,
                observed: Some(Observed {
                    liveness: observation.liveness,
                    version: observation.version,
                }),
                record_try,
                name,
            });
        }
        // A service the step records do not list whose order ended (an install that ended
        // before it placed anything, or a service that left the records while its order ran)
        // is listed absent at the order's target, with its verdict while one is to be sent and
        // with none once the coordinator has acknowledged it, which then reads the entry as
        // still reporting that verdict (`verdict-acks-v1`): it does not order the same target
        // again as if it had never heard of the service.
        let unlisted: Vec<(ServiceName, Version)> = self
            .absent
            .iter()
            .filter_map(|(name, target)| Some((ServiceName::new(name).ok()?, target.clone())))
            .collect();
        for (name, target) in unlisted {
            let facts = self.absent_facts(name, target, now);
            services.push(facts);
        }
        let Renewal {
            cert_not_after_unix,
            last_renew_error,
        } = self.ports.renewal.renewal();
        Ok(Heartbeat {
            platform: self.config.platform,
            agent_version: self.config.agent_version.clone(),
            features: ANNOUNCED.to_vec(),
            services,
            sent_unix: now,
            renewal: Renewal {
                cert_not_after_unix,
                last_renew_error: last_renew_error
                    .map(|text| clip_renew_error(&text).to_owned())
                    .filter(|text| !text.is_empty()),
            },
        })
    }

    /// The verdict of `name` the heartbeat carries: of those not yet acknowledged, the oldest to
    /// a coordinator that advertised `verdict-acks-v1`, else the newest; shaped to the features
    /// advertised.
    fn reported(&self, name: &ServiceName) -> Option<OrderResult> {
        self.pending
            .get(name.as_str())
            .and_then(|queue| {
                if self.advertised.has(Feature::VerdictAcksV1) {
                    queue.first()
                } else {
                    queue.last()
                }
            })
            .map(|result| result.shaped(&self.advertised))
    }

    /// The entry of `name`, a service the step records do not list whose last order ended at
    /// `target`: at the target of the verdict it carries (`target` when it carries none),
    /// absent (installed and never run, as far as the coordinator is told: it orders such a
    /// service an install), its units absent, with its deferral when the coordinator advertised
    /// `deferred-v1`.
    fn absent_facts(&mut self, name: ServiceName, target: Version, now: u64) -> ServiceFacts {
        let last_update = self.reported(&name);
        let log = self.ports.log.as_ref();
        let deferred = self
            .advertised
            .has(Feature::DeferredV1)
            .then(|| self.book.get(&name, now, log))
            .flatten()
            .and_then(|deferral| deferral.report(now));
        ServiceFacts {
            version: last_update
                .as_ref()
                .map_or(target, |result| result.target.clone()),
            health: Health::Absent,
            updating: None,
            last_update,
            deferred,
            health_detail: None,
            observed: Some(Observed {
                liveness: Liveness::Absent,
                version: None,
            }),
            record_try: None,
            name,
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
        let in_order = self.advertised.has(Feature::VerdictAcksV1);
        let mut counted_now = Vec::new();
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
                    counted_now.push((service.clone(), result.order.clone()));
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
        for (service, order) in counted_now {
            let Ok(name) = ServiceName::new(&service) else {
                continue;
            };
            if let Err(e) = self.ports.records.acknowledged(&name, &order) {
                self.log(&format!(
                    "error: the acknowledgement of order {} try {} of {service} is not kept: {e}",
                    order.id.as_str(),
                    order.order_try
                ));
            }
        }
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
        let service = order.service().clone();
        let named = format!(
            "order {} try {} of {service}",
            order.id.as_str(),
            order.order_try
        );
        if matches!(order.verb, Verb::Abort(_)) {
            self.abort(order, &named);
            return;
        }
        let listed = self.installed.get(service.as_str());
        let target = match (order.target(), listed) {
            (Some(target), _) => target.version.clone(),
            (None, Some(version)) => version.clone(),
            (None, None) => {
                self.log(&format!(
                    "{named} is not run: {service} is not installed on this host"
                ));
                return;
            }
        };
        if let Some(running) = &self.running {
            if running.order != order {
                self.log(&format!(
                    "{named} is not started: busy with order {} try {} of {}",
                    running.order.id.as_str(),
                    running.order.order_try,
                    running.service
                ));
                self.push(&service, OrderResult::busy(&order, target));
            }
            return;
        }
        if self.started.get(service.as_str()) == Some(&order) {
            return;
        }
        if listed.is_none() & !matches!(order.verb, Verb::Install(_)) {
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
            .filter(|deferral| deferral.declines(&target, now));
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
        let job_target = target.clone();
        self.ports.dispatch.start(Box::new(move || {
            let _ = sender.send(orders.execute(&job_order, &job_target));
        }));
        self.started
            .insert(service.as_str().to_owned(), order.clone());
        self.running = Some(Running {
            order,
            service,
            target,
            done: receiver,
        });
    }

    /// Hands the abort `order` to the executor on a worker of its own, beside the order it stops,
    /// and logs its answer when it ends. Its answer names the target of the order of the service
    /// that runs, else the version listed; an abort of a service with neither is not run.
    fn abort(&self, order: Order, named: &str) {
        let service = order.service();
        let running = self
            .running
            .as_ref()
            .filter(|running| &running.service == service)
            .map(|running| running.target.clone());
        let Some(target) = running.or_else(|| self.installed.get(service.as_str()).cloned()) else {
            self.log(&format!(
                "{named} is not run: no order of {service} runs and it is not installed on this host"
            ));
            return;
        };
        let orders = Arc::clone(&self.ports.orders);
        let log = Arc::clone(&self.ports.log);
        let named = named.to_owned();
        self.ports.dispatch.start(Box::new(move || {
            let answer = orders.execute(&order, &target);
            log.line(&format!("{named} answered {}", answered(&answer)));
        }));
    }
}

/// An answer as the log says it: the outcome, then the reason and the detail when it has them.
fn answered(result: &OrderResult) -> String {
    let mut text = result.outcome.as_str().to_owned();
    if let Some(reason) = &result.reason {
        text.push_str(": ");
        text.push_str(reason.as_str());
    }
    if let Some(detail) = &result.detail {
        text.push_str(" (");
        text.push_str(detail);
        text.push(')');
    }
    text
}

/// The deferral a deferred verdict starts at `now`, under the verdict's reason, or `deferred`
/// when it gave none.
fn deferral_of(result: &OrderResult, now: u64, retry_after_secs: u64) -> Deferral {
    let reason = result.reason.clone().unwrap_or_else(Reason::deferred);
    Deferral::starting(result.target.clone(), reason, now, retry_after_secs)
}
