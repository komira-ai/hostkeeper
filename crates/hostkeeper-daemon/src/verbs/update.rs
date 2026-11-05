//! `update`, and through the same path `force_rollback` and `install`: a hooked order (design
//! section 5.2).
//!
//! In order: the service's lock, never waited for (another package's order is `busy`, the same
//! package's a resend that runs nothing); the held halt generation, which must read; the step
//! record and the try rules ([`checks::try_rule`]); where the service is in its life
//! ([`checks::installed_rule`]: nothing while it is being uninstalled, `install` only with no
//! release installed, `update` and `force_rollback` only with one); an `update` below the
//! running version is refused; the package from the [`crate::packages::Packages`] port, outside
//! the step lock; its manifest, read by the manifest's loader for the order's service and
//! version and this daemon's version; the release it replaces, none for an `install`; for an
//! order with no release to replace, the paths it places checked against `placed.json`
//! (`path_conflict`); then the step lock and [`hostkeeper_steps::StepRunner::run`], whose verdict
//! is the reply. Every refusal before the runner writes nothing. An order that ends `Succeeded`,
//! `promotion_pending` or not, confirms its switch: under its locks, before a pending promotion is
//! watched, the service's package directory is pruned to `current` and `previous`
//! ([`crate::prune::confirmed_switch`]), keeping the order's target. A resend answered from the
//! record prunes nothing.
//!
//! An `install` is the update path with no previous release: no `updater.previous_*` key in the
//! params file and no `previous_digest` in the step record. The runner's switch
//! ([`crate::executor::Ctx::runner`]) places the package and points `current` at it immediately
//! before `cutover`, and removes both immediately before `rollback`.

use std::num::NonZeroU64;
use std::path::PathBuf;

use hostkeeper_config::manifest::{Expected, Manifest};
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_hook::{Params, Verdict};
use hostkeeper_steps::record::{Loaded, Record, RecordError};
use hostkeeper_steps::{Bounds, Finished, RunError, Staged, UpdateOrder};

use crate::checks::{self, OrderKey, TryRule};
use crate::executor::{Ctx, Reply};
use crate::files;
use crate::installed::{self, CURRENT, PREVIOUS};
use crate::locks::{Held, Holder, Package, ServiceGuard};
use crate::packages::VersionDir;
use crate::prune;
use crate::refusal::Refusal;
use crate::verb::{Hooked, HookedVerb};

/// `update`: refused below the running version, and for a service with no release installed.
pub fn run(ctx: &Ctx, order: Hooked) -> Reply {
    hooked(ctx, HookedVerb::Update, order)
}

/// A hooked order of `verb`.
pub fn hooked(ctx: &Ctx, verb: HookedVerb, order: Hooked) -> Reply {
    let package = Package {
        verb,
        version: order.version.clone(),
        digest: order.digest.clone(),
    };
    let holder = Holder::Package(package.clone());
    let guard = match ctx.locks.take(&order.service, package) {
        Ok(guard) => guard,
        Err(held) => return while_held(&order.service, &held, &holder),
    };
    let reply = steps(ctx, verb, &order, &guard).unwrap_or_else(|refused| refused);
    match reply.refusal() {
        // The step could not be stopped: the service stays locked until `abort` clears it.
        Some(Refusal::Unstopped) => guard.keep(),
        _ => drop(guard),
    }
    reply
}

/// The answer to an order for a service whose lock is held: a resend of the order that holds
/// it runs nothing, any other is `busy`.
pub(crate) fn while_held(service: &ServiceName, held: &Held, holder: &Holder) -> Reply {
    match held.holder == *holder {
        true => Reply::Running,
        false => Reply::refused(
            Refusal::Busy,
            format!("{service} is locked by {}", held.holder),
        ),
    }
}

/// The step record of a service, or the refusal of an order while it cannot be read.
pub(crate) fn last_record(ctx: &Ctx, service: &ServiceName) -> Result<Option<Box<Record>>, Reply> {
    match ctx.store(service).load() {
        Ok(Loaded::Absent) => Ok(None),
        Ok(Loaded::Current(rec)) => Ok(Some(rec)),
        Ok(Loaded::Newer { schema }) => Err(Reply::refused(
            Refusal::RecordNewer,
            format!("the step record of {service} has schema {schema}, newer than this daemon"),
        )),
        Err(RecordError::Corrupt(why)) => Err(Reply::refused(Refusal::RecordUnreadable, why)),
        Err(e) => Err(Reply::refused(Refusal::StoreFailed, e)),
    }
}

