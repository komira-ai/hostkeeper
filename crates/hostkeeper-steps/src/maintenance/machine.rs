//! The pure rules of a host's turn (docs/design/maintenance-updates.md, sections 5.2 to 5.4 and
//! 5.7): given the record and what happened, the next record and what to do next. No I/O, no
//! clock.
//!
//! - **Stage** fails (or defers): nothing was touched and the host never left service, so the turn
//!   ends deferred.
//! - **Drain**'s deadline passes (the step defers, exit 75): every drained service is put back,
//!   the one whose `drain` deferred included, and the turn ends deferred.
//! - Any other failure **from drain on** (a drain, an apply, the check or a resume that fails, or
//!   defers) halts the turn where it is: no later step runs, so no `resume` and never a
//!   `rollback`, and the host stays out of service until an operator acts.
//! - **Apply** runs once per kind, in the record's order. A kind that reboots records the boot id
//!   before its reboot ([`Event::RebootUnderWay`]); while the boot id read after a start is that
//!   one, the turn waits ([`Action::Wait`]); once it is another, or the kind's new version is
//!   read, the kind is applied and the turn goes on with the next kind, or the check.
//! - **Put back** resumes the drained services in reverse order, then the turn ends: done, or
//!   deferred when it was a deferral that put them back.
//! - A **halted** turn moves only by an operator's order (section 4.2), whose try the record
//!   takes: **Retry** ([`Event::Retried`]) lifts the halt and launches the step that failed again
//!   (the same drain, the first kind not applied, the check, or the same resume), and the turn
//!   goes on from there; **Put back** ([`Event::PutBack`]) lifts the halt, keeps its reason in
//!   `put_back`, and resumes every drained service in reverse order, leaving each kind applied or
//!   not as it was, and the turn ends done (deferred when the halt came while a deferral put the
//!   services back). Neither ever runs a `rollback`.

use std::fmt;

use hostkeeper_config::names::ServiceName;

use super::record::{Kind, MaintenanceRecord, RunStep};
use crate::clock::BootId;

/// A step to launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Launch {
    /// The maintenance hook's `prepare`.
    Stage,
    /// The service's `drain`.
    Drain(ServiceName),
    /// The maintenance hook's `cutover <kind>`.
    Apply(Kind),
    /// The maintenance hook's `verify`.
    Check,
    /// The service's `resume`.
    Resume(ServiceName),
}

impl fmt::Display for Launch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Launch::Stage => write!(f, "stage"),
            Launch::Drain(service) => write!(f, "drain {service}"),
            Launch::Apply(kind) => write!(f, "apply {kind}"),
            Launch::Check => write!(f, "check"),
            Launch::Resume(service) => write!(f, "resume {service}"),
        }
    }
}

/// How a turn ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ending {
    /// Every step ran: the host is back in service with the update applied.
    Done,
    /// Deferred, and why: the host is back in service with nothing changed.
    Deferred(String),
    /// Halted, and why: the host stays out of service, and nothing more runs.
    Halted(String),
}

/// What the sequencer does next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Launch this step: write the record of [`Event::Started`] first.
    Launch(Launch),
    /// The kind's reboot is under way and the host has not come back on a new boot: launch
    /// nothing, and give the boot id read after the next start ([`Event::BootIdRead`]).
    Wait(Kind),
    /// The turn is over.
    End(Ending),
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Action::Launch(launch) => write!(f, "{launch}"),
            Action::Wait(kind) => write!(f, "wait for the reboot of {kind}"),
            Action::End(Ending::Done) => write!(f, "done"),
            Action::End(Ending::Deferred(_)) => write!(f, "deferred"),
            Action::End(Ending::Halted(_)) => write!(f, "halted"),
        }
    }
}

/// How a launched step ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// It succeeded (exit 0).
    Ok,
    /// It failed: a non-zero exit other than 75, or past its bound. The reason is the
    /// sequencer's words (the exit code, the hook's reason), never a secret.
    Failed(String),
    /// It deferred (exit 75): for a drain, its deadline passed.
    Deferred(String),
}

/// What happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The step of [`Action::Launch`] is about to be launched. Given again for a step that was
    /// launched and did not end (a crash), it changes nothing: the step is launched again.
    Started,
    /// The launched step ended.
    Ended(Outcome),
    /// The kind being applied is about to reboot the host, which now has this boot id.
    RebootUnderWay(BootId),
    /// The boot id read after a start, while a reboot is under way.
    BootIdRead(BootId),
    /// The version of the kind waited for was read: `reached` when it is the one the kind
    /// installs.
    VersionRead { reached: bool },
    /// An operator's Retry of the halted turn, by the order's try `order_try`, with the boot id
    /// read now (`None` when it cannot be read): the step that failed is launched again.
    Retried {
        order_try: u64,
        boot: Option<BootId>,
    },
    /// An operator's Put back of the halted turn, by the order's try `order_try`: the drained
    /// services are resumed and the turn ends.
    PutBack { order_try: u64 },
}

