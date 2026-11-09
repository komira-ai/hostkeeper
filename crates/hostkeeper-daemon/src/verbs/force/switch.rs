//! Steps 7 to 18 of a force (design section 5.6), under the service's lock only, each part
//! journaled in the force record before the next begins:
//!
//! 9. the `run` units of the version left are stopped, those its manifest names; a unit that is
//!    not running is no error;
//! 10. nothing: no version this daemon stages places a path of its own, so there is none to
//!     unlink;
//! 11. the target's installer activates it ([`crate::repository::Repository::activate_version`]):
//!     its tree checked against its seal first, the package installed, its units enabled, and
//!     `current` switched to the target; `previous` becomes the version left, and a switch to the
//!     version `current` names changes nothing;
//! 12. nothing, for the same reason;
//! 13. `daemon-reload`;
//! 14. and 15. the target's `run` units are started, and each must be seen active once within
//!     `force_start_secs`, polled on the daemon's clock;
//! 16. the verdict, `Succeeded` or `Failed` with `link_failed` (steps 9 to 12),
//!     `daemon_reload_failed` (step 13) or `start_failed` (steps 14 and 15);
//! 18. when the force switched `current` to another version, the prune, whatever the verdict
//!     ([`crate::prune::confirmed_switch`]), after the force lock point
//!     ([`crate::prune::ForceLockPoint`]).
//!
//! Step 17, the report, is the verdict in the record: the status read model reports it
//! ([`crate::ServiceStatus::forced`]). After a failure the host stays where it is: nothing is
//! rolled back, the daemon does not try again, and the repair is a second force. Every part is
//! safe to run twice, so a force journaled with no verdict is run again from its top.

use std::collections::BTreeSet;

use hostkeeper_config::manifest::{Expected, Manifest};
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_hook::{Class, Phase, Verdict};
use hostkeeper_steps::record::{Record, State};
use hostkeeper_steps::{ForceFailure, ForcePhase, ForceRecord, ForceVerdict, Layout, RecordStore};

use crate::executor::{Ctx, Reply};
use crate::locks::ServiceGuard;
use crate::prune;
use crate::refusal::Refusal;

/// The alert of a force that ended `Failed` once it had begun to change the host.
pub const ALERT_FORCE_FAILED: &str = "force_failed";

/// Why the parts of a force stopped before the end.
enum Stop {
    /// A part failed: the force ends `Failed` with this reason, and the text says what failed.
    Failed(ForceFailure, String),
    /// The record could not be written: the force stops without a verdict, and a resend of the
    /// order, or the next start, runs it again from its top.
    Journal(Reply),
}

/// A part of steps 9 to 12 that failed.
fn link_failed(error: impl ToString) -> Stop {
    Stop::Failed(ForceFailure::LinkFailed, error.to_string())
}

/// Steps 8 to 18 for the force `force` journals in `rec`, to `target`, with `held` the service's
/// lock.
pub(super) fn run(
    ctx: &Ctx,
    held: &ServiceGuard<'_>,
    mut rec: Record,
    mut force: ForceRecord,
    target: &Manifest,
) -> Reply {
    let service = &target.name;
    let store = ctx.store(service);
    let failure = match parts(ctx, &store, &mut rec, &mut force, target) {
        Ok(()) => None,
        Err(Stop::Failed(failure, why)) => {
            ctx.alert(
                ALERT_FORCE_FAILED,
                &format!("service={service}: {}: {why}", failure.token()),
            );
            Some(failure)
        }
        Err(Stop::Journal(reply)) => return reply,
    };
    let switched = force.phase >= ForcePhase::Switched;
    let (verdict, reason) = verdict_of(switched, failure);
    force.verdict = Some(match failure {
        None => ForceVerdict::Succeeded,
        Some(reason) => ForceVerdict::Failed { reason },
    });
    force.phase = ForcePhase::Verdicted;
    rec.state = State::Finished {
        verdict: verdict.clone(),
        reason: reason.clone(),
    };
    let moved = switched & force.moves_current();
    rec.force = Some(force);
    if let Err(error) = store.save(&rec) {
        return Reply::Interrupted {
            refusal: Refusal::StoreFailed,
            detail: error.to_string(),
        };
    }
    // Step 18: a switch of `current` is confirmed whatever the verdict. A prune that fails is
    // an alert, and the verdict stands.
    if moved {
        ctx.force_lock_point.after_switch_before_prune();
        prune::confirmed_switch(ctx, held, service, &target.version);
    }
    Reply::Finished {
        verdict,
        reason,
        resent: false,
    }
}

