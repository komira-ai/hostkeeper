//! The prune after a confirmed switch (design sections 4.7, 5.6 step 18 and 8.7, and Q14): every
//! version directory of the service but the ones the host keeps is deleted, with its seal, under
//! the service's lock.
//!
//! A switch is confirmed when an update ends `Succeeded` (one with `promotion_pending` counts),
//! [`crate::verbs::update`], or a force moves `current`, whatever its verdict,
//! [`crate::verbs::force`]. Those are the only two callers of [`confirmed_switch`], and both hold
//! the service's lock already: the function takes its guard, so a prune without it does not
//! compile, and it takes no other lock (not the step lock, not the lock of `placed.json`). Nothing
//! else prunes: not a failed, deferred, halted or `forced_over` update, not the start pass, not
//! `Status`, and not a package that could not be staged for want of space, since nothing is
//! pruned to make room.
//!
//! A prune keeps, for these reasons:
//!
//! - the versions `current` and `previous` name;
//! - the version the service's pin names ([`crate::pin`]);
//! - what the service's records say is still in use: of an unfinished step record, the release it
//!   replaces and its target; the target of a force record with no verdict; and the version the
//!   confirmed switch went to;
//! - the newest [`Ctx::keep_versions`] others that are whole (a directory and a seal), newest by
//!   the time each was staged (`--keep-versions`, [`DEFAULT_KEEP_VERSIONS`] by default).
//!
//! A version staged by an update that did not switch is one of the others: it stays until a
//! confirmed switch finds it beyond the newest kept, so a retry needs no fetch.
//!
//! A prune never runs during a trial: while the service's step record holds an update that is not
//! over, and not in the post-success watch, it deletes nothing. Such an update may still switch
//! `current` to its target or back (`rollback`), and the updater's own update waits unfinished
//! for its new binary; the watch never switches back, since its commit has happened. Neither
//! caller confirms a switch while one is under way, so this is the rule's guard, not a path a
//! caller takes; the next confirmed switch prunes.
//!
//! The rule and the deletion are `hostkeeper_steps::prune`: the deletion of a version's directory
//! and seal, which is the same for every installer and all a repository installer's `retire`
//! has to do. A prune that fails, wholly or for one version, is logged as the alert
//! [`ALERT_PRUNE`] and never changes the verdict of the order that confirmed the switch; the
//! next confirmed switch tries again. A pin that cannot be read fails the prune the same way.

use std::collections::BTreeSet;
use std::io;

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_steps::record::{Loaded, Record, State};
use hostkeeper_steps::{Layout, Protected, PruneReport, Seal, read_seal};

use crate::executor::Ctx;
use crate::installers;
use crate::locks::ServiceGuard;
use crate::pin;

/// The alert of a prune that failed; the verdict of the order stands.
pub const ALERT_PRUNE: &str = "prune_failed";

/// How many versions of a service a host keeps besides `current`, `previous`, the pin and what
/// the records use, when `--keep-versions` does not say.
pub const DEFAULT_KEEP_VERSIONS: u64 = 4;

/// The most `--keep-versions` accepts.
pub const MAX_KEEP_VERSIONS: u64 = 32;

/// Why a prune failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the old versions of {service} could not be pruned: {why}")]
pub struct PruneError {
    pub service: String,
    pub why: String,
}

/// Deletes the version directories a confirmed switch leaves behind.
pub trait Prune: Send + Sync {
    /// Deletes every version of the package directory `layout` but the ones `current` and
    /// `previous` name, the ones `protect` names and the newest it keeps. `held` is the service's
    /// lock, which the caller holds for the whole call.
    fn prune(
        &self,
        layout: &Layout,
        protect: &Protected,
        held: &ServiceGuard<'_>,
    ) -> io::Result<PruneReport>;
}

/// The prune of the host: [`Layout::prune`].
#[derive(Clone, Copy, Debug, Default)]
pub struct LayoutPrune;

impl Prune for LayoutPrune {
    fn prune(
        &self,
        layout: &Layout,
        protect: &Protected,
        _held: &ServiceGuard<'_>,
    ) -> io::Result<PruneReport> {
        layout.prune(protect)
    }
}

/// The point of a force between its switch of `current` and its prune, with the service's lock
/// held: a test interleaves another order there.
pub trait ForceLockPoint: Send + Sync {
    fn after_switch_before_prune(&self);
}

/// The force lock point of the host: nothing happens there.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoLockPoint;

