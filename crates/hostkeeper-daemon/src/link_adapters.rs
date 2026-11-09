//! The daemon behind six ports of the heartbeat loop (`hostkeeper_link`): the executor, the
//! held halts, the installed services, the service observer, the enrollment loop's word on the
//! certificate and the report of the Sigstore trusted root.
//!
//! [`DaemonOrders`] is the loop's [`OrderSink`]: it writes the loop's order (an update, a forced
//! update, an install, an uninstall, an abort or a clear-record) as the protobuf `Order` that
//! [`Daemon::order`] takes ([`wire_order`]), and reads the daemon's [`Reply`] as the verdict the loop reports
//! ([`order_result`]). It decides nothing about the order: the checks, the locks and the try
//! rules are the daemon's.
//!
//! [`DaemonHalts`] is the loop's [`HeldHalts`] over the very store the executor reads
//! ([`crate::executor::Ctx::halts`]): a generation the loop raises lifts the service's halt as
//! `clear_halt` does, and is the one the next order of the service is judged against.
//!
//! [`DaemonRecords`] is the loop's [`ServiceRecords`]: every service whose `current` link names a
//! version, at that version, with the target of its step record while the record is unfinished
//! ([`recorded`]). [`DaemonObserver`] is its [`ServiceObserver`]: the observation
//! [`Daemon::status`] reports ([`Daemon::observer`]), in the loop's words ([`observation`]).
//!
//! [`DaemonRenewal`] is its [`RenewalSource`]: the [`RenewalState`] the enrollment loop writes
//! after each step ([`crate::links::Links::renewal`]), as it is when the heartbeat is made.
//!
//! [`DaemonTrustedRoot`] is its [`TrustedRootSource`]: the report of the trusted root the package
//! store checks attestations with ([`crate::packages::Packages::trusted_root`], the one
//! [`Daemon::status`] reports), read when the heartbeat is made; none from a store that keeps no root.

use std::io;
use std::sync::Arc;

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_enroll::RenewalState;
use hostkeeper_hook::{Class, Phase, Verdict};
use hostkeeper_link::{
    Health, HeldHalts, Liveness, Observation, Order, OrderId, OrderRef, OrderResult, OrderSink,
    Outcome, Reason, RecordedService, Renewal, RenewalSource, RootReason, ServiceObserver,
    ServiceRecords, Target, TrustedRootFacts, TrustedRootSource, Verb,
};
use hostkeeper_observe::{ServiceState, VersionView, beat_health};
use hostkeeper_proto::hostkeeper::agent::v1 as wire;
use hostkeeper_steps::record::{Loaded, Record, State};
use hostkeeper_steps::runner::DEFAULT_RETRY_SECS;

use crate::executor::{Ctx, Daemon, Reply};
use crate::installed::{self, CURRENT};
use crate::observer;
use crate::refusal::Refusal;
use crate::trusted_root::{RootProblem, RootReport};
use crate::verbs::force::SELF_SERVICE;

/// The most bytes of a verdict's `detail` (`UpdateResult.detail` of `hostkeeper.agent.v1`).
pub const MAX_DETAIL: usize = 512;

/// The executor port over the daemon.
pub struct DaemonOrders {
    daemon: Arc<Daemon>,
}

impl DaemonOrders {
    pub fn new(daemon: Arc<Daemon>) -> DaemonOrders {
        DaemonOrders { daemon }
    }
}

impl OrderSink for DaemonOrders {
    fn execute(&self, order: &Order, target: &Version) -> OrderResult {
        order_result(order, target, &self.daemon.order(&wire_order(order)))
    }
}

