//! The run state of a host's maintenance report (the `run` of `MaintenanceReport`,
//! maintenance.proto): what the host's maintenance run record says
//! ([`hostkeeper_steps::maintenance`]), in the wire's words (docs/design/maintenance-updates.md,
//! sections 4.1 and 5.7).
//!
//! Beside the run state, the report carries the host's last probe, the probe file the `probe`
//! kind keeps beside the run record ([`crate::maintenance_hook::probe`]), when it reads and keeps
//! the report's bounds, and the kinds the daemon runs only when an operator names them (`probe`
//! on a Mac, [`on_demand_of`]), so that the coordinator orders a probe only to a daemon that has
//! one.
//!
//! The step is the one the record's next action stands at, so a record read after a crash reads
//! as the step it resumes: `waiting` before the stage is launched, `rebooting` while a kind's
//! reboot is under way, `deferred` and `halted` with the record's reason. The kinds applied are
//! given in the order they were applied. The reason is cut to the wire's bound
//! ([`MAX_HALTED_REASON`]) at a character's boundary; the ids are given as the record holds them.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use hostkeeper_launchd::PlatformKind;
use hostkeeper_link::MaintenanceSource;
use hostkeeper_proto::hostkeeper::agent::v1 as wire;
use hostkeeper_proto::maintenance::{MAX_HALTED_REASON, check_report};
use hostkeeper_steps::maintenance::{
    Action, Ending, Kind, Launch, MaintenanceLoaded, MaintenanceRecord, MaintenanceStore,
    next_action,
};

use crate::maintenance_facts::MaintenanceFacts;
use crate::maintenance_hook::probe::{PROBE_FILE, read_probe};

/// The kinds a daemon on `platform` runs only when an operator names them: `probe` on a Mac,
/// none on Linux.
pub fn on_demand_of(platform: PlatformKind) -> Vec<wire::MaintenanceKind> {
    match platform {
        PlatformKind::Macos => vec![wire::MaintenanceKind::Probe],
        PlatformKind::Linux | PlatformKind::Openrc => Vec::new(),
    }
}

/// The probe the probe file in `dir` holds, when it reads and keeps the report's bounds; none
/// otherwise, so that a damaged file never has the whole report refused.
pub fn last_probe(dir: &Path) -> Option<wire::MaintenanceProbe> {
    let probe = read_probe(&dir.join(PROBE_FILE)).ok()?;
    let alone = wire::MaintenanceReport {
        probe: Some(probe),
        ..wire::MaintenanceReport::default()
    };
    check_report(&alone).ok()?;
    alone.probe
}

/// The report's run state of `record`.
pub fn run_state(record: &MaintenanceRecord) -> wire::MaintenanceRunState {
    let (step, reason) = match next_action(record) {
        Action::Launch(Launch::Stage) => match record.running {
            true => (wire::MaintenanceStep::Staging, None),
            false => (wire::MaintenanceStep::Waiting, None),
        },
        Action::Launch(Launch::Drain(_)) => (wire::MaintenanceStep::Draining, None),
        Action::Launch(Launch::Apply(_)) => (wire::MaintenanceStep::Applying, None),
        Action::Wait(_) => (wire::MaintenanceStep::Rebooting, None),
        Action::Launch(Launch::Check) => (wire::MaintenanceStep::Checking, None),
        Action::Launch(Launch::Resume(_)) => (wire::MaintenanceStep::PuttingBack, None),
        Action::End(Ending::Done) => (wire::MaintenanceStep::Done, None),
        Action::End(Ending::Deferred(reason)) => (wire::MaintenanceStep::Deferred, Some(reason)),
        Action::End(Ending::Halted(reason)) => (wire::MaintenanceStep::Halted, Some(reason)),
    };
    wire::MaintenanceRunState {
        run_id: record.run_id.clone(),
        order_id: record.order_id.clone(),
        step: step.into(),
        applied: record
            .kinds
            .iter()
            .filter(|progress| progress.applied)
            .map(|progress| i32::from(wire_kind(progress.kind)))
            .collect(),
        halted_reason: reason.map(bounded).unwrap_or_default(),
    }
}

/// The wire's name of `kind`.
pub fn wire_kind(kind: Kind) -> wire::MaintenanceKind {
    match kind {
        Kind::Macos => wire::MaintenanceKind::Macos,
        Kind::Xcode => wire::MaintenanceKind::Xcode,
        Kind::Linux => wire::MaintenanceKind::Linux,
        Kind::Reboot => wire::MaintenanceKind::Reboot,
        Kind::Probe => wire::MaintenanceKind::Probe,
    }
}

/// `text` cut to at most [`MAX_HALTED_REASON`] bytes, at the last character boundary that fits.
fn bounded(mut text: String) -> String {
    let end = (0..=MAX_HALTED_REASON)
        .rev()
        .find(|at| text.is_char_boundary(*at))
        .unwrap_or(0);
    text.truncate(end);
    text
}

/// The maintenance report of the heartbeat with the run state of the host's turn: the facts of
/// the last look (`facts`, any [`MaintenanceSource`]), and the run state of the run record in the
/// directory beside `state_dir` ([`MaintenanceStore::beside`]), read at every report, so the
/// report says the step the record says at every beat, across a restart or a reboot. A record
/// that is absent or does not read leaves the run state out.
///
/// When the record's turn is seen ended (done or deferred) for the first time, the facts are
/// asked to look again before the report ([`MaintenanceFacts::look_again`]): what the host has
/// installed may have changed, so what is due is looked for at once.
pub struct RunReported<F> {
    facts: F,
    state_dir: PathBuf,
    /// The kinds this daemon runs only when an operator names them ([`on_demand_of`]).
    on_demand: Vec<wire::MaintenanceKind>,
    /// The order id and step of the last ended turn the facts were asked to look again for.
    seen: Mutex<Option<(String, i32)>>,
}

/// What a report's facts must do besides reporting: look again once a turn has ended.
pub trait LookAgain {
    fn look_again(&self);
}

impl LookAgain for MaintenanceFacts {
    fn look_again(&self) {
        MaintenanceFacts::look_again(self);
    }
}

impl<F> RunReported<F> {
    /// `facts`' report with the run state of the record beside `state_dir`, the last probe kept
    /// beside it, and `on_demand` as the kinds run only when named.
    pub fn new(
        facts: F,
        state_dir: &Path,
        on_demand: Vec<wire::MaintenanceKind>,
    ) -> RunReported<F> {
        RunReported {
            facts,
            state_dir: state_dir.to_path_buf(),
            on_demand,
            seen: Mutex::new(None),
        }
    }
}

impl<F: MaintenanceSource + LookAgain> MaintenanceSource for RunReported<F> {
    fn report(&self) -> wire::MaintenanceReport {
        let store = MaintenanceStore::beside(&self.state_dir);
        let run = match store.load() {
            Ok(MaintenanceLoaded::Current(record)) => Some(run_state(&record)),
            _ => None,
        };
        let ended = run.as_ref().filter(|run| {
            [wire::MaintenanceStep::Done, wire::MaintenanceStep::Deferred]
                .map(i32::from)
                .contains(&run.step)
        });
        if let Some(run) = ended {
            let key = (run.order_id.clone(), run.step);
            let mut seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
            if seen.as_ref() != Some(&key) {
                *seen = Some(key);
                self.facts.look_again();
            }
        }
        wire::MaintenanceReport {
            run,
            probe: last_probe(store.dir()),
            on_demand: self.on_demand.iter().copied().map(i32::from).collect(),
            ..self.facts.report()
        }
    }
}
