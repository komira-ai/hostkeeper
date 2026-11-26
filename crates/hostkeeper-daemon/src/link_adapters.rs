//! The daemon behind seven ports of the heartbeat loop (`hostkeeper_link`): the executor, the
//! held halts, the installed services, the service observer, the enrollment loop's word on the
//! certificate, the report of the Sigstore trusted root and the runner of `maintain` orders.
//!
//! [`DaemonOrders`] is the loop's [`OrderSink`]: it writes the loop's order (an update, a forced
//! update, an install, an uninstall, an abort or a clear-record) as the protobuf `Order` that
//! [`Daemon::order`] takes ([`wire_order`]), and reads the daemon's [`Reply`] as the verdict the loop reports
//! ([`order_result`]). It decides nothing about the order: the checks, the locks and the try
//! rules are the daemon's.
//!
//! [`DaemonMaintain`] is the loop's [`MaintainSink`]: a `maintain` order, as the protobuf `Order`
//! [`Daemon::order`] takes.
//!
//! [`DaemonHalts`] is the loop's [`HeldHalts`] over the very store the executor reads
//! ([`crate::executor::Ctx::halts`]): each generation of a reply goes to
//! [`crate::verbs::clear_halt::heard`], which holds a halt under it, lifts a halt as `clear_halt`
//! does, or removes the file of a service that holds no halt.
//!
//! [`DaemonRecords`] is the loop's [`ServiceRecords`]: every service whose `current` link names a
//! version, at that version, with the target of its step record while the record is unfinished
//! ([`recorded`]). [`DaemonObserver`] is its [`ServiceObserver`]: the observation
//! [`Daemon::status`] reports ([`Daemon::observer`]), in the loop's words ([`observation`]), with
//! the service's restarts counted over the last ten minutes of the daemon's monotonic clock
//! ([`RestartWindow`]).
//!
//! [`DaemonRenewal`] is its [`RenewalSource`]: the [`RenewalState`] the enrollment loop writes
//! after each step ([`crate::links::Links::renewal`]), as it is when the heartbeat is made.
//!
//! [`NoTrustedRoot`] is its [`TrustedRootSource`]: the daemon checks no attestation, so it
//! reports no trusted root; the heartbeat's `trusted_root_age_secs` and `trusted_root_reason`
//! stay in the protocol, unsent.

use std::io;
use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_enroll::RenewalState;
use hostkeeper_hook::{Class, Phase, Verdict};
use hostkeeper_link::{
    Drift, Health, HeldHalt, HeldHalts, Liveness, MaintainOrder, MaintainSink, Observation, Order,
    OrderId, OrderRef, OrderResult, OrderSink, Outcome, Reason, RecordedService, Renewal,
    RenewalSource, ServiceObserver, ServiceRecords, Target, TrustedRootFacts, TrustedRootSource,
    Verb,
};
use hostkeeper_observe::{RestartWindow, ServiceState, VersionView, beat_health};
use hostkeeper_proto::hostkeeper::agent::v1 as wire;
use hostkeeper_steps::Clock as _;
use hostkeeper_steps::record::{Loaded, Record, State};
use hostkeeper_steps::runner::DEFAULT_RETRY_SECS;

use crate::executor::{Ctx, Daemon, Reply};
use crate::installed::{self, CURRENT};
use crate::observer;
use crate::refusal::Refusal;
use crate::verbs::force::SELF_SERVICE;

/// The runner of `maintain` orders over the daemon: each order the loop gives it is run by
/// [`Daemon::order`] (`verbs::maintain`), on the loop's worker, to the turn's end or a wait for a
/// reboot. The answer goes to the daemon's log only, but for a resend's: how the turn goes is the run state of the
/// maintenance report ([`crate::maintenance_state::RunReported`]).
pub struct DaemonMaintain {
    daemon: Arc<Daemon>,
}

impl DaemonMaintain {
    pub fn new(daemon: Arc<Daemon>) -> DaemonMaintain {
        DaemonMaintain { daemon }
    }
}

impl MaintainSink for DaemonMaintain {
    fn maintain(&self, order: &MaintainOrder) {
        let maintain = wire::Order {
            order_id: order.id.as_str().to_owned(),
            r#try: order.order_try,
            verb: Some(wire::order::Verb::Maintain(order.maintain.clone())),
        };
        let reply = self.daemon.order(&maintain);
        // A resend is answered `Running` while the turn runs or waits, and from the record once
        // it ended: neither says anything new.
        if !matches!(reply, Reply::Running | Reply::Finished { resent: true, .. }) {
            self.daemon.ctx().log(&format!(
                "maintain order {} try {}: {reply:?}",
                order.id.as_str(),
                order.order_try
            ));
        }
    }
}

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
    /// The daemon's reply as its verdict ([`order_result`]), with the hook's own `detail` of a
    /// verdict the step record holds ([`hook_detail`]) when the reply gives none.
    fn execute(&self, order: &Order, target: &Version) -> OrderResult {
        let reply = self.daemon.order(&wire_order(order));
        let mut result = order_result(order, target, &reply);
        if matches!(reply, Reply::Finished { .. }) & result.detail.is_none() {
            result.detail = hook_detail(self.daemon.ctx(), order, target);
        }
        result
    }
}