fn steps(
    ctx: &Ctx,
    verb: HookedVerb,
    order: &Hooked,
    guard: &ServiceGuard<'_>,
) -> Result<Reply, Reply> {
    let service = &order.service;
    ctx.halts
        .get(service)
        .map_err(|e| Reply::refused(Refusal::HaltUnreadable, e))?;
    let last = last_record(ctx, service)?;
    let key = OrderKey {
        verb,
        version: order.version.clone(),
        digest: order.digest.clone(),
        order_try: order.order_try.get(),
    };
    match checks::try_rule(last.as_deref(), &key) {
        TryRule::Run => {}
        TryRule::Answer(Finished { verdict, reason }) => {
            return Ok(Reply::Finished {
                verdict,
                reason,
                resent: true,
            });
        }
        TryRule::Refuse(refusal) => {
            let held = last.as_ref().map_or(0, |rec| rec.order_try.unwrap_or(0));
            return Err(Reply::refused(
                refusal,
                format!(
                    "{} {} try {} of {service}; the step record holds try {held}",
                    verb.name(),
                    order.version,
                    order.order_try,
                ),
            ));
        }
    }
    let running = installed::linked_version(&ctx.dirs.packages, service, CURRENT);
    // An order the record holds unfinished for this package is resumed, whatever `current` says.
    let resuming = last
        .as_deref()
        .is_some_and(|rec| checks::finished_of(rec).is_none() & checks::same_package(rec, &key));
    let tombstoned = crate::uninstall::tombstoned(ctx, service)
        .map_err(|e| Reply::refused(Refusal::from(&e), e))?;
    checks::installed_rule(verb, running.is_some(), resuming, tombstoned).map_err(|refusal| {
        let had = running.as_ref().map_or("none", Version::as_str);
        Reply::refused(
            refusal,
            format!(
                "{} of {service}: its installed release is {had}{}",
                verb.name(),
                match tombstoned {
                    true => ", and it is being uninstalled",
                    false => "",
                }
            ),
        )
    })?;
    checks::not_below_running(verb, &order.version, running.as_ref()).map_err(|refusal| {
        let running = running.as_ref().map_or("", Version::as_str);
        Reply::refused(refusal, format!("{} is below {running}", order.version))
    })?;
    let dir = ctx
        .packages
        .version_dir(service, &order.version, &order.digest)
        .map_err(|e| Reply::refused(checks::package_refusal(&e), e))?;
    let expected = Expected {
        name: service.clone(),
        version: order.version.clone(),
    };
    let manifest = Manifest::load(dir.dir(), &expected, &ctx.own_version)
        .map_err(|e| Reply::refused(Refusal::from(&e), e))?;
    let previous = match verb {
        HookedVerb::Install => None,
        HookedVerb::Update | HookedVerb::ForceRollback | HookedVerb::Force => {
            previous(ctx, service, &order.version, running)?
        }
    };
    // With no release to replace, the runner's switch places this package: its paths are
    // checked now, before anything changes.
    if previous.is_none() {
        claims_check(ctx, &manifest)?;
    }
    let declared = declared_params(&manifest.params)?;
    let update = update_order(order, &dir, &manifest, previous, declared);
    files::make_private(&ctx.service_dir(service))
        .map_err(|e| Reply::refused(Refusal::StoreFailed, e))?;
    let runner = ctx.runner(service);
    let _step = guard.step(&ctx.step_lock);
    // The runner stops where a pending promotion hands the record to the post-success watcher:
    // the update has ended `Succeeded` there, and its switch is confirmed before the watcher
    // runs, so the prune never waits for the promotion. The watcher then runs here, under the
    // same locks, and its end is the reply.
    let ended = runner.run_until_watching(&update);
    if let Ok(Finished {
        verdict: Verdict::Succeeded { promotion_pending },
        ..
    }) = &ended
    {
        let watching = *promotion_pending;
        prune::confirmed_switch(ctx, guard, service, &order.version);
        if watching {
            return Ok(ran(runner.run(&update)));
        }
    }
    Ok(ran(ended))
}

/// The paths `manifest` places, checked against `placed.json` and what is on disk: a path
/// another service owns, or one occupied by anything the record does not list, is
/// `path_conflict`; a unit or `[[files]]` entry that breaks a rule of linking is
/// `manifest_mismatch`. Reads only.
fn claims_check(ctx: &Ctx, manifest: &Manifest) -> Result<(), Reply> {
    let placer = ctx.placer();
    placer
        .desired(manifest)
        .and_then(|claims| placer.check(&claims))
        .map_err(|e| Reply::refused(Refusal::from(&e), e))
}

