//! `force`: the hookless switch of a service to a version (design section 5.6, the host's half).
//! No hook runs, the step lock is never taken, and running work is cut: a force is the one
//! sanctioned exception to "drains never cut work".
//!
//! In order:
//!
//! - Refused before any lock, fetch or write: the service `hostkeeper` (`self_service`: it
//!   updates itself by its own path); a service whose first install is under way (`installing`)
//!   or whose uninstall is unfinished (`uninstalling`), by [`checks::force_allowed`]; an order whose fields break their grammars, an empty or
//!   malformed digest included (refused as the order is decoded, [`crate::verb`]); a held halt
//!   generation or a step record that cannot be read; and the try rules
//!   ([`checks::try_rule_with`] held to [`Checks::for_force`]): a resend of a force that ended is
//!   answered from its record, `try_conflict` and `stale_try` are as for any order, and neither
//!   `busy` (for a hooked update of the service) nor `halted` applies. A version below the
//!   running one is not refused: the host keeps no floor of its own. The order's entry, when it
//!   carries one, is judged next by [`crate::entry::check`], as for any order that names a
//!   package, and the port is given it.
//! - Steps 1 to 3, before anything on the host changes ([`obtain`]): the version directory, as
//!   the host holds it when its tree still has its seal's digest and the seal names the order's
//!   package digest, else from the [`crate::packages::Packages`] port (fetched, verified,
//!   unpacked and sealed); its manifest read for the order's service and version and this
//!   daemon's version; and the path check of the placed-path linker against `placed.json`. A
//!   failure is the verdict `Failed` before the commit with the reason the port, the loader or
//!   the linker gives, and nothing is written.
//! - The standing halt: an order whose halt generation is greater than the held one raises the
//!   held generation and clears a `Halt` record
//!   ([`hostkeeper_steps::StepRunner::clear_halt`]); a lower or equal one leaves the hold as it
//!   is, and the force goes on.
//! - Steps 4 to 6 ([`locked`]): a post-success watcher of the service is stopped; an unfinished
//!   hooked update in the record is flagged `forced` under the record lock and the unit of its
//!   running step stopped by name (tried twice; then the verdict `Failed`, `unstopped`, and
//!   nothing else changed); then the force waits, polling on the daemon's clock, for the
//!   service's lock, which the update's runner yields once it has closed its record
//!   `forced_over`. A lock held for good by a step that could not be stopped is taken over.
//! - Steps 7 to 18 under the service's lock only ([`switch`]): the force record, then the switch,
//!   each part journaled in the record before the next begins.
//!
//! A force record with no verdict is the journal of a force under way. A resend of the same
//! order, and the start pass ([`resume`]), run steps 7 to 16 again from its top with the recorded
//! target, fetching it again only when its directory is gone: a resend under the same checks as
//! the order; the start pass, whose record keeps no index digest, against the package digest, the
//! index entry and the attestation, with no check of the index's own SHA-256.

mod switch;

use std::fmt::Display;
use std::path::{Path, PathBuf};

use hostkeeper_config::manifest::{Class as ManifestClass, Expected, Manifest};
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_hook::{Class, Phase, Step, Verdict};
use hostkeeper_proto::hostkeeper::release::v1::Target as Entry;
use hostkeeper_steps::record::{HookSel, Progress, Record, SCHEMA, State};
use hostkeeper_steps::{Finished, ForcePhase, ForceRecord, Layout, Seal, read_seal};

use crate::boot::ALERT_UNSTOPPABLE;
use crate::checks::{self, Checks, OrderKey, TryRule};
use crate::entry;
use crate::executor::{Ctx, Reply};
use crate::files;
use crate::installed::{self, CURRENT};
use crate::locks::{Holder, Package, ServiceGuard};
use crate::packages::PackageError;
use crate::refusal::Refusal;
use crate::verb::{Forced, HookedVerb, PackageDigest};
use crate::verbs::update::{last_record, while_held};

pub use crate::prune::ALERT_PRUNE;
pub use switch::ALERT_FORCE_FAILED;

/// The service a force is refused for: hostkeeper updates itself by its own path.
pub const SELF_SERVICE: &str = "hostkeeper";
/// The seconds between two looks of a force, on the daemon's clock: for the service's lock while
/// the runner of a stopped step yields it (step 6), and for the target's units to be active
/// (step 15).
pub const FORCE_POLL_SECS: u64 = 1;

/// The package a force switches to, read and checked.
struct Target {
    manifest: Manifest,
    /// The digest of its version directory, as its seal holds it.
    tree_digest: String,
}