/// The hook's `detail` of how `order` ended at `target` ([`Record::finished_detail`]), cut to
/// [`MAX_DETAIL`] bytes: from the service's step record when it is finished and is the record of
/// that try at that version. `None` when the record holds none, is of another order or version
/// (a verdict answered without a step record, such as a refused entry), or cannot be read.
fn hook_detail(ctx: &Ctx, order: &Order, target: &Version) -> Option<String> {
    let Ok(Loaded::Current(record)) = ctx.store(order.service()).load() else {
        return None;
    };
    let same =
        (record.order_try == Some(order.order_try)) & (record.target_version == target.as_str());
    same.then(|| record.finished_detail().map(bounded))
        .flatten()
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
    /// Judges the generation as [`Daemon::heard_halt`] does: a halt held under no generation is
    /// held under `before` (or lifted by a greater generation), one held under a smaller one is
    /// lifted (the service's `Halt` record cleared
    /// first, then its file removed), and a file of a service that holds no halt is removed. A
    /// service whose lock is held, and a record or file that cannot be read, change nothing and
    /// are not an error: the next reply judges them again, and the executor refuses the service's
    /// orders (`record_unreadable`, `halt_unreadable`) until they read. A file or record that
    /// cannot be written is the error, which the loop logs.
    fn heard(&self, service: &ServiceName, before: Option<u64>, generation: u64) -> io::Result<()> {
        self.daemon
            .heard_halt(service, before, generation)
            .map(drop)
            .map_err(|refused| io::Error::other(refused.detail))
    }

    /// The halt the service's step record ended in, under the generation the store holds, and
    /// when that generation was written. `None` while the record holds no halt, and while the
    /// record or the store cannot be read: the executor then refuses the service's orders under
    /// that name (`record_unreadable`, `halt_unreadable`), which the verdict reports.
    fn holding(&self, service: &ServiceName) -> Option<HeldHalt> {
        let ctx = self.daemon.ctx();
        let Ok(Loaded::Current(record)) = ctx.store(service).load() else {
            return None;
        };
        let State::Finished {
            verdict: Verdict::Halt,
            reason,
        } = record.state
        else {
            return None;
        };
        Some(HeldHalt {
            reason: reason
                .as_deref()
                .and_then(|reason| Reason::new(reason).ok()),
            generation: ctx.halts.get(service).ok()?,
            written_unix: ctx.halts.written_unix(service),
        })
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
/// (`ServiceBeat.installer`): none for a service with no entry, and none when the record cannot
/// be read.
fn recorded_installer(ctx: &Ctx, service: &ServiceName) -> Option<String> {
    let record = ctx.placer().store().load().ok()?;
    record.installer_of(service.as_str()).map(str::to_owned)
}

/// The verdict a finished record of an update of hostkeeper itself keeps to report: under the
/// order id the settlement kept (`Progress::report_order_id`) and the record's try, at its
/// target, as an order that ended with the record's verdict ([`finished`]), with the hook's
/// `detail` the record holds of it ([`Record::finished_detail`]); `None` for a record
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
    let result = finished(base, verdict, reason.as_deref());
    Some(OrderResult {
        detail: result.detail.or(record.finished_detail().map(bounded)),
        ..result
    })
}

/// The service-observer port over the daemon's observer, and the window its services' restarts
/// are counted in.
pub struct DaemonObserver {
    daemon: Arc<Daemon>,
    restarts: Mutex<RestartWindow>,
}

impl DaemonObserver {
    pub fn new(daemon: Arc<Daemon>) -> DaemonObserver {
        DaemonObserver {
            daemon,
            restarts: Mutex::new(RestartWindow::new()),
        }
    }
}

/// The detail of a service the daemon does not observe: it has no `current`, or the manifest of
/// the version `current` names does not load.
pub const NOT_OBSERVED: &str = "not_observed";

/// The detail of the daemon's own service whose unit the service manager does not load while
/// its package is installed (a unit an earlier move left hides it).
pub const OWN_UNIT_NOT_LOADED: &str = "own_unit_not_loaded";

/// `observed`, the observation of `service`, as the heartbeat reports it: the daemon's own
/// service observed absent (the service manager finds no unit) is running, since this daemon is
/// its process, and installed, since only a service with a `current` is observed. It is reported
/// unhealthy, [`OWN_UNIT_NOT_LOADED`], so the coordinator never reads it as not installed and
/// orders its install. Any other service or observation is reported as observed.
fn own_running(service: &ServiceName, observed: Observation) -> Observation {
    if (service.as_str() != SELF_SERVICE) | (observed.health != Health::Absent) {
        return observed;
    }
    Observation {
        liveness: Liveness::Running,
        health: Health::Unhealthy,
        health_detail: Some(OWN_UNIT_NOT_LOADED.to_owned()),
        ..observed
    }
}

impl ServiceObserver for DaemonObserver {
    /// One observation of `service` ([`observer::observe`]); a service the daemon does not
    /// observe is `unobserved`, never absent or stopped, and the daemon's own service is never
    /// absent ([`own_running`]). A repository package's observation also carries its drift
    /// ([`drift`]). Its restarts are its run units' counts at this observation, with the
    /// restarts of the last ten minutes the window counted ([`RestartWindow::record`], at the
    /// daemon's monotonic second); a service not observed, or whose units the service manager
    /// counts no restarts of, has none, and the window forgets it.
    fn observe(&self, service: &ServiceName) -> Observation {
        let ctx = self.daemon.ctx();
        let seen = observer::observe(
            self.daemon.observer(),
            &ctx.dirs.packages,
            &crate::selfupdate::binary_dir(&ctx.dirs.state),
            &ctx.own_version,
            service,
        );
        let units = seen.as_ref().map_or(&[][..], |seen| &seen.restarts[..]);
        let restarts = self
            .restarts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .record(service.as_str(), units, ctx.clock.mono_secs());
        let observed = seen.map_or_else(
            || Observation {
                liveness: Liveness::Unobserved,
                version: None,
                health: Health::Unhealthy,
                health_detail: Some(NOT_OBSERVED.to_owned()),
                drift: None,
                health_body: None,
                restarts: None,
            },
            |seen| own_running(service, observation(&seen)),
        );
        Observation {
            drift: drift(ctx, service),
            restarts,
            ..observed
        }
    }
}

/// The drift of `service` at this heartbeat (packages-repositories.md section 4.4): for a
/// version `current` names that a repository installer staged, the version the package manager
/// shows against the one `placed.json` records, when they differ. `None` for every other
/// service, while an install is pending, and when the installed version cannot be read, which is
/// logged.
pub fn drift(ctx: &Ctx, service: &ServiceName) -> Option<Drift> {
    let version = installed::linked_version(&ctx.dirs.packages, service, CURRENT)?;
    let dir = ctx
        .dirs
        .packages
        .join(service.as_str())
        .join(version.as_str());
    let entry = crate::target::read(&dir).ok().flatten()?;
    match ctx.repository().drift(service, &entry) {
        Ok(found) => found.map(|drift| Drift {
            found: drift.found,
            recorded: drift.recorded,
        }),
        Err(failure) => {
            ctx.log(&format!(
                "service={service}: the installed version could not be read for its drift: {failure}"
            ));
            None
        }
    }
}

/// `seen` in the loop's words: the state as the liveness (live is running); the version only when
/// the running executable confirmed it; the health as the heartbeat's health
/// ([`beat_health`]); the token that says why the service is not plainly live, healthy and
/// confirmed as the detail; and the first bytes of the body of the health URL's answer that was
/// not 2xx. A service with nothing to observe (no run units and no health URL: a
/// configuration package, which only places files) is healthy, as a live service with no health
/// URL is: nothing of it can fail to serve, and a coordinator that read it unhealthy would never
/// count its host as serving, or its bake as healthy.
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
    let health = match (beat_health(seen).0, &seen.state) {
        (wire::Health::Healthy, _) => Health::Healthy,
        (wire::Health::Absent, _) => Health::Absent,
        // `beat_health` leaves an unmonitored service with no health URL unspecified: there is
        // nothing to observe, so nothing unhealthy.
        (wire::Health::Unspecified, ServiceState::Unmonitored) => Health::Healthy,
        // `Silent` is the coordinator's word, which a local read never yields; like any other
        // health the daemon cannot tell, it counts as unhealthy.
        (wire::Health::Unhealthy | wire::Health::Unspecified | wire::Health::Silent, _) => {
            Health::Unhealthy
        }
    };
    Observation {
        liveness,
        version,
        health,
        health_detail: seen.reason_token().map(str::to_owned),
        // The repository installer's drift is added by the daemon's observer ([`drift`]).
        drift: None,
        health_body: seen.health_body.clone(),
        // The restarts are counted by the daemon's observer, across observations.
        restarts: None,
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

/// The heartbeat loop's [`TrustedRootSource`]: none, since the daemon keeps no trusted root.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoTrustedRoot;

impl TrustedRootSource for NoTrustedRoot {
    fn trusted_root(&self) -> Option<TrustedRootFacts> {
        None
    }
}

#[cfg(test)]
#[path = "link_adapters_tests.rs"]
mod tests;