/// A manifest's `[params]` as the hook's declared params. The manifest's loader already refuses
/// what [`Params::parse_declared`] refuses (a non-scalar, an `updater.*` key); a table it let
/// through that this refuses is `manifest_mismatch` all the same.
pub fn declared_params(table: &toml::Table) -> Result<Params, Reply> {
    Params::parse_declared(&table.to_string())
        .map_err(|e| Reply::refused(Refusal::ManifestMismatch, e))
}

/// The release the order replaces, sealed: the one `current` names, or, when `current` already
/// names the target (a switch the daemon made before it stopped), the one `previous` names.
/// `None` for a service that runs no version.
fn previous(
    ctx: &Ctx,
    service: &ServiceName,
    target: &Version,
    running: Option<Version>,
) -> Result<Option<(Version, Staged)>, Reply> {
    let replaced = match running.as_ref() == Some(target) {
        true => installed::linked_version(&ctx.dirs.packages, service, PREVIOUS),
        false => running,
    };
    let seal = |version: Version| {
        let dir = ctx
            .dirs
            .packages
            .join(service.as_str())
            .join(version.as_str());
        Staged::seal(&dir)
            .map(|staged| (version, staged))
            .map_err(|e| Reply::refused(Refusal::ReleaseUnreadable, e))
    };
    replaced.map(seal).transpose()
}

/// The order for the step runner: the package's version directory and hook, the bounds and
/// params of its manifest, the release it replaces and the order's digest and try.
fn update_order(
    order: &Hooked,
    dir: &VersionDir,
    manifest: &Manifest,
    previous: Option<(Version, Staged)>,
    declared_params: Params,
) -> UpdateOrder {
    let hook = &manifest.hook;
    let timeouts = &hook.timeout_secs;
    let (previous_version, previous) = previous.map(|(v, s)| (v.to_string(), s)).unzip();
    UpdateOrder {
        service: order.service.to_string(),
        target_version: order.version.to_string(),
        previous_version,
        artifact_digest: order.digest.to_string(),
        staged: dir.staged().clone(),
        previous,
        // The program only: the leading arguments of `command` have no field in the step
        // launcher's call yet, which carries the step's own arguments alone.
        hook_binary: hook.command.first().map(PathBuf::from).unwrap_or_default(),
        bounds: Bounds {
            // `prepare = 0` is the unbounded `prepare` of the manifest.
            prepare_secs: timeouts.prepare.map_or(0, NonZeroU64::get),
            cutover_secs: timeouts.cutover.get(),
            verify_secs: timeouts.verify.get(),
            rollback_secs: timeouts.rollback.get(),
            cleanup_secs: timeouts.cleanup.get(),
            verify_deadline_secs: hook.verify_deadline_secs,
            // No ceiling: the watcher runs until `verify` passes without the flag, or fails.
            promotion_ceiling_secs: hook
                .promotion_ceiling_secs
                .map_or(u64::MAX, NonZeroU64::get),
            cleanup_retries: hook.cleanup_retries,
        },
        declared_params,
        min_updater_protocol: None,
        order_try: Some(order.order_try.get()),
    }
}

/// The reply for how the runner ended.
fn ran(result: Result<Finished, RunError>) -> Reply {
    match result {
        Ok(Finished { verdict, reason }) => Reply::Finished {
            verdict,
            reason,
            resent: false,
        },
        Err(error) => interrupted(error),
    }
}

/// The reply for a runner that stopped without a verdict: its error as a token, and its text.
pub fn interrupted(error: RunError) -> Reply {
    let refusal = match &error {
        RunError::Unstopped { .. } => Refusal::Unstopped,
        RunError::Launch(_) => Refusal::LauncherFailed,
        RunError::Record(_) => Refusal::StoreFailed,
        RunError::Unreadable(_) => Refusal::ReleaseUnreadable,
        RunError::Busy { .. } => Refusal::Busy,
        RunError::RecordNewer(_) => Refusal::RecordNewer,
        RunError::Switch(_) => Refusal::LinkFailed,
        RunError::BadOrder(_) | RunError::UnitNameTooLong { .. } => Refusal::InvalidOrder,
    };
    Reply::Interrupted {
        refusal,
        detail: error.to_string(),
    }
}