/// `order` as the protobuf `Order` the daemon takes, every field as it is: the id, the try, and
/// the target's service, version, digest and entry, or the service alone. An update is an `update`, a
/// forced update a `force`, an install an `install`, an uninstall an `uninstall`, an abort an
/// `abort` and a clear-record a `clear_record`. The loop's order has no halt generation, so a
/// `force` carries 0 and lifts no halt. The entry of a release the order carries goes back on the
/// verb's `target` unchanged; an order with none leaves it unset.
pub fn wire_order(order: &Order) -> wire::Order {
    let terms = |target: &Target| {
        (
            target.service.as_str().to_owned(),
            target.version.as_str().to_owned(),
            target.digest.as_str().to_owned(),
            target
                .index_digest
                .as_ref()
                .map_or_else(String::new, |d| d.as_str().to_owned()),
            target.entry.as_ref().map(|entry| entry.as_target().clone()),
        )
    };
    let verb = match &order.verb {
        Verb::Update(target) => {
            let (service, version, package_digest, index_digest, entry) = terms(target);
            wire::order::Verb::Update(wire::Update {
                service,
                version,
                package_digest,
                index_digest,
                target: entry,
            })
        }
        Verb::Force(target) => {
            let (service, version, package_digest, index_digest, entry) = terms(target);
            wire::order::Verb::Force(wire::Force {
                service,
                version,
                package_digest,
                index_digest,
                halt_generation: 0,
                target: entry,
            })
        }
        Verb::Install(target) => {
            let (service, version, package_digest, index_digest, entry) = terms(target);
            wire::order::Verb::Install(wire::Install {
                service,
                version,
                package_digest,
                index_digest,
                target: entry,
            })
        }
        Verb::Uninstall(service) => wire::order::Verb::Uninstall(wire::Uninstall {
            service: service.as_str().to_owned(),
        }),
        Verb::Abort(service) => wire::order::Verb::Abort(wire::Abort {
            service: service.as_str().to_owned(),
        }),
        Verb::ClearRecord(service) => wire::order::Verb::ClearRecord(wire::ClearRecord {
            service: service.as_str().to_owned(),
        }),
    };
    wire::Order {
        order_id: order.id.as_str().to_owned(),
        r#try: order.order_try,
        verb: Some(verb),
    }
}

/// The verdict the loop reports for `order`, naming `target`, from the daemon's `reply`.
///
/// - A finished order is its verdict, with its reason when that is a token: a halt is `failed`
///   in the phase `halt`; a deferral keeps its retry time, or the runner's default when the hook
///   gave none. A reason that is not a token is left out and said in the detail.
/// - A refused or interrupted order is `failed` with the refusal's token as its reason and the
///   daemon's text as its detail, in no phase: a refusal is not a new halt, and the halt that
///   caused `halted` was reported when it was raised. The refusals of an order's entry that
///   docs/design/packages.md names with a class ([`blamed`]) carry it, before the commit point.
/// - An order that is already running is not started: it is answered `busy`, as the loop
///   answers an order that arrives while another one runs.
///
/// The order's id and try, and whether it is forced, are the order's own; the version is the one
/// the loop gave. The detail is cut to [`MAX_DETAIL`] bytes.
pub fn order_result(order: &Order, target: &Version, reply: &Reply) -> OrderResult {
    let base = OrderResult {
        order: order.reference(),
        target: target.clone(),
        outcome: Outcome::Failed,
        phase: None,
        class: None,
        reason: None,
        forced: order.is_forced(),
        detail: None,
    };
    match reply {
        Reply::Running => OrderResult::busy(order, target.clone()),
        Reply::Refused { refusal, detail } | Reply::Interrupted { refusal, detail } => {
            let (phase, class) = blamed(*refusal).map_or((None, None), |class| {
                (Some(Phase::BeforeCommit), Some(class))
            });
            OrderResult {
                reason: Reason::new(refusal.token()).ok(),
                detail: Some(bounded(detail)),
                phase,
                class,
                ..base
            }
        }
        Reply::Finished {
            verdict, reason, ..
        } => finished(base, verdict, reason.as_deref()),
    }
}

/// The class a refusal of an order's entry is blamed on (docs/design/packages.md sections 4.4,
/// 7.1 and 7.4): `release` for an entry the daemon cannot accept (`order_invalid`) or whose
/// manifest it refuses (`manifest_invalid`), `environment` for an entry none of whose stores the
/// host defines (`no_store`). Every other refusal names no class, as before.
pub fn blamed(refusal: Refusal) -> Option<Class> {
    match refusal {
        Refusal::OrderInvalid | Refusal::ManifestInvalid => Some(Class::Release),
        Refusal::NoStore => Some(Class::Environment),
        _ => None,
    }
}

