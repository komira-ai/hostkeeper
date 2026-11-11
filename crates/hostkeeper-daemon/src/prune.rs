//! The prune after a confirmed switch (design sections 4.7, 5.6 step 18 and 8.7): every version
//! directory of the service but the ones `current` and `previous` name is deleted, with its seal,
//! under the service's lock.
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
//! Besides `current` and `previous`, a prune keeps what the service's records say is still in
//! use: the release an unfinished step record replaces, the target of an
//! unfinished step record or force record, and the version the confirmed switch went to. A version staged by an update
//! that did not switch stays until the next confirmed switch, so a retry needs no fetch.
//!
//! The rule and the deletion are `hostkeeper_steps::prune`: the deletion of a version's directory
//! and seal, which is the same for every installer and all a repository installer's `retire`
//! has to do. A prune that fails, wholly or for one version, is logged as the alert
//! [`ALERT_PRUNE`] and never changes the verdict of the order that confirmed the switch; the
//! next confirmed switch tries again.

use std::collections::BTreeSet;
use std::io;

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_steps::record::{Loaded, State};
use hostkeeper_steps::{Layout, Protected, PruneReport, Seal, read_seal};

use crate::executor::Ctx;
use crate::installers;
use crate::locks::ServiceGuard;

/// The alert of a prune that failed; the verdict of the order stands.
pub const ALERT_PRUNE: &str = "prune_failed";

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
    /// `previous` name and the ones `protect` names. `held` is the service's lock, which the
    /// caller holds for the whole call.
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
/// versions [`protected`] names and `to` are kept besides `current` and `previous`. A failure is
/// the alert [`ALERT_PRUNE`]; the report is `None` when nothing could be read.
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
        Ok(protected) => protected,
        Err(why) => {
            failed(why);
            return None;
        }
    };
    protect.switch_targets.insert(to.to_string());
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

/// What the records of `service` say is still in use, besides `current` and `previous`: of an
/// unfinished step record, its target and the release it replaces (the version whose seal holds
/// the record's `previous_digest`); and the target of a force record with no verdict. An error,
/// with nothing pruned, when a record cannot be read, or `placed.json` names an installer of the
/// service this daemon has no steps of: what it holds cannot be known, or not undone.
fn protected(ctx: &Ctx, layout: &Layout, service: &ServiceName) -> Result<Protected, String> {
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
    Ok(protect)
}

/// The versions of `layout` whose seal holds the tree digest `digest`. A seal that cannot be read
/// names no version.
fn sealed_as(layout: &Layout, digest: &str) -> Result<BTreeSet<String>, String> {
    let inputs = layout
        .prune_inputs(&Protected::default())
        .map_err(|error| error.to_string())?;
    let sealed = inputs.seals.into_iter().filter(|version| {
        read_seal(&layout.seal_path(version))
            .ok()
            .and_then(|bytes| Seal::parse(&bytes).ok())
            .is_some_and(|seal| seal.tree_digest() == digest)
    });
    Ok(sealed.collect())
}
