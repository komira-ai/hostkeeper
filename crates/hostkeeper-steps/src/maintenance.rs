//! One `drain` or `resume` of one service, as a maintenance run runs it
//! (docs/design/maintenance-updates.md, sections 5.2 to 5.4).
//!
//! [`run_maintenance_step`] runs the step the service's manifest names in `[hook] drain` or
//! `[hook] resume` through the launcher of the host, as an additive step
//! ([`StepLauncher::run_additive`]): `<executable> <hook_args...> <word> --params <params>`, in
//! the unit the caller names, bounded by the call's [`Bound`]. It waits for the step to end and
//! reads how it ended:
//!
//! | the step ended | outcome |
//! |---|---|
//! | exit `0` | [`MaintenanceOutcome::Succeeded`] |
//! | exit `75` | [`MaintenanceOutcome::Deferred`] |
//! | a `drain` bounded by a deadline, ended at or past it with no code or exit `1` (the launcher ended it: no code, or `systemd-run`'s `1` for a unit `RuntimeMaxSec=` ended) | [`MaintenanceOutcome::Deferred`] |
//! | any other code, or no code | [`MaintenanceOutcome::Failed`] |
//!
//! Work is never cut to finish an update: a `drain` is unbounded ([`Bound::Unbounded`], the
//! unit's `RuntimeMaxSec=infinity`) unless the run names a deadline, and then it is bounded by
//! exactly that deadline, which the hook is also given in its params file
//! (`hostkeeper_hook::DRAIN_DEADLINE_KEY`, written by the caller) so that it puts the service
//! back and exits `75` itself. A `resume` is bounded, by [`RESUME_BOUND_SECS`] unless the caller
//! gives another bound; a `resume` that runs past its bound is [`MaintenanceOutcome::Failed`].
//! Exit `76` (halt) is [`MaintenanceOutcome::Failed`] here too: the run halts on either, and
//! the code is kept in [`MaintenanceExit::exit`].
//!
//! Nothing here runs `rollback`, or any other step: the caller decides what follows. What is kept
//! of the hook's output is what the launcher keeps (the last [`crate::RING_BYTES`] of each
//! stream, the standard error handed to the launcher's log), and a last line longer than
//! [`LAST_LINE_MAX`] is not kept. A [`LaunchError`] is answered as it is: the unit may then still
//! run, and the caller stops it by name before it runs anything else for the service.

use std::num::NonZeroU64;
use std::path::Path;

use hostkeeper_hook::{EXIT_DEFERRED, EXIT_FAILURE, EXIT_OK, Step};

use crate::capture::LAST_LINE_MAX;
use crate::clock::Clock;
use crate::launcher::{AdditiveCall, Bound, LaunchError, StepExit, StepLauncher};

/// The bound of a `resume` when the caller gives none: ten minutes. A `resume` puts the service
/// back in service (js-agent's re-reads its facts and lifts its cordon) and waits for nothing
/// that is the host's work, so a `resume` still running then is a failed one.
pub const RESUME_BOUND_SECS: NonZeroU64 = NonZeroU64::MIN.saturating_add(599);

/// Which of the two maintenance steps of a service's hook runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaintenanceStep {
    /// Takes the service out of service; returns once it has no work.
    Drain,
    /// Puts the service back in service.
    Resume,
}

impl MaintenanceStep {
    /// The step of the hook contract.
    pub const fn step(self) -> Step {
        match self {
            MaintenanceStep::Drain => Step::Drain,
            MaintenanceStep::Resume => Step::Resume,
        }
    }

    /// The bound of the step when the run names none: no bound for a `drain`, so that no work
    /// is cut, and [`RESUME_BOUND_SECS`] for a `resume`.
    pub const fn default_bound(self) -> Bound {
        match self {
            MaintenanceStep::Drain => Bound::Unbounded,
            MaintenanceStep::Resume => Bound::Secs(RESUME_BOUND_SECS),
        }
    }
}