/// `base` as the order ended with `verdict` and `reason`.
fn finished(base: OrderResult, verdict: &Verdict, reason: Option<&str>) -> OrderResult {
    let (outcome, phase, class) = match verdict {
        Verdict::Succeeded { .. } => (Outcome::Succeeded, None, None),
        Verdict::Failed { phase, class } => (Outcome::Failed, Some(*phase), Some(*class)),
        Verdict::Deferred { retry_after_secs } => (
            Outcome::Deferred {
                retry_after_secs: retry_after_secs.unwrap_or(DEFAULT_RETRY_SECS),
            },
            None,
            None,
        ),
        Verdict::Halt => (Outcome::Failed, Some(Phase::Halt), None),
    };
    let (reason, detail) = match reason.map(Reason::new) {
        None => (None, None),
        Some(Ok(reason)) => (Some(reason), None),
        Some(Err(refused)) => (None, Some(bounded(&refused.to_string()))),
    };
    OrderResult {
        outcome,
        phase,
        class,
        reason,
        detail,
        ..base
    }
}

/// `text` cut to at most [`MAX_DETAIL`] bytes, at a character boundary.
fn bounded(text: &str) -> String {
    text.chars()
        .scan(0, |used, c| {
            *used += c.len_utf8();
            (*used <= MAX_DETAIL).then_some(c)
        })
        .collect()
}

/// The held-halt port over the daemon's own store.
pub struct DaemonHalts {
    daemon: Arc<Daemon>,
}

impl DaemonHalts {
    pub fn new(daemon: Arc<Daemon>) -> DaemonHalts {
        DaemonHalts { daemon }
    }
}

impl HeldHalts for DaemonHalts {
    /// The generation the store holds. A store that cannot be read holds every generation: the
    /// loop raises a generation only above the one held, so a halt is never lifted on a file
    /// that cannot be read, and the executor refuses the service's orders (`halt_unreadable`)
    /// until it can.
    fn held(&self, service: &ServiceName) -> u64 {
        self.daemon.ctx().halts.get(service).unwrap_or(u64::MAX)
    }

    /// Lifts the halt for a greater generation as `clear_halt` does ([`Daemon::clear_halt`]):
    /// the service's `Halt` record cleared first, then the store's generation raised. A
    /// generation not above the one held changes nothing and is not an error; a service whose
    /// lock is held, and a store or record that cannot be read or written, are, and the loop
    /// tries again at its next heartbeat.
    fn raise(&self, service: &ServiceName, generation: u64) -> io::Result<()> {
        match self.daemon.clear_halt(service, generation) {
            Ok(_) => Ok(()),
            Err(refused) if refused.refusal == Refusal::StaleHaltGeneration => Ok(()),
            Err(refused) => Err(io::Error::other(refused.detail)),
        }
    }
}

/// The installed-services port over the daemon's packages directory and step records.
pub struct DaemonRecords {
    daemon: Arc<Daemon>,
}

impl DaemonRecords {
    pub fn new(daemon: Arc<Daemon>) -> DaemonRecords {
        DaemonRecords { daemon }
    }
}

impl ServiceRecords for DaemonRecords {
    /// Every service of the packages directory that [`recorded`] lists, by name. The packages
    /// directory that cannot be listed is the error: the loop then sends no heartbeat.
    fn installed(&self) -> io::Result<Vec<RecordedService>> {
        let ctx = self.daemon.ctx();
        let services = installed::service_dirs(&ctx.dirs.packages)?;
        Ok(services
            .into_values()
            .filter_map(|service| recorded(ctx, service))
            .collect())
    }