impl ForceLockPoint for NoLockPoint {
    fn after_switch_before_prune(&self) {}
}

/// The prune after the switch of `service` to `to` was confirmed, with `held` its lock: the
/// versions [`protected`] names, `to` and the newest [`Ctx::keep_versions`] others are kept
/// besides `current` and `previous`. A failure is the alert [`ALERT_PRUNE`]. The report is `None`
/// when nothing was pruned: nothing could be read (with the alert), or an update of the service is
/// on trial (with none).
pub fn confirmed_switch(
    ctx: &Ctx,
    held: &ServiceGuard<'_>,
    service: &ServiceName,
    to: &Version,
) -> Option<PruneReport> {
    let failed = |why: String| {
        let error = PruneError {
            service: service.to_string(),
            why,
        };
        ctx.alert(ALERT_PRUNE, &format!("service={service}: {error}"));
    };
    let layout = Layout::package(&ctx.dirs.packages.join(service.as_str()));
    let mut protect = match protected(ctx, &layout, service) {
        Ok(Some(protected)) => protected,
        Ok(None) => return None,
        Err(why) => {
            failed(why);
            return None;
        }
    };
    protect.switch_targets.insert(to.to_string());
    protect.newest = usize::try_from(ctx.keep_versions()).unwrap_or(usize::MAX);
    match ctx.prune.prune(&layout, &protect, held) {
        Ok(report) => {
            let each: Vec<String> = report
                .failed
                .iter()
                .map(|(version, why)| format!("{version}: {why}"))
                .collect();
            if !each.is_empty() {
                failed(each.join("; "));
            }
            Some(report)
        }
        Err(error) => {
            failed(error.to_string());
            None
        }
    }
}

/// Whether `rec` holds an update on trial: not over, and not in the post-success watch.
fn on_trial(rec: &Record) -> bool {
    !matches!(rec.state, State::Finished { .. }) & rec.progress.watching.is_none()
}

/// What `service` keeps besides `current`, `previous` and the newest: of an unfinished step
/// record in the post-success watch, its target and the release it replaces (the version whose
/// seal holds the record's `previous_digest`); the target of a force record with no verdict; and
/// the version its pin names. `None` when an update of the service is on trial ([`on_trial`]):
/// nothing is to be pruned. An error, with nothing pruned, when a record or the pin cannot be
/// read, or `placed.json` names an installer of the service this daemon has no steps of: what
/// they hold cannot be known, or not undone.
fn protected(
    ctx: &Ctx,
    layout: &Layout,
    service: &ServiceName,
) -> Result<Option<Protected>, String> {
    let mut protect = Protected::default();
    let rec = match ctx.store(service).load() {
        Ok(Loaded::Absent) => None,
        Ok(Loaded::Current(rec)) => Some(rec),
        Ok(Loaded::Newer { schema }) => {
            return Err(format!(
                "the step record has schema {schema}, newer than this daemon"
            ));
        }
        Err(error) => return Err(error.to_string()),
    };
    if let Some(rec) = rec {
        if on_trial(&rec) {
            return Ok(None);
        }
        if !matches!(rec.state, State::Finished { .. }) {
            protect.switch_targets.insert(rec.target_version.clone());
            if let Some(digest) = &rec.previous_digest {
                protect.previous_releases = sealed_as(layout, digest)?;
            }
        }
        if let Some(force) = rec.force.as_ref().filter(|force| force.verdict.is_none()) {
            protect.switch_targets.insert(force.target_version.clone());
        }
    }
    let placed = ctx
        .placer()
        .store()
        .load()
        .map_err(|error| error.to_string())?;
    installers::steps_for(&placed, service)?;
    let pinned = pin::read(&pin::path(&ctx.dirs.state, service)).map_err(|e| e.to_string())?;
    protect
        .pinned
        .extend(pinned.map(|version| version.to_string()));
    Ok(Some(protect))
}

/// The versions of `layout` whose seal holds the tree digest `digest`. A seal that cannot be read
/// names no version.
fn sealed_as(layout: &Layout, digest: &str) -> Result<BTreeSet<String>, String> {
    let inputs = layout
        .prune_inputs(&Protected::default())
        .map_err(|error| error.to_string())?;
    let sealed = inputs.seals.into_keys().filter(|version| {
        read_seal(&layout.seal_path(version))
            .ok()
            .and_then(|bytes| Seal::parse(&bytes).ok())
            .is_some_and(|seal| seal.tree_digest() == digest)
    });
    Ok(sealed.collect())
}