/// The lifecycle rule of a force ([`checks::force_allowed`]): a force of a service whose first
/// install is under way, or whose uninstall is unfinished, is refused, and nothing changes. A
/// `placed.json` that cannot be read refuses it too.
fn lifecycle_check(ctx: &Ctx, service: &ServiceName) -> Result<(), Reply> {
    let lifecycle = ctx
        .lifecycle(service)
        .map_err(|e| Reply::refused(Refusal::from(&e), e))?;
    checks::force_allowed(lifecycle).map_err(|refusal| {
        Reply::refused(refusal, format!("force of {service}: it is {lifecycle:?}"))
    })
}

/// A `force`.
pub fn run(ctx: &Ctx, order: Forced) -> Reply {
    let service = order.package.service.clone();
    if service.as_str() == SELF_SERVICE {
        return Reply::refused(
            Refusal::SelfService,
            format!("{service} updates itself by its own path, never by a force"),
        );
    }
    if let Err(refused) = lifecycle_check(ctx, &service) {
        return refused;
    }
    let held_halt = match ctx.halts.get(&service) {
        Ok(held) => held,
        Err(error) => return Reply::refused(Refusal::HaltUnreadable, error),
    };
    let package = Package {
        verb: HookedVerb::Force,
        version: order.package.version.clone(),
        digest: order.package.digest.clone(),
    };
    // The same force under way is a resend that runs nothing; another force is `busy`. A hooked
    // order, or a step that could not be stopped, is stopped below. The record is read once the
    // lock is had, so that a resend arriving just as the force ends reads its verdict.
    let guard = match ctx.locks.take(&service, package.clone()) {
        Ok(guard) => Some(guard),
        Err(held) if held.is_force() => {
            return while_held(&service, &held, &Holder::Package(package));
        }
        Err(_) => None,
    };
    let last = match last_record(ctx, &service) {
        Ok(last) => last,
        Err(refused) => return refused,
    };
    let key = OrderKey {
        verb: HookedVerb::Force,
        version: package.version.clone(),
        digest: package.digest.clone(),
        order_try: order.package.order_try.get(),
    };
    match checks::try_rule_with(last.as_deref(), &key, Checks::for_force()) {
        TryRule::Run => {}
        TryRule::Answer(Finished { verdict, reason }) => {
            return Reply::Finished {
                verdict,
                reason,
                resent: true,
            };
        }
        TryRule::Refuse(refusal) => {
            let held = last.as_ref().map_or(0, |rec| rec.order_try.unwrap_or(0));
            return Reply::refused(
                refusal,
                format!(
                    "force {} try {} of {service}; the step record holds try {held}",
                    order.package.version, order.package.order_try,
                ),
            );
        }
    }
    // The entry of the order, when it carries one, is judged before anything is fetched.
    let judged = order.package.entry.as_ref().map_or(Ok(()), |entry| {
        entry::check(ctx.own_version, &service, &order.package.digest, entry)
    });
    if let Err(refused) = judged {
        return refused;
    }
    // The try rules let through a force record with no verdict only for this very order.
    let unfinished = last.and_then(|rec| {
        let force = rec.force.clone().filter(|force| force.verdict.is_none())?;
        Some((*rec, force))
    });
    match unfinished {
        Some((rec, force)) => match locked(ctx, &service, &package, guard) {
            Ok(guard) => resume_with(
                ctx,
                &guard,
                &service,
                (rec, force),
                (
                    &package,
                    order.package.index_digest.as_ref(),
                    order.package.entry.as_ref(),
                ),
            ),
            Err(reply) => reply,
        },
        None => fresh(ctx, &order, held_halt, &package, guard),
    }
}

/// The start pass's resume of the force `rec` holds with no verdict (design section 6.5):
/// steps 7 to 16 again from the top of the record, under the service's lock.
pub fn resume(ctx: &Ctx, service: &ServiceName, rec: Record, force: ForceRecord) -> Reply {
    let parsed = Version::new(&force.target_version)
        .ok()
        .zip(PackageDigest::new(&force.target_digest));
    let Some((version, digest)) = parsed else {
        return Reply::refused(
            Refusal::RecordUnreadable,
            format!("the force record of {service} names no version and digest to switch to"),
        );
    };
    let package = Package {
        verb: HookedVerb::Force,
        version,
        digest,
    };
    match locked(ctx, service, &package, None) {
        Ok(guard) => resume_with(ctx, &guard, service, (rec, force), (&package, None, None)),
        Err(reply) => reply,
    }
}

/// A force with no record of its own yet: steps 1 to 3, the halt, steps 4 to 6, then the switch.
fn fresh<'c>(
    ctx: &'c Ctx,
    order: &Forced,
    held_halt: u64,
    package: &Package,
    guard: Option<ServiceGuard<'c>>,
) -> Reply {
    let service = &order.package.service;
    let target = match obtain(
        ctx,
        service,
        &order.package.version,
        (
            &order.package.digest,
            order.package.index_digest.as_ref(),
            order.package.entry.as_ref(),
        ),
    ) {
        Ok(target) => target,
        Err(failed) => return failed,
    };
    let lifted = match order.halt_generation > held_halt {
        true => lift_halt(ctx, service, order.halt_generation),
        false => Ok(()),
    };
    if let Err(refused) = lifted {
        return refused;
    }
    let guard = match locked(ctx, service, package, guard) {
        Ok(guard) => guard,
        Err(reply) => return reply,
    };
    let (rec, force) = match begin(ctx, order, &target) {
        Ok(begun) => begun,
        Err(refused) => return refused,
    };
    switch::run(ctx, &guard, rec, force, &target.manifest)
}