    /// The coordinator acknowledged the verdict of `order` of `service`: when it is the settled
    /// update of hostkeeper itself that its record keeps to report ([`settled_result`]), the
    /// record stops keeping it, so no later start sends it again. Any other is left alone. A
    /// record that cannot be read or written is the error, which the loop logs; the verdict is
    /// then sent again at the next start, and acknowledged again.
    fn acknowledged(&self, service: &ServiceName, order: &OrderRef) -> io::Result<()> {
        if service.as_str() != SELF_SERVICE {
            return Ok(());
        }
        let store = self.daemon.ctx().store(service);
        store
            .verdict_acknowledged(order.id.as_str(), order.order_try)
            .map(|_| ())
            .map_err(|e| io::Error::other(e.to_string()))
    }
}

/// `service` as the heartbeat lists it: at the version its `current` link names, `None` when
/// it has none (it is not installed, or not yet); `updating` the target of its step record while
/// the record is unfinished, and nothing when it is finished, absent, newer or unreadable (the
/// executor refuses the service's orders while it cannot read the record); and the try the
/// record holds, finished or not, none when it is absent, newer or unreadable or names no try.
/// The record keeps no order id of a hooked order, so no verdict is given back from it: after a
/// restart the coordinator's resend of the order is answered from the record (design 8.3), and
/// the try tells a coordinator that never heard the verdict which try to number past. The
/// installer is the one `placed.json` records for it, none for a `hostkeeper-archive` service.
///
/// The one verdict given back is that of a settled update of hostkeeper itself, as the service
/// `hostkeeper`'s: its record keeps the order's id until the coordinator acknowledges the
/// verdict (`Progress::report_order_id`, [`settled_result`]). The coordinator ends that order
/// once the host runs its target and never resends it, and the binary that ran it exited
/// before its verdict was sent, so the loop sends it, from each start's first heartbeat until it
/// is acknowledged ([`DaemonRecords::acknowledged`]), in place of whatever older verdict the
/// coordinator still holds for the host.
pub fn recorded(ctx: &Ctx, service: ServiceName) -> Option<RecordedService> {
    let version = installed::linked_version(&ctx.dirs.packages, &service, CURRENT)?;
    let (updating, record_try, last_result) = match ctx.store(&service).load() {
        Ok(Loaded::Current(record)) => {
            let updating = match record.state {
                State::Finished { .. } => None,
                _ => Version::new(&record.target_version).ok(),
            };
            let settled = (service.as_str() == SELF_SERVICE)
                .then(|| settled_result(&record))
                .flatten();
            (updating, record.order_try, settled)
        }
        _ => (None, None, None),
    };
    let installer = recorded_installer(ctx, &service);
    Some(RecordedService {
        name: service,
        version,
        updating,
        last_result,
        record_try,
        installer,
    })
}

/// The installer `placed.json` records for `service`, as the heartbeat names it
/// (`ServiceBeat.installer`): none for a `hostkeeper-archive` service, which has no entry, and
/// none when the record cannot be read.
fn recorded_installer(ctx: &Ctx, service: &ServiceName) -> Option<String> {
    let record = ctx.placer().store().load().ok()?;
    record.installer_of(service.as_str()).map(str::to_owned)
}

/// The verdict a finished record of an update of hostkeeper itself keeps to report: under the
/// order id the settlement kept (`Progress::report_order_id`) and the record's try, at its
/// target, as an order that ended with the record's verdict ([`finished`]); `None` for a record
/// that keeps none, is unfinished, names no try, or whose id or target the loop cannot send.
pub fn settled_result(record: &Record) -> Option<OrderResult> {
    let State::Finished { verdict, reason } = &record.state else {
        return None;
    };
    let base = OrderResult {
        order: OrderRef {
            id: OrderId::new(record.progress.report_order_id.as_deref()?).ok()?,
            order_try: record.order_try?,
        },
        target: Version::new(&record.target_version).ok()?,
        outcome: Outcome::Failed,
        phase: None,
        class: None,
        reason: None,
        forced: false,
        detail: None,
    };
    Some(finished(base, verdict, reason.as_deref()))
}

/// The service-observer port over the daemon's observer.
pub struct DaemonObserver {
    daemon: Arc<Daemon>,
}

