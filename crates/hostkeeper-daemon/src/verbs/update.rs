//! `update`, and through the same path `force_rollback` and `install`: a hooked order (design
//! section 5.2).
//!
//! In order: the service's lock, never waited for (another package's order is `busy`, the same
//! package's a resend that runs nothing); the held halt generation, which must read; the step
//! record and the try rules ([`checks::try_rule`]); where the service is in its life
//! ([`checks::installed_rule`]: nothing while it is being uninstalled, `install` only with no
//! release installed, `update` and `force_rollback` only with one); an `update` below the
//! running version is refused; the order's entry, when it carries one, judged by
//! [`crate::entry::check`] (nothing is fetched for an entry that fails); the package from the
//! [`crate::packages::Packages`] port, outside the step lock (the move's rollback from the sealed
//! copy the host keeps, asking the port for nothing); its manifest, read by the manifest's
//! loader for the order's service and version and this daemon's version; the release it
//! replaces, none for an `install`; the paths it places checked against `placed.json` and the
//! disk (`path_conflict`); then the step lock and [`hostkeeper_steps::StepRunner::run`], whose
//! verdict is the reply. Every refusal before the runner writes nothing. An order that ends `Succeeded`, `promotion_pending` or not,
//! confirms its switch: under its locks, before a pending promotion is watched, the service's
//! package directory is pruned to `current` and `previous` ([`crate::prune::confirmed_switch`]),
//! keeping the order's target. A resend answered from the record prunes nothing.
//!
//! An `install` is the update path with no previous release: no `updater.previous_*` key in the
//! params file and no `previous_digest` in the step record. The runner's switch
//! ([`crate::place::hooked_switch`]) places the package and points `current` at it immediately
//! before `cutover`; before `rollback` it takes an install off the host, and puts an update back
//! on the release it replaced. After `cleanup` it commits the placed paths and removes those the
//! release no longer lists.

use std::num::NonZeroU64;
use std::path::PathBuf;

use hostkeeper_config::manifest::{Expected, HostkeeperVersion, Manifest};
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_config::platform::Platform;
use hostkeeper_hook::{Class, Phase};
use hostkeeper_hook::{Params, Verdict};
use hostkeeper_install::repository::DRIFT_REPAIRED;
use hostkeeper_release::Installer;
use hostkeeper_steps::record::{Loaded, Record, RecordError};
use hostkeeper_steps::{Bounds, Finished, RunError, Staged, UpdateOrder};

use crate::checks::{self, OrderKey, TryRule};
use crate::entry;
use crate::executor::{Ctx, Reply};
use crate::files;
use crate::installed::{self, CURRENT, PREVIOUS};
use crate::locks::{Held, Holder, Package, ServiceGuard};
use crate::packages::{Entry, VersionDir};
use crate::prune;
use crate::refusal::Refusal;
use crate::repository;
use crate::selfupdate;
use crate::target;
use crate::verb::{Hooked, HookedVerb};

/// `update`: refused below the running version, and for a service with no release installed.
pub fn run(ctx: &Ctx, order: Hooked) -> Reply {
    hooked(ctx, HookedVerb::Update, order)
}

/// A hooked order of `verb`.
pub fn hooked(ctx: &Ctx, verb: HookedVerb, order: Hooked) -> Reply {
    locked(ctx, verb, &order, |guard| steps(ctx, verb, &order, guard))
}

