//! The sequencer of a host's maintenance turn (docs/design/maintenance-updates.md, sections 5.2 to
//! 5.4 and 5.7): it drives the run record ([`hostkeeper_steps::maintenance`]) from step to step,
//! launching each step through the host's launcher ([`launch`]), and stops where the record says
//! the turn ends or waits for a reboot.
//!
//! **The sequence.** 1 stage (the maintenance hook's `prepare`); 2 drain (each planned service's
//! `drain`, in the record's order); 3 apply (the hook's `cutover`, once per kind, macOS first, with
//! the parameters of that one kind); 4 check (the hook's `verify`); 5 put back (each drained
//! service's `resume`, in reverse); 6 done. Which step comes next, and what a step's end means, are
//! the record's rules ([`transition`]): a stage that fails defers the turn with nothing drained; a
//! drain whose deadline passes puts every drained service back and defers; any other failure from
//! drain on halts, and nothing more runs, not a `resume` and never a `rollback`.
//!
//! **Durability.** The record is written, fsynced, before each step is launched and again when it
//! ends, so a crash, a reboot or a restart as a newer release resumes the turn from it
//! ([`resume_at_start`]): a step launched and not ended is launched again (each step may run
//! twice). The run's parameters (the `maintain` order's fields) are kept beside the record, in
//! [`PARAMS_FILE`], written before the record of a new turn, so the record never names a turn whose
//! parameters are missing.
//!
//! **The reboot.** The hook's `cutover` of a kind that needs a reboot writes the kind's reboot
//! under way into the record, with the boot id, before it asks for the reboot
//! ([`crate::maintenance_hook::StoreRecord`]). The sequencer reads that from the record when the
//! step ends ([`Event::RebootUnderWay`]) and waits: it launches nothing more in this boot. At the
//! next start it reads the boot id; a new one applies the kind and the turn goes on with the next
//! kind, never the one that is applied; the same one keeps waiting. A `reboot` kind that comes
//! right after a kind whose reboot the host came back from is applied by that reboot, in the
//! same write of the record, without a second reboot.
//!
//! **One turn at a time.** One sequencer runs on a host at a time ([`MaintenanceRuns::hold`]); the
//! order's verb ([`crate::verbs::maintain`]) refuses a second turn while one is unfinished.
//!
//! **A halted turn.** Nothing more runs until an operator's `RETRY` or `PUT_BACK` of its order
//! ([`crate::verbs::maintain`]) lifts the halt in the record; the sequencer then drives the turn
//! from there, as from any other record.

pub mod launch;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_hook::Verdict;
use hostkeeper_link::UnixClock;
use hostkeeper_steps::CrashGate;
use hostkeeper_steps::maintenance::{
    Action, Ending, Event, Kind, Launch, MaintenanceLoaded, MaintenanceRecord,
    MaintenanceRecordError, MaintenanceStore, Outcome, next_action, transition,
};

use crate::executor::{Ctx, Reply};
use crate::maintenance_hook::MaintainParams;
use crate::refusal::Refusal;

/// The file, beside the run record, that holds the run's parameters: the `maintain` order's
/// fields as proto3 JSON, which the maintenance hook reads.
pub const PARAMS_FILE: &str = "maintain-params.json";

/// The alert of a turn the sequencer could not carry on: its record could not be written or
/// read. The turn stays where its record is, and the next start or resend of its order resumes
/// it.
pub const ALERT_MAINTENANCE_STOPPED: &str = "maintenance_stopped";

/// What the daemon keeps of maintenance turns while it runs: which turn a sequencer runs now,
/// the clock a drain's deadline is told in, and the crash seam of the tests.
pub struct MaintenanceRuns {
    /// The order id of the turn a sequencer runs now.
    running: Mutex<Option<String>>,
    /// The wall clock a drain's deadline is given to its hook in (unix seconds).
    unix: Arc<dyn UnixClock>,
    /// Stops the record's writer where it says, as a crash would: the seam of the tests.
    crash: Option<CrashGate>,
}

