//! The run state of a host's maintenance report (the `run` of `MaintenanceReport`,
//! maintenance.proto): what the host's maintenance run record says
//! ([`hostkeeper_steps::maintenance`]), in the wire's words (docs/design/maintenance-updates.md,
//! sections 4.1 and 5.7).
//!
//! The step is the one the record's next action stands at, so a record read after a crash reads
//! as the step it resumes: `waiting` before the stage is launched, `rebooting` while a kind's
//! reboot is under way, `deferred` and `halted` with the record's reason. The kinds applied are
//! given in the order they were applied. The reason is cut to the wire's bound
//! ([`MAX_HALTED_REASON`]) at a character's boundary; the ids are given as the record holds them.

use hostkeeper_proto::hostkeeper::agent::v1 as wire;
use hostkeeper_proto::maintenance::MAX_HALTED_REASON;
use hostkeeper_steps::maintenance::{Action, Ending, Kind, Launch, MaintenanceRecord, next_action};

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
fn wire_kind(kind: Kind) -> wire::MaintenanceKind {
    match kind {
        Kind::Macos => wire::MaintenanceKind::Macos,
        Kind::Xcode => wire::MaintenanceKind::Xcode,
        Kind::Linux => wire::MaintenanceKind::Linux,
        Kind::Reboot => wire::MaintenanceKind::Reboot,
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