/// The force under way that `journal` holds, to the version and digest of `package`, which the
/// record names: steps 7 to 16 again, its target read from its version directory, or fetched
/// again under the checks of steps 1 to 3 when that directory is gone or no longer whole.
fn resume_with(
    ctx: &Ctx,
    guard: &ServiceGuard<'_>,
    service: &ServiceName,
    journal: (Record, ForceRecord),
    (package, index_digest, entry): (&Package, Option<&PackageDigest>, Option<&Entry>),
) -> Reply {
    let (rec, force) = journal;
    match obtain(
        ctx,
        service,
        &package.version,
        (&package.digest, index_digest, entry),
    ) {
        Ok(target) => switch::run(ctx, guard, rec, force, &target.manifest),
        Err(reply) => reply,
    }
}

/// Steps 1 to 3: the package, its manifest and the check of its paths. A version already on the
/// host whole, under a seal of the order's digest, is used as it is; any other is asked of the
/// package port, which fetches, verifies, unpacks and seals it. Nothing on the host changes but
/// what the fetch itself stages.
fn obtain(
    ctx: &Ctx,
    service: &ServiceName,
    version: &Version,
    (digest, index_digest, entry): (&PackageDigest, Option<&PackageDigest>, Option<&Entry>),
) -> Result<Target, Reply> {
    let (dir, tree_digest) = match on_disk(ctx, service, version, digest) {
        Some(found) => found,
        None => {
            let dir = ctx
                .packages
                .version_dir(service, version, digest, index_digest, entry)
                .map_err(|error| {
                    let refusal = checks::package_refusal(&error);
                    failed_before(package_class(&error), refusal.token())
                })?;
            (dir.dir().to_path_buf(), dir.staged().digest().to_owned())
        }
    };
    let manifest = read_manifest(ctx, service, version, &dir)?;
    let placer = ctx.placer();
    placer
        .desired(&manifest)
        .and_then(|claims| placer.check(&claims))
        .map_err(|error| failed_before(error.class(), error.reason()))?;
    Ok(Target {
        manifest,
        tree_digest,
    })
}

/// The version directory of `service` at `version`, and its tree digest, when the host holds it
/// whole: its tree has the digest its seal holds ([`Layout::check_tree`]), and the seal names
/// the package digest `digest`.
fn on_disk(
    ctx: &Ctx,
    service: &ServiceName,
    version: &Version,
    digest: &PackageDigest,
) -> Option<(PathBuf, String)> {
    let layout = Layout::package(&ctx.dirs.packages.join(service.as_str()));
    layout.check_tree(version.as_str()).ok()?;
    let seal = read_seal(&layout.seal_path(version.as_str())).ok()?;
    let seal = Seal::parse(&seal).ok()?;
    (seal.package_digest() == digest.as_str()).then(|| {
        (
            layout.version_path(version.as_str()),
            seal.tree_digest().to_owned(),
        )
    })
}

/// The manifest of the version directory `dir`, read for `service` at `version` and this
/// daemon's version.
fn read_manifest(
    ctx: &Ctx,
    service: &ServiceName,
    version: &Version,
    dir: &Path,
) -> Result<Manifest, Reply> {
    let expected = Expected {
        name: service.clone(),
        version: version.clone(),
        platform: hostkeeper_config::platform::Platform::current(),
    };
    Manifest::load(dir, &expected, &ctx.own_version)
        .map_err(|error| failed_before(error.class(), Refusal::from(&error).token()))
}

/// The verdict of a force that failed before anything on the host changed.
fn failed_before(class: ManifestClass, reason: &str) -> Reply {
    Reply::Finished {
        verdict: Verdict::Failed {
            phase: Phase::BeforeCommit,
            class: match class {
                ManifestClass::Release => Class::Release,
                ManifestClass::Environment => Class::Environment,
            },
        },
        reason: Some(reason.to_owned()),
        resent: false,
    }
}

/// Whom a package error is blamed on: the class it carries ([`PackageError::class`]).
fn package_class(error: &PackageError) -> ManifestClass {
    match error.class() {
        Class::Release => ManifestClass::Release,
        Class::Environment => ManifestClass::Environment,
    }
}