/// `steps` of `order` under its service's lock, taken for `verb` and the order's package and
/// never waited for: another package's order is `busy`, the same package's a resend that runs
/// nothing. A step that could not be stopped keeps the lock.
pub(crate) fn locked(
    ctx: &Ctx,
    verb: HookedVerb,
    order: &Hooked,
    steps: impl FnOnce(&ServiceGuard<'_>) -> Result<Reply, Reply>,
) -> Reply {
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
    let reply = steps(&guard).unwrap_or_else(|refused| refused);
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
    if let Some(answer) = answer(verb, order, last.as_deref())? {
        return Ok(answer);
    }
    let prepared = prepare(ctx, verb, order, last.as_deref())?;
    files::make_private(&ctx.service_dir(service))
        .map_err(|e| Reply::refused(Refusal::StoreFailed, e))?;
    let runner = ctx.runner(service);
    let _step = guard.step(&ctx.step_lock);
    // The runner stops where a pending promotion hands the record to the post-success watcher:
    // the update has ended `Succeeded` there, and its switch is confirmed before the watcher
    // runs, so the prune never waits for the promotion. The watcher then runs here, under the
    // same locks, and its end is the reply.
    let update = &prepared.update;
    // Only this update's own `activate` may make it `drift_repaired`: what an earlier force or a
    // resumed update left is dropped.
    drop(ctx.repository().take_repaired(service));
    let ended = runner.run_until_watching(update);
    if let Ok(Finished {
        verdict: Verdict::Succeeded { promotion_pending },
        ..
    }) = &ended
    {
        let watching = *promotion_pending;
        prune::confirmed_switch(ctx, guard, service, &order.version);
        if watching {
            let ended = runner.run(update);
            return Ok(repaired(ctx, service, ran(ended)));
        }
    }
    Ok(repaired(ctx, service, ran(ended)))
}

/// `reply` with the reason `drift_repaired` when the update `Succeeded` with no reason of its own
/// and its `activate` found the package at a version hostkeeper did not set and installed the
/// desired one over it (packages-repositories.md section 4.4). The drift is taken either way,
/// so a later update never reports it.
fn repaired(ctx: &Ctx, service: &ServiceName, reply: Reply) -> Reply {
    let drift = ctx.repository().take_repaired(service);
    match reply {
        Reply::Finished {
            verdict: verdict @ Verdict::Succeeded { .. },
            reason: None,
            resent,
        } if drift.is_some() => Reply::Finished {
            verdict,
            reason: Some(DRIFT_REPAIRED.to_owned()),
            resent,
        },
        other => other,
    }
}

/// The try rules ([`checks::try_rule`]) for `order`, given the service's step record `last`:
/// `Some` reply for a resend of an order that ended, answered from the record, `None` to run it,
/// and the refusal otherwise.
pub(crate) fn answer(
    verb: HookedVerb,
    order: &Hooked,
    last: Option<&Record>,
) -> Result<Option<Reply>, Reply> {
    let key = OrderKey {
        verb,
        version: order.version.clone(),
        digest: order.digest.clone(),
        order_try: order.order_try.get(),
    };
    match checks::try_rule(last, &key) {
        TryRule::Run => Ok(None),
        TryRule::Answer(Finished { verdict, reason }) => Ok(Some(Reply::Finished {
            verdict,
            reason,
            resent: true,
        })),
        TryRule::Refuse(refusal) => {
            let held = last.map_or(0, |rec| rec.order_try.unwrap_or(0));
            Err(Reply::refused(
                refusal,
                format!(
                    "{} {} try {} of {}; the step record holds try {held}",
                    verb.name(),
                    order.version,
                    order.order_try,
                    order.service,
                ),
            ))
        }
    }
}

/// An order checked and ready for the step runner: its package's version directory and the
/// runner's order.
pub(crate) struct Prepared {
    pub dir: VersionDir,
    pub update: UpdateOrder,
}

/// The checks of `order` that follow the try rules, in order, and the runner's order built from
/// its package: where the service is in its life, an `update` below the running version, the
/// package, its manifest, the release it replaces, the paths of a first install and the params.
/// Writes nothing.
pub(crate) fn prepare(
    ctx: &Ctx,
    verb: HookedVerb,
    order: &Hooked,
    last: Option<&Record>,
) -> Result<Prepared, Reply> {
    let service = &order.service;
    let key = OrderKey {
        verb,
        version: order.version.clone(),
        digest: order.digest.clone(),
        order_try: order.order_try.get(),
    };
    let running = installed::linked_version(&ctx.dirs.packages, service, CURRENT);
    // An order the record holds unfinished for this package is resumed, whatever `current` says.
    let resuming = last
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
    if let Some(entry) = &order.entry {
        entry::check(
            ctx.own_version,
            &ctx.installers(),
            service,
            &order.digest,
            entry,
        )?;
    }
    let native = order
        .entry
        .as_ref()
        .and_then(|entry| repository::native_of_entry(entry).map(|native| (native, entry)));
    let moved_back = same_installer(
        ctx,
        service,
        running.as_ref(),
        &order.version,
        order.entry.as_ref(),
    )?;
    let dir = match (moved_back, native) {
        // The move's rollback: the archive version the host keeps, from its sealed copy.
        (true, _) => VersionDir::new(sealed(ctx, service, &order.version)?),
        (false, Some((native, entry))) => {
            ctx.repository()
                .stage(native, service, &order.version, entry)?
        }
        (false, None) => ctx
            .packages
            .version_dir(
                service,
                &order.version,
                &order.digest,
                order.index_digest.as_ref(),
                order.entry.as_ref(),
            )
            .map_err(|e| Reply::refused(checks::package_refusal(&e), e))?,
    };
    let expected = Expected {
        name: service.clone(),
        version: order.version.clone(),
        platform: Platform::current(),
    };
    let manifest = target::manifest(dir.dir(), &expected, &ctx.own_version)
        .map_err(|e| Reply::refused(Refusal::from(&e), e))?;
    let previous = match verb {
        HookedVerb::Install => None,
        HookedVerb::Update | HookedVerb::ForceRollback | HookedVerb::Force => {
            previous(ctx, service, &order.version, running)?
        }
    };
    // The runner's switch places this package, whether or not it replaces a release: its paths
    // are checked now, before anything changes. A native package places nothing: its installer
    // installs its files.
    if native.is_none() {
        claims_check(ctx, &manifest)?;
    }
    let declared = declared_params(&manifest.params)?;
    let update = update_order(order, &dir, &manifest, previous, declared, &ctx.own_version);
    Ok(Prepared { dir, update })
}

/// The installer of the release `running` of `service`, as its sealed `target.json` names it
/// (`hostkeeper-archive` for a version without one), against the order's (`entry`'s, or
/// `hostkeeper-archive` for an order of the release channel): a service keeps the installer it
/// was installed with until it is uninstalled (packages.md section 5.1), so another is
/// `Failed`, `release`, `installer_changed`, with nothing fetched. A version whose `target.json`
/// does not read is `release_unreadable`.
///
/// Two changes are the move of docs/design/packages-native.md, section 10.6, and are accepted:
/// from `hostkeeper-archive` to a native installer (the move), and from a native installer back
/// to `hostkeeper-archive` when the order's version `target` is the archive version `previous`
/// names, still on the host (the move's rollback; once the next update retired it, the move
/// cannot be undone). Back to another native installer, or between two, stays
/// `installer_changed`. `true` for the move's rollback, which [`prepare`] takes from the sealed
/// copy the host keeps, asking the package port for nothing.
fn same_installer(
    ctx: &Ctx,
    service: &ServiceName,
    running: Option<&Version>,
    target: &Version,
    entry: Option<&Entry>,
) -> Result<bool, Reply> {
    let Some(running) = running else {
        return Ok(false);
    };
    let dir = ctx
        .dirs
        .packages
        .join(service.as_str())
        .join(running.as_str());
    let archive = Installer::HostkeeperArchive.as_str();
    let installed = target::read(&dir)
        .map_err(|e| Reply::refused(Refusal::ReleaseUnreadable, e))?
        .map_or_else(|| archive.to_owned(), |sealed| sealed.installer);
    let ordered = entry.map_or(archive, |entry| entry.installer.as_str());
    let native = |name: &str| name.parse::<Installer>().is_ok_and(Installer::is_native);
    let kept = || {
        let dir = ctx
            .dirs
            .packages
            .join(service.as_str())
            .join(target.as_str());
        let previous = installed::linked_version(&ctx.dirs.packages, service, PREVIOUS);
        (previous.as_ref() == Some(target)) & dir.is_dir() & matches!(target::read(&dir), Ok(None))
    };
    let moved = (installed == archive) & native(ordered);
    let moved_back = native(&installed) & (ordered == archive) && kept();
    match (installed == ordered) | moved | moved_back {
        true => Ok(moved_back),
        false => Err(Reply::Finished {
            verdict: Verdict::Failed {
                phase: Phase::BeforeCommit,
                class: Class::Release,
            },
            reason: Some(REASON_INSTALLER_CHANGED.to_owned()),
            resent: false,
        }),
    }
}

/// The reason of an order whose installer is not the one the service runs.
pub const REASON_INSTALLER_CHANGED: &str = "installer_changed";

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
    let seal = |version: Version| sealed(ctx, service, &version).map(|staged| (version, staged));
    replaced.map(seal).transpose()
}