/// The verdict of a force and its reason: a failure after the switch is past the commit, one
/// before it is not; a target whose units do not start is blamed on the release, a failure to
/// link or reload on the host.
fn verdict_of(switched: bool, failure: Option<ForceFailure>) -> (Verdict, Option<String>) {
    let Some(failure) = failure else {
        return (
            Verdict::Succeeded {
                promotion_pending: false,
            },
            None,
        );
    };
    let phase = match switched {
        true => Phase::AfterCommit,
        false => Phase::BeforeCommit,
    };
    let class = match failure {
        ForceFailure::StartFailed => Class::Release,
        ForceFailure::LinkFailed | ForceFailure::DaemonReloadFailed => Class::Environment,
    };
    (
        Verdict::Failed { phase, class },
        Some(failure.token().to_owned()),
    )
}

/// Writes the force record at `phase`, before the part that follows it begins. The recorded phase
/// never goes back: a force run again from its top keeps the furthest phase it reached, so that a
/// failure on the second run still counts the switch it made on the first.
fn journal(
    store: &RecordStore,
    rec: &mut Record,
    force: &mut ForceRecord,
    phase: ForcePhase,
) -> Result<(), Stop> {
    force.phase = force.phase.max(phase);
    rec.force = Some(force.clone());
    store.save(rec).map_err(|error| {
        Stop::Journal(Reply::Interrupted {
            refusal: Refusal::StoreFailed,
            detail: error.to_string(),
        })
    })
}

/// Steps 8 to 15. Steps 10 and 12 have nothing to do for a version a repository installer
/// staged, which places nothing, and stay in the journal as the phases a record of an earlier
/// release names.
fn parts(
    ctx: &Ctx,
    store: &RecordStore,
    rec: &mut Record,
    force: &mut ForceRecord,
    target: &Manifest,
) -> Result<(), Stop> {
    let service = &target.name;
    let placer = ctx.placer();
    journal(store, rec, force, ForcePhase::Started)?;
    stop_left(ctx, service, force.from_version.as_deref()).map_err(link_failed)?;
    journal(store, rec, force, ForcePhase::UnitsStopped)?;
    let layout = Layout::package(&ctx.dirs.packages.join(service.as_str()));
    let dir = layout.version_path(target.version.as_str());
    let entry = crate::target::read(&dir)
        .map_err(link_failed)?
        .ok_or_else(|| {
            link_failed(format!(
                "{} holds no {}: it was not staged by an installer of this daemon",
                dir.display(),
                crate::target::TARGET_FILE
            ))
        })?;
    journal(store, rec, force, ForcePhase::Unlinked)?;
    layout
        .check_tree(target.version.as_str())
        .map_err(link_failed)?;
    // A force reports no `drift_repaired`: what its `activate` repaired is dropped. The
    // installer's `activate` installs the package, enables its units and switches `current`
    // once the package manager shows it.
    let repository = ctx.repository();
    let activated = repository.activate_version(service, &target.version, &dir, &entry);
    drop(repository.take_repaired(service));
    activated.map_err(link_failed)?;
    journal(store, rec, force, ForcePhase::Switched)?;
    journal(store, rec, force, ForcePhase::Linked)?;
    placer
        .reload()
        .map_err(|error| Stop::Failed(ForceFailure::DaemonReloadFailed, error.to_string()))?;
    journal(store, rec, force, ForcePhase::Reloaded)?;
    start(ctx, &target.run).map_err(|why| Stop::Failed(ForceFailure::StartFailed, why))?;
    journal(store, rec, force, ForcePhase::UnitsStarted)
}

/// Step 9: stops the `run` units of the version `from`, which `current` named when the force
/// began, those its manifest names.
fn stop_left(ctx: &Ctx, service: &ServiceName, from: Option<&str>) -> Result<(), String> {
    let manifest = from
        .and_then(|from| Version::new(from).ok())
        .and_then(|version| {
            let dir = ctx
                .dirs
                .packages
                .join(service.as_str())
                .join(version.as_str());
            let expected = Expected {
                name: service.clone(),
                version,
                platform: hostkeeper_config::platform::Platform::current(),
            };
            crate::target::manifest(&dir, &expected, &ctx.own_version).ok()
        });
    let units: BTreeSet<String> = manifest
        .map(|manifest| manifest.run)
        .unwrap_or_default()
        .into_iter()
        .collect();
    for unit in &units {
        ctx.units
            .stop_unit(unit)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Steps 14 and 15: starts each of `run`, then asks after each until it has been seen active
/// once, every [`super::FORCE_POLL_SECS`] on the daemon's clock, within `force_start_secs` of the start
/// ([`crate::builtin::start_and_wait`]). An answer that is an error counts as not active.
fn start(ctx: &Ctx, run: &[String]) -> Result<(), String> {
    crate::builtin::start_and_wait(
        &*ctx.units,
        &*ctx.clock,
        run,
        ctx.config.bounds.force_start_secs,
        "force_start_secs",
    )
}