/// The hold of the one sequencer of a host: let go when dropped.
pub struct RunHold<'a> {
    runs: &'a MaintenanceRuns,
}

impl Drop for RunHold<'_> {
    fn drop(&mut self) {
        *self.runs.lock() = None;
    }
}

impl MaintenanceRuns {
    /// No turn running; drain deadlines told in `unix`'s seconds.
    pub fn new(unix: Arc<dyn UnixClock>) -> MaintenanceRuns {
        MaintenanceRuns {
            running: Mutex::new(None),
            unix,
            crash: None,
        }
    }

    /// Every record store it makes from now on stopped where `gate` says.
    pub fn set_crash_gate(&mut self, gate: CrashGate) {
        self.crash = Some(gate);
    }

    /// Drain deadlines told in `unix`'s seconds from now on.
    pub fn set_unix_clock(&mut self, unix: Arc<dyn UnixClock>) {
        self.unix = unix;
    }

    /// The wall clock of a drain's deadline.
    pub fn unix(&self) -> &dyn UnixClock {
        &*self.unix
    }

    /// The run record of the daemon whose state directory is `state_dir`, in its directory
    /// ([`MaintenanceStore::beside`]), with the crash seam when a test set one. One store is
    /// made per run of the sequencer, so the seam counts that run's writes.
    pub fn store(&self, state_dir: &Path) -> MaintenanceStore {
        let store = MaintenanceStore::beside(state_dir);
        match &self.crash {
            Some(gate) => store.with_crash_gate(Arc::clone(gate)),
            None => store,
        }
    }