impl Event {
    fn name(&self) -> &'static str {
        match self {
            Event::Started => "started",
            Event::Ended(_) => "ended",
            Event::RebootUnderWay(_) => "reboot under way",
            Event::BootIdRead(_) => "boot id read",
            Event::VersionRead { .. } => "version read",
            Event::Retried { .. } => "retried",
            Event::PutBack { .. } => "put back",
        }
    }
}

/// An event that does not fit the record: nothing is changed.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("the event {event} does not fit a maintenance turn at {at}")]
pub struct TransitionError {
    /// The event's name.
    pub event: &'static str,
    /// The action the record stands at.
    pub at: String,
}

/// What to do next with the record as it stands: the step to launch (again, when it was
/// launched and did not end), a wait for a reboot, or the end.
pub fn next_action(record: &MaintenanceRecord) -> Action {
    settle(record.clone()).1
}

/// The record after `event`, and what to do next; an event that does not fit the record is
/// refused ([`TransitionError`]).
///
/// - [`Event::Started`] fits an [`Action::Launch`]: the record says the step runs (a drain counts
///   its service drained from here).
/// - [`Event::Ended`] fits a launched step that runs, and a [`Action::Wait`] whose `cutover` still
///   runs (it ended after it recorded the reboot): see the module's rules.
/// - [`Event::RebootUnderWay`] fits a running [`Launch::Apply`].
/// - [`Event::BootIdRead`] and [`Event::VersionRead`] fit an [`Action::Wait`]: the same boot id,
///   or a version not reached, keeps waiting; another boot id, or the version reached, applies
///   the kind.
/// - [`Event::Retried`] and [`Event::PutBack`] fit a halted turn alone: see the module's rules.
pub fn transition(
    record: &MaintenanceRecord,
    event: Event,
) -> Result<(MaintenanceRecord, Action), TransitionError> {
    let (mut rec, action) = settle(record.clone());
    match (event, action) {
        (Event::Started, Action::Launch(launch)) => {
            if let (false, Launch::Drain(service)) = (rec.running, &launch) {
                rec.drained.push(service.clone());
            }
            rec.running = true;
            Ok((rec, Action::Launch(launch)))
        }
        (Event::Ended(outcome), Action::Launch(launch)) if rec.running => {
            Ok(ended(rec, &launch, outcome))
        }
        (Event::Ended(outcome), Action::Wait(kind)) if rec.running => {
            Ok(ended(rec, &Launch::Apply(kind), outcome))
        }
        (Event::RebootUnderWay(boot), Action::Launch(Launch::Apply(kind))) if rec.running => {
            rec.kinds
                .iter_mut()
                .filter(|progress| progress.kind == kind)
                .for_each(|progress| progress.reboot_under_way = Some(boot.clone()));
            Ok((rec, Action::Wait(kind)))
        }
        (Event::BootIdRead(boot), Action::Wait(kind)) => {
            let same = rec
                .kinds
                .iter()
                .any(|p| (p.kind == kind) & (p.reboot_under_way.as_ref() == Some(&boot)));
            match same {
                true => Ok((rec, Action::Wait(kind))),
                false => Ok(applied(rec, kind)),
            }
        }
        (Event::VersionRead { reached }, Action::Wait(kind)) => match reached {
            true => Ok(applied(rec, kind)),
            false => Ok((rec, Action::Wait(kind))),
        },
        (Event::Retried { order_try, boot }, Action::End(Ending::Halted(_))) => {
            Ok(retried(rec, order_try, boot.as_ref()))
        }
        (Event::PutBack { order_try }, Action::End(Ending::Halted(reason))) => {
            Ok(put_back(rec, order_try, reason))
        }
        (event, action) => Err(TransitionError {
            event: event.name(),
            at: action.to_string(),
        }),
    }
}