/// The version directory of `service` at `version` the host keeps, sealed; one that does not
/// seal is `release_unreadable`.
fn sealed(ctx: &Ctx, service: &ServiceName, version: &Version) -> Result<Staged, Reply> {
    let dir = ctx
        .dirs
        .packages
        .join(service.as_str())
        .join(version.as_str());
    Staged::seal(&dir).map_err(|e| Reply::refused(Refusal::ReleaseUnreadable, e))
}

/// The order for the step runner: the package's version directory and hook, the bounds and
/// params of its manifest, the release it replaces and the order's digest and try.
fn update_order(
    order: &Hooked,
    dir: &VersionDir,
    manifest: &Manifest,
    previous: Option<(Version, Staged)>,
    declared_params: Params,
    own: &HostkeeperVersion,
) -> UpdateOrder {
    let hook = &manifest.hook;
    let timeouts = &hook.timeout_secs;
    let previous_hook = previous.as_ref().and_then(|(version, staged)| {
        crate::target::previous_hook(staged.dir(), &order.service, version, own)
    });
    let (previous_version, previous) = previous.map(|(v, s)| (v.to_string(), s)).unzip();
    UpdateOrder {
        service: order.service.to_string(),
        target_version: order.version.to_string(),
        previous_version,
        artifact_digest: order.digest.to_string(),
        staged: dir.staged().clone(),
        previous,
        hook_binary: hook.command.first().map(PathBuf::from).unwrap_or_default(),
        hook_args: hook.command.iter().skip(1).cloned().collect(),
        previous_hook,
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
        // A version whose manifest names no command (a repository package without one) runs the
        // built-in steps ([`crate::builtin`]).
        builtin_steps: selfupdate::builtin_steps(&order.service, manifest.min_hostkeeper)
            | hook.command.is_empty(),
    }
}

/// The reply for how the runner ended.
pub(crate) fn ran(result: Result<Finished, RunError>) -> Reply {
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