/// One run of a service's `drain` or `resume`.
#[derive(Clone, Debug)]
pub struct MaintenanceCall<'a> {
    /// The service the step belongs to.
    pub service: &'a str,
    /// Which step runs.
    pub step: MaintenanceStep,
    /// The word the step is run by: what the manifest names in `[hook] drain` or
    /// `[hook] resume`.
    pub word: &'a str,
    /// The arguments of the manifest's hook command after its program, given before the word.
    pub hook_args: &'a [String],
    /// The params file of the run; for a `drain` with a deadline it holds
    /// `hostkeeper_hook::DRAIN_DEADLINE_KEY`.
    pub params: &'a Path,
    /// The hook executable, a file of the installed release the caller checked
    /// (`Staged::check`) just before.
    pub executable: &'a Path,
    /// The name of the step's unit.
    pub unit: &'a str,
    /// The bound of the run: [`MaintenanceStep::default_bound`] unless the run names another (a
    /// `drain`'s deadline: `Bound::Secs` of exactly its seconds).
    pub bound: Bound,
}

/// How a `drain` or a `resume` ended, as the maintenance run reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaintenanceOutcome {
    /// Exit `0`.
    Succeeded,
    /// Exit `75`, or a `drain` whose deadline passed: nothing irreversible happened.
    Deferred,
    /// Any other end.
    Failed,
}

/// How a `drain` or a `resume` ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaintenanceExit {
    /// The outcome the run reads.
    pub outcome: MaintenanceOutcome,
    /// The exit the launcher reported, its last line dropped when longer than [`LAST_LINE_MAX`].
    pub exit: StepExit,
    /// Whether the step was a `drain` bounded by a deadline that ended with no code or exit `1`
    /// (the launcher's end of a step at its bound) once at least that many seconds had passed on
    /// the clock since it was launched.
    pub deadline_passed: bool,
}

/// Runs one `drain` or `resume` through `launcher` and waits for it to end; `clock` is read
/// before the launch and after the end, to tell a `drain` its deadline ended from one that
/// failed. See the module's documentation for the outcome of each end.
pub fn run_maintenance_step(
    launcher: &dyn StepLauncher,
    clock: &dyn Clock,
    call: &MaintenanceCall<'_>,
) -> Result<MaintenanceExit, LaunchError> {
    let started = clock.mono_secs();
    let ran = launcher.run_additive(&AdditiveCall {
        service: call.service,
        step: call.word,
        hook_args: call.hook_args,
        params: call.params,
        executable: call.executable,
        unit: call.unit,
        bound: call.bound,
    })?;
    let spent = clock.mono_secs().saturating_sub(started);
    let exit = StepExit {
        code: ran.code,
        last_line: ran.last_line.filter(|line| line.len() <= LAST_LINE_MAX),
    };
    Ok(read_exit(call.step, call.bound, spent, exit))
}

/// The outcome of a step that ran for `spent` seconds under `bound` and ended with `exit`.
fn read_exit(step: MaintenanceStep, bound: Bound, spent: u64, exit: StepExit) -> MaintenanceExit {
    let deadline_passed = match (step, bound) {
        // Only the ends the launcher gives a step it ended at its bound: no code, or
        // `systemd-run`'s `1`. A halt (`76`) or another code is the hook's own, and fails.
        (MaintenanceStep::Drain, Bound::Secs(deadline)) => {
            matches!(exit.code, None | Some(EXIT_FAILURE)) && spent >= deadline.get()
        }
        _ => false,
    };
    let outcome = match exit.code {
        Some(EXIT_OK) => MaintenanceOutcome::Succeeded,
        Some(EXIT_DEFERRED) => MaintenanceOutcome::Deferred,
        _ if deadline_passed => MaintenanceOutcome::Deferred,
        _ => MaintenanceOutcome::Failed,
    };
    MaintenanceExit {
        outcome,
        exit,
        deadline_passed,
    }
}