/// The record after the launched step `launch` ended with `outcome`.
fn ended(
    mut rec: MaintenanceRecord,
    launch: &Launch,
    outcome: Outcome,
) -> (MaintenanceRecord, Action) {
    rec.running = false;
    let why = |detail: String| format!("{launch}: {detail}");
    match outcome {
        Outcome::Ok => match launch {
            Launch::Stage | Launch::Check => rec.step = rec.step.following(),
            Launch::Drain(_) => {}
            // A kind whose reboot is under way is applied only once the host is back on a new
            // boot (or the new version is read).
            Launch::Apply(kind) => rec
                .kinds
                .iter_mut()
                .filter(|progress| progress.kind == *kind)
                .for_each(|progress| progress.applied |= progress.reboot_under_way.is_none()),
            Launch::Resume(_) => {
                rec.drained.pop();
            }
        },
        Outcome::Failed(detail) if matches!(launch, Launch::Stage) => {
            rec.deferred = Some(why(detail));
            rec.step = RunStep::PutBack;
        }
        Outcome::Deferred(detail) if matches!(launch, Launch::Stage | Launch::Drain(_)) => {
            rec.deferred = Some(why(detail));
            rec.step = RunStep::PutBack;
        }
        Outcome::Failed(detail) | Outcome::Deferred(detail) => rec.halted = Some(why(detail)),
    }
    settle(rec)
}

/// The record with `kind` applied, its reboot over, and what comes next.
fn applied(mut rec: MaintenanceRecord, kind: Kind) -> (MaintenanceRecord, Action) {
    rec.kinds
        .iter_mut()
        .filter(|progress| progress.kind == kind)
        .for_each(|progress| {
            progress.applied = true;
            progress.reboot_under_way = None;
        });
    rec.running = false;
    settle(rec)
}

/// The halted record retried by the try `order_try`, the boot id read now being `boot`, and the
/// step that failed, which comes next.
///
/// The step stays the one that failed, so the next action is that step again: the first kind not
/// applied, the check, or the last drained service's `resume`. A failed drain counted its service
/// drained when it was launched, so it is marked running, as a drain cut short is, and the same
/// drain is launched again rather than the next one. A kind whose reboot was asked in the boot
/// the host still runs (its `cutover` failed after it asked, and the reboot never came) has that
/// reboot forgotten, so that its `cutover` is launched again; one asked in another boot, or when
/// the boot id cannot be read, is left to the wait, which applies the kind once the host is on a
/// new boot.
fn retried(
    mut rec: MaintenanceRecord,
    order_try: u64,
    boot: Option<&BootId>,
) -> (MaintenanceRecord, Action) {
    rec.halted = None;
    rec.order_try = order_try;
    rec.kinds
        .iter_mut()
        .filter(|progress| boot.is_some() & (progress.reboot_under_way.as_ref() == boot))
        .for_each(|progress| progress.reboot_under_way = None);
    rec.running = (rec.step == RunStep::Drain) & !rec.drained.is_empty();
    settle(rec)
}

/// The halted record put back by the try `order_try`, the halt's `reason` kept as its note: every
/// drained service resumed next, in reverse order, then the end. A kind whose reboot is under way
/// is not applied, and its reboot is no longer waited for.
fn put_back(
    mut rec: MaintenanceRecord,
    order_try: u64,
    reason: String,
) -> (MaintenanceRecord, Action) {
    rec.halted = None;
    rec.put_back = Some(reason);
    rec.order_try = order_try;
    rec.kinds
        .iter_mut()
        .for_each(|progress| progress.reboot_under_way = None);
    rec.step = RunStep::PutBack;
    rec.running = false;
    settle(rec)
}

/// The record moved past every step with nothing left to do (a drain with no service left, an
/// apply with every kind applied, a put back with nothing drained), and the action it stands at.
fn settle(mut rec: MaintenanceRecord) -> (MaintenanceRecord, Action) {
    loop {
        if let Some(reason) = &rec.halted {
            let action = Action::End(Ending::Halted(reason.clone()));
            return (rec, action);
        }
        let action = match rec.step {
            RunStep::Stage => Some(Action::Launch(Launch::Stage)),
            // The service launched last while it runs, the next one otherwise.
            RunStep::Drain => rec
                .services
                .get(rec.drained.len().saturating_sub(usize::from(rec.running)))
                .map(|service| Action::Launch(Launch::Drain(service.clone()))),
            RunStep::Apply => {
                rec.kinds
                    .iter()
                    .find(|p| !p.applied)
                    .map(|p| match p.reboot_under_way {
                        Some(_) => Action::Wait(p.kind),
                        None => Action::Launch(Launch::Apply(p.kind)),
                    })
            }
            RunStep::Check => Some(Action::Launch(Launch::Check)),
            RunStep::PutBack => rec
                .drained
                .last()
                .map(|service| Action::Launch(Launch::Resume(service.clone()))),
            RunStep::Done => Some(Action::End(match &rec.deferred {
                Some(reason) => Ending::Deferred(reason.clone()),
                None => Ending::Done,
            })),
        };
        match action {
            Some(action) => return (rec, action),
            None => {
                rec.step = rec.step.following();
                rec.running = false;
            }
        }
    }
}