    /// Takes the host's one sequencer for the turn of the order `order_id`; `Err` with the order
    /// id of the turn that holds it.
    pub fn hold(&self, order_id: &str) -> Result<RunHold<'_>, String> {
        let mut running = self.lock();
        match running.as_ref() {
            Some(holder) => Err(holder.clone()),
            None => {
                *running = Some(order_id.to_owned());
                Ok(RunHold { runs: self })
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<String>> {
        self.running.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The path of the run's parameters in the record's directory `dir`.
pub fn params_path(dir: &Path) -> PathBuf {
    dir.join(PARAMS_FILE)
}

/// Where a run of the sequencer stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Driven {
    /// The turn is over.
    Ended(Ending),
    /// A kind's reboot is under way and the host still runs the boot it was asked in: nothing
    /// more runs in this boot.
    Waiting(Kind),
    /// The record could not be written or read, or an event did not fit it: the turn stays
    /// where the record is.
    Stopped(String),
}

impl Driven {
    /// The daemon's answer to the order of a turn that stopped here.
    pub fn reply(self, resent: bool) -> Reply {
        match self {
            Driven::Ended(Ending::Done) => Reply::Finished {
                verdict: Verdict::Succeeded {
                    promotion_pending: false,
                },
                reason: None,
                resent,
            },
            Driven::Ended(Ending::Deferred(reason)) => Reply::Finished {
                verdict: Verdict::Deferred {
                    retry_after_secs: None,
                },
                reason: Some(reason),
                resent,
            },
            Driven::Ended(Ending::Halted(reason)) => Reply::Finished {
                verdict: Verdict::Halt,
                reason: Some(reason),
                resent,
            },
            Driven::Waiting(_) => Reply::Running,
            Driven::Stopped(why) => Reply::Interrupted {
                refusal: Refusal::StoreFailed,
                detail: why,
            },
        }
    }
}

/// Runs the turn of `record` from where it stands to its end or a wait for a reboot, writing the
/// record to `store` before each launch and after each end. The caller holds the host's
/// sequencer ([`MaintenanceRuns::hold`]).
pub fn drive(ctx: &Ctx, store: &MaintenanceStore, record: MaintenanceRecord) -> Driven {
    let params = read_params(store.dir());
    let mut record = record;
    loop {
        let next = match next_action(&record) {
            Action::End(ending) => return Driven::Ended(ending),
            Action::Wait(kind) => match ctx.clock.boot_id() {
                Ok(boot) => match transition(&record, Event::BootIdRead(boot)) {
                    Ok((_, Action::Wait(_))) => return Driven::Waiting(kind),
                    Ok((applied, _)) => {
                        ctx.log(&format!(
                            "maintenance run {}: the host came back from the reboot of {kind}",
                            record.run_id
                        ));
                        match rebooted_already(applied) {
                            Ok(applied) => applied,
                            Err(error) => return stopped(ctx, &record, &error),
                        }
                    }
                    Err(error) => return stopped(ctx, &record, &error.to_string()),
                },
                // Unknown: whether the reboot happened cannot be told, so the turn waits.
                Err(error) => {
                    ctx.log(&format!(
                        "maintenance run {}: waits for the reboot of {kind}: {error}",
                        record.run_id
                    ));
                    return Driven::Waiting(kind);
                }
            },
            Action::Launch(step) => match launched(ctx, store, &record, &step, &params) {
                Ok(ended) => ended,
                Err(why) => return stopped(ctx, &record, &why),
            },
        };
        if let Err(error) = store.save(&next) {
            return stopped(ctx, &next, &error.to_string());
        }
        record = next;
    }
}

/// Launches `step` of `record`: the record of its start written first, then the step, then the
/// record after its end (unsaved: the caller writes it).
fn launched(
    ctx: &Ctx,
    store: &MaintenanceStore,
    record: &MaintenanceRecord,
    step: &Launch,
    params: &Result<MaintainParams, String>,
) -> Result<MaintenanceRecord, String> {
    let started = after(record, [Event::Started])?;
    store.save(&started).map_err(text)?;
    ctx.log(&format!("maintenance run {}: {step}", started.run_id));
    let outcome = launch::run(ctx, store.dir(), &started, step, params);
    // The hook's `cutover` of a kind that reboots wrote the reboot under way in the record.
    let reboot = match step {
        Launch::Apply(kind) => reboot_written(store, *kind),
        _ => None,
    };
    let events = reboot
        .map(Event::RebootUnderWay)
        .into_iter()
        .chain([Event::Ended(outcome)]);
    after(&started, events)
}

/// `record` after each of `events` in turn; the first that does not fit it is refused.
fn after(
    record: &MaintenanceRecord,
    events: impl IntoIterator<Item = Event>,
) -> Result<MaintenanceRecord, String> {
    events
        .into_iter()
        .try_fold(record.clone(), |record, event| {
            transition(&record, event).map(|(next, _)| next)
        })
        .map_err(text)
}

/// `record` just applied a kind by the reboot the host came back from: when the next kind is
/// `reboot`, it is applied too, by that same reboot, without a launch and in the same write of the
/// record, so that no restart between the two can ask for a second reboot.
fn rebooted_already(record: MaintenanceRecord) -> Result<MaintenanceRecord, String> {
    if next_action(&record) != Action::Launch(Launch::Apply(Kind::Reboot)) {
        return Ok(record);
    }
    after(&record, [Event::Started, Event::Ended(Outcome::Ok)])
}

/// The boot id the hook's `cutover` wrote as `kind`'s reboot under way, read from the record on
/// disk; `None` when it wrote none, or the record does not read.
fn reboot_written(store: &MaintenanceStore, kind: Kind) -> Option<hostkeeper_steps::BootId> {
    match store.load() {
        Ok(MaintenanceLoaded::Current(record)) => record
            .kinds
            .into_iter()
            .find(|progress| progress.kind == kind)
            .and_then(|progress| progress.reboot_under_way),
        _ => None,
    }
}

/// The run's parameters, kept beside the record in `dir`.
pub fn read_params(dir: &Path) -> Result<MaintainParams, String> {
    let path = params_path(dir);
    let mut file = std::fs::File::open(&path)
        .map_err(|error| format!("the run's parameters {}: {error}", path.display()))?;
    MaintainParams::read(&mut file).map_err(|error| format!("the run's parameters: {error}"))
}

/// The turn could not go on: an alert, and the record stays where it is.
fn stopped(ctx: &Ctx, record: &MaintenanceRecord, why: &str) -> Driven {
    let why = format!("maintenance run {}: {why}", record.run_id);
    ctx.alert(ALERT_MAINTENANCE_STOPPED, &why);
    Driven::Stopped(why)
}

fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

/// What the record of the host holds, for a start or an order: a turn to resume, or none.
pub fn load(store: &MaintenanceStore) -> Result<Option<MaintenanceRecord>, MaintenanceRecordError> {
    match store.load()? {
        MaintenanceLoaded::Absent => Ok(None),
        MaintenanceLoaded::Current(record) => Ok(Some(*record)),
        MaintenanceLoaded::Newer { schema } => Err(MaintenanceRecordError::Newer { schema }),
    }
}

/// Whether the turn of `record` has not ended: it launches a step or waits for a reboot. A
/// halted turn has ended for the sequencer, and holds the host until an operator acts.
pub fn unfinished(record: &MaintenanceRecord) -> bool {
    !matches!(next_action(record), Action::End(_))
}

/// At the daemon's start: the turn the record holds, resumed from where it stands when it has
/// not ended, on this thread. `None` when there is no turn to resume (no record, an ended turn,
/// a record that does not read, which is logged, or a sequencer already running). The record is
/// read again once the host's sequencer is held, so that a turn an order ran in between is not
/// undone.
///
/// A daemon that starts on the very boot a kind's reboot was asked in (it stopped after the hook
/// recorded the reboot and before the reboot came, or the reboot was not made) launches that
/// kind's `cutover` once more, which asks for the reboot again and installs nothing
/// ([`crate::maintenance_hook`]); the turn then waits as before.
pub fn resume_at_start(ctx: &Ctx) -> Option<Driven> {
    let store = ctx.maintenance.store(&ctx.dirs.state);
    let first = resumable(ctx, &store)?;
    let _hold = ctx.maintenance.hold(&first.order_id).ok()?;
    let record = resumable(ctx, &store)?;
    ctx.log(&format!(
        "maintenance run {}: resumed at {}",
        record.run_id,
        next_action(&record)
    ));
    ask_reboot_again(ctx, &store, &record);
    Some(drive(ctx, &store, record))
}

/// The unfinished turn of the record; `None` for none, an ended one, or a record that does not
/// read (logged).
fn resumable(ctx: &Ctx, store: &MaintenanceStore) -> Option<MaintenanceRecord> {
    match load(store) {
        Ok(Some(record)) if unfinished(&record) => Some(record),
        Ok(_) => None,
        Err(error) => {
            ctx.log(&format!("maintenance run record not resumed: {error}"));
            None
        }
    }
}

/// When `record` waits for a reboot asked in the boot the host still runs: the kind's `cutover`
/// launched once more, which asks for the reboot again. The record is not changed; what the
/// launch came to is logged.
fn ask_reboot_again(ctx: &Ctx, store: &MaintenanceStore, record: &MaintenanceRecord) {
    let Action::Wait(kind) = next_action(record) else {
        return;
    };
    let now = ctx.clock.boot_id().ok();
    let same_boot = now.is_some()
        & record
            .kinds
            .iter()
            .any(|progress| (progress.kind == kind) & (progress.reboot_under_way == now));
    if !same_boot {
        return;
    }
    let params = read_params(store.dir());
    let outcome = launch::run(ctx, store.dir(), record, &Launch::Apply(kind), &params);
    ctx.log(&format!(
        "maintenance run {}: the host still runs the boot the reboot of {kind} was asked in; \
         its cutover asked again: {outcome:?}",
        record.run_id
    ));
}

impl crate::executor::Daemon {
    /// The maintenance turn the run record holds, resumed from where it stands to its end or a
    /// wait for a reboot ([`resume_at_start`]); `None` when there is none to resume.
    pub fn resume_maintenance(&self) -> Option<Driven> {
        resume_at_start(self.ctx())
    }
}