impl DaemonObserver {
    pub fn new(daemon: Arc<Daemon>) -> DaemonObserver {
        DaemonObserver { daemon }
    }
}

/// The detail of a service the daemon does not observe: it has no `current`, or the manifest of
/// the version `current` names does not load.
pub const NOT_OBSERVED: &str = "not_observed";

impl ServiceObserver for DaemonObserver {
    /// One observation of `service` ([`observer::observe`]); a service the daemon does not
    /// observe is `unobserved`, never absent or stopped.
    fn observe(&self, service: &ServiceName) -> Observation {
        let ctx = self.daemon.ctx();
        observer::observe(
            self.daemon.observer(),
            &ctx.dirs.packages,
            &crate::selfupdate::binary_dir(&ctx.dirs.state),
            &ctx.own_version,
            service,
        )
        .map_or_else(
            || Observation {
                liveness: Liveness::Unobserved,
                version: None,
                health: Health::Unhealthy,
                health_detail: Some(NOT_OBSERVED.to_owned()),
            },
            |seen| observation(&seen),
        )
    }
}

/// `seen` in the loop's words: the state as the liveness (live is running); the version only when
/// the running executable confirmed it; the health as the heartbeat's health
/// ([`beat_health`]), a health the observer cannot tell being unhealthy; and the token
/// that says why the service is not plainly live, healthy and confirmed as the detail.
pub fn observation(seen: &hostkeeper_observe::Observation) -> Observation {
    let liveness = match &seen.state {
        ServiceState::Absent => Liveness::Absent,
        ServiceState::Stopped => Liveness::Stopped,
        ServiceState::Transitioning => Liveness::Transitioning,
        ServiceState::Unobserved { .. } => Liveness::Unobserved,
        ServiceState::Unmonitored => Liveness::Unmonitored,
        ServiceState::Live => Liveness::Running,
    };
    let version = match &seen.version {
        VersionView::Recorded {
            version,
            confirmed: true,
        } => Version::new(version).ok(),
        _ => None,
    };
    let health = match beat_health(seen).0 {
        wire::Health::Healthy => Health::Healthy,
        wire::Health::Absent => Health::Absent,
        wire::Health::Unhealthy | wire::Health::Unspecified => Health::Unhealthy,
    };
    Observation {
        liveness,
        version,
        health,
        health_detail: seen.reason_token().map(str::to_owned),
    }
}

/// The heartbeat loop's [`RenewalSource`] over the enrollment loop's [`RenewalState`].
pub struct DaemonRenewal {
    state: RenewalState,
}

impl DaemonRenewal {
    pub fn new(state: RenewalState) -> DaemonRenewal {
        DaemonRenewal { state }
    }
}

impl RenewalSource for DaemonRenewal {
    fn renewal(&self) -> Renewal {
        let noted = self.state.get();
        Renewal {
            cert_not_after_unix: noted.not_after,
            last_renew_error: noted.last_error,
        }
    }
}

/// The heartbeat loop's [`TrustedRootSource`] over the daemon's package store.
pub struct DaemonTrustedRoot {
    daemon: Arc<Daemon>,
}

impl DaemonTrustedRoot {
    pub fn new(daemon: Arc<Daemon>) -> DaemonTrustedRoot {
        DaemonTrustedRoot { daemon }
    }
}

impl TrustedRootSource for DaemonTrustedRoot {
    fn trusted_root(&self) -> Option<TrustedRootFacts> {
        self.daemon
            .ctx()
            .packages
            .trusted_root()
            .as_ref()
            .map(root_facts)
    }
}

/// The report of the trusted root in the loop's words: its age, and its problem as the reason.
pub fn root_facts(report: &RootReport) -> TrustedRootFacts {
    TrustedRootFacts {
        age_secs: report.age_secs,
        reason: report.problem.map(|problem| match problem {
            RootProblem::Stale => RootReason::Stale,
            RootProblem::Missing => RootReason::Missing,
            RootProblem::Unparseable => RootReason::Unparseable,
        }),
    }
}