/// Lifts the standing halt of `service`: its held generation raised to `generation`, and a
/// `Halt` record cleared, as `clear_halt` does.
fn lift_halt(ctx: &Ctx, service: &ServiceName, generation: u64) -> Result<(), Reply> {
    ctx.halts
        .raise(service, generation)
        .map_err(|error| Reply::refused(Refusal::StoreFailed, error))?;
    ctx.runner(service)
        .clear_halt()
        .map_err(|error| Reply::refused(Refusal::StoreFailed, error))?;
    Ok(())
}

/// Steps 4 to 6, and the service's lock: a watcher of the service is stopped; then, until the
/// lock is had, an unfinished hooked update is flagged and its running unit stopped, and the
/// force waits on the daemon's clock for the update's runner to yield the lock. `guard` is the
/// lock when the force already holds it.
fn locked<'c>(
    ctx: &'c Ctx,
    service: &ServiceName,
    package: &Package,
    mut guard: Option<ServiceGuard<'c>>,
) -> Result<ServiceGuard<'c>, Reply> {
    drop(ctx.watchers.stop(service));
    let holder = Holder::Package(package.clone());
    loop {
        stop_running(ctx, service)?;
        if let Some(guard) = guard.take() {
            return Ok(guard);
        }
        match ctx.locks.take_over(service, package.clone()) {
            Ok(guard) => return Ok(guard),
            Err(held) if held.is_force() => {
                return Err(while_held(service, &held, &holder));
            }
            Err(_) => ctx.clock.sleep_secs(FORCE_POLL_SECS),
        }
    }
}

/// Flags an unfinished hooked update of `service` `forced`, and stops the unit of its running
/// step by name. It is called at every poll while the force waits, so a unit the runner started
/// just after a stop of its name is stopped at the next poll. A stop that fails is tried once
/// more; when that one fails too, the force ends `Failed`, `unstopped`, with an alert.
fn stop_running(ctx: &Ctx, service: &ServiceName) -> Result<(), Reply> {
    let marked = ctx
        .store(service)
        .mark_forced()
        .map_err(|error| Reply::refused(Refusal::StoreFailed, error))?;
    let running = marked.filter(|rec| matches!(rec.state, State::Running { .. }));
    let Some(rec) = running else {
        return Ok(());
    };
    let unit = rec.unit(rec.step);
    ctx.launcher
        .stop(&unit)
        .or_else(|_| ctx.launcher.stop(&unit))
        .map_err(|error| {
            ctx.alert(
                ALERT_UNSTOPPABLE,
                &format!("service={service}: a force could not stop {unit}: {error}"),
            );
            Reply::Finished {
                verdict: Verdict::Failed {
                    phase: Phase::BeforeCommit,
                    class: Class::Environment,
                },
                reason: Some(Refusal::Unstopped.token().to_owned()),
                resent: false,
            }
        })?;
    Ok(())
}

/// Step 8's record, before it is written: a hooked update the record still holds unfinished
/// (its runner gone, or the daemon stopped mid-step) is closed `forced_over` first, as its
/// runner would have closed it, and its params file removed.
fn begin(ctx: &Ctx, order: &Forced, target: &Target) -> Result<(Record, ForceRecord), Reply> {
    let service = &order.package.service;
    let store = ctx.store(service);
    let last = last_record(ctx, service)?;
    let generation = last.as_ref().map_or(0, |rec| rec.generation);
    let left =
        last.filter(|rec| rec.force.is_none() & !matches!(rec.state, State::Finished { .. }));
    if let Some(mut rec) = left {
        rec.close_forced_over();
        store.save(&rec).map_err(store_failed)?;
        store.remove_params().map_err(store_failed)?;
    }
    files::make_private(&ctx.service_dir(service)).map_err(store_failed)?;
    let from = installed::linked_version(&ctx.dirs.packages, service, CURRENT);
    let force = ForceRecord {
        target_version: order.package.version.to_string(),
        target_digest: order.package.digest.to_string(),
        from_version: from.map(|version| version.to_string()),
        phase: ForcePhase::Started,
        order_try: order.package.order_try.get(),
        halt_generation: order.halt_generation,
        order_id: order.package.order_id.clone(),
        verdict: None,
    };
    let rec = Record {
        schema: SCHEMA,
        service: service.to_string(),
        target_version: order.package.version.to_string(),
        artifact_digest: target.tree_digest.clone(),
        tarball_digest: order.package.digest.to_string(),
        previous_digest: None,
        generation,
        params: store.params_path(),
        step: Step::Prepare,
        attempt: 1,
        state: State::Pending,
        hook: HookSel::New,
        progress: Progress::default(),
        order_try: Some(order.package.order_try.get()),
        forced: false,
        force: None,
    };
    Ok((rec, force))
}

/// The refusal of a force whose own files cannot be written.
fn store_failed(error: impl Display) -> Reply {
    Reply::refused(Refusal::StoreFailed, error)
}
