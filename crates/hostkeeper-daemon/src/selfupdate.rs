//! The update of hostkeeper itself (design section 6.6): an `update`, an `install` or a
//! `force_rollback` of the service `hostkeeper` followed by the self-update follow-on, recorded
//! `Succeeded` only once the new binary has run.
//!
//! [`run`] is the executor's path for an `update`, an `install` and a `force_rollback` of the
//! service `hostkeeper` (a `force_rollback` is a hand-over to the version it names, lower ones
//! included: docs/design/packages-native.md, sections 10.4 and 10.6); every other service's
//! orders are untouched. An `install` is what a daemon the deb or the pkg
//! installed is ordered first: it runs, but no package of it is installed, so an `update` of it
//! would be refused `not_installed`. The install places the package and, when the release is not
//! the binary that runs, swaps the binary as an update does; when it is, the install is
//! `Succeeded` at once. Under the service's lock, in this order:
//!
//! 1. the held halt; the step record; the local list of bad versions
//!    (`<state-dir>/hostkeeper/bad-versions`, [`hostkeeper_steps::bad`]): an order for a version
//!    on it runs nothing and is answered `Failed` after the commit, of class release,
//!    `self_update_fallback`, as the record of its fallback said, whatever order replaced that
//!    record since. A list that cannot be read refuses the order (`bad_versions_unreadable`);
//! 2. an update that waits for the restart answers a resend of its own order `Deferred`
//!    `awaiting_restart`, and any other order `busy`;
//! 3. the try rules, as for every `update` and `install`;
//!    While the record waits for the `activate` of hostkeeper's own package (below), an order for
//!    that version runs the `activate` again and is answered with how it went, and any other is
//!    `busy`;
//! 4. the step record of every other service: while any holds an unfinished update (`pending`,
//!    `running`, or `done` between two steps), or cannot be read, the order is `busy`, and
//!    nothing is written; then a release older than this daemon below an installer of what the
//!    host holds is `Failed` before the commit, of class release (`rollback_below_installer`,
//!    [`crate::rollback_floor`]), with nothing staged;
//! 5. the checks of every `update` (or `install`: none of hostkeeper is installed yet), and the
//!    package;
//! 6. the step lock, waited for (a running step of another service is never interrupted), and
//!    the records of step 4 read again under it;
//! 7. the release judged ([`judge`]) before anything is staged: a release older than this
//!    daemon's [`OLDEST_READER`] (or whose version is not `major.minor.patch`) is `Failed` before
//!    the commit, of class release (`protocol_unsupported`), since it could not read this
//!    daemon's files, and any other is accepted, as is the version that runs, whose binary is
//!    never staged (an install of the version a pkg or a hand upgrade put on the host goes on to
//!    its steps). Nothing is asked of the new binary: a daemon older than the release's floor,
//!    its manifest's `min_hostkeeper`, never gets here, since the manifest's loader deferred the
//!    order `daemon_too_old` at step 5. A release without the binary is `Failed` before the
//!    commit, of class release (`self_binary_missing`), and a release that changed since it was
//!    verified, or cannot be read now, is refused (`release_unreadable`). Nothing is written for
//!    any of them;
//! 8. the hook's steps through the step runner, which holds the success back
//!    ([`hostkeeper_steps::StepRunner::with_held_success`]). For a release whose floor is above
//!    [`LEGACY_FLOOR`] the steps are built in ([`builtin_steps`]): each succeeds at once and no
//!    command of the release runs, and the record is the one a hook that does nothing gives.
//!    Below it the command runs;
//! 9. the follow-on ([`hostkeeper_steps::follow_on`]), the hand-over (docs/design/two-programs.md,
//!    4.3): the new binary ([`release_binary`]: `bin/hostkeeper` of an archive version,
//!    `tree/usr/bin/hostkeeper` of a `.deb`, `tree/usr/local/bin/hostkeeper` of a `.pkg`)
//!    staged in the binary directory ([`binary_dir`]), the link `next`
//!    pointed at it, the record written as waiting for the restart into it with a start count of
//!    0. `current` is not moved. The reply is `Deferred` `awaiting_restart`, never `Succeeded`,
//!    and the daemon is asked to exit with status 0 ([`Restart`]). Its unit
//!    (`packaging/linux/units/hostkeeper.service`) starts `current` of the binary directory and
//!    has `Restart=always`, so that clean exit starts the old binary again, which execs `next`
//!    while its start count lasts ([`crate::handover`]); three starts of the new binary that do
//!    not settle the update make the old one settle it `Failed` and run. The step lock is
//!    released only after the hand-over, so a step of another service that waits for it starts
//!    after it.
//!
//! **Through a native installer** (docs/design/packages-native.md, section 10.4): an update to a
//! native version, or on a host where hostkeeper runs from its native package, switches nothing
//! before the new binary has run ([`crate::own_package`]): the step runner's switch and the
//! built-in steps do nothing for it. The new binary, once it confirmed itself at its start,
//! installs its own package ([`Daemon::activate_own_package`]): its confirmation leaves the
//! `activate` pending in the record ([`hostkeeper_steps::settle_activating`]), and the verdict is
//! written, and reported, only after the `activate`. An update or an install of the version that
//! runs, which needs no restart, runs the `activate` at once, under the order.
//!
//! At start, after the lock of the state directory and before the boot pass,
//! [`Daemon::settle_self_update`] settles an update that waits for the restart
//! ([`hostkeeper_steps::selfupdate::settle`]): the new binary confirms it `Succeeded` and clears
//! the deferrals an old updater caused in every service's record (`updater_old`,
//! `daemon_too_old`); any other binary records it `Failed` and its version bad. The binary that
//! ran the order exited before its `Deferred` reached a heartbeat, and the coordinator ends the
//! order once the host runs its target, so it never orders it again for a resend to be
//! answered: the wait keeps the order's id, the settlement (by this start) keeps
//! it in the finished record, and every start of a daemon lists the record's verdict under that
//! id and the record's try for the heartbeat to send until the coordinator acknowledges it
//! (`crate::link_adapters::recorded`). A wait written by a daemon from before the id was kept
//! names no order, and its verdict is reported by no binary.
//!
//! "Can fall back from" means that the binary that runs reads every file the new one writes. The
//! floor carries that rule: a release that writes a new format sets [`OLDEST_READER`] to the
//! release that first read it, and the release builder writes that as the floor of hostkeeper's
//! package.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_config::names::ServiceName;
use hostkeeper_hook::{Class, Phase, Verdict};
use hostkeeper_proto::hostkeeper::release::v1::Target;
use hostkeeper_release::Installer;
use hostkeeper_steps::record::{
    AwaitingRestart, Loaded, PendingActivate, REASON_SELF_UPDATE_FALLBACK, RecordError, State,
};
use hostkeeper_steps::selfupdate::settle_activating;
use hostkeeper_steps::{
    ArtifactError, BadVersions, FollowOn, Layout, OLDEST_READER, SelfUpdateError, Settled,
    StepRunner, follow_on,
};
use tokio::sync::Notify;

use crate::executor::{Ctx, Daemon, Reply};
use crate::files;
use crate::installed;
use crate::locks::{Holder, ServiceGuard};
use crate::own_package::{self, DEFER_SECS, OwnActivate, REASON_ACTIVATE_PENDING};
use crate::refusal::Refusal;
use crate::rollback_floor::{self, REASON_ROLLBACK_BELOW_INSTALLER};
use crate::target;
use crate::verb::{Hooked, HookedVerb};
use crate::verbs::force::SELF_SERVICE;
use crate::verbs::update;

/// The hostkeeper binary in a release of the service `hostkeeper` that is a
/// `hostkeeper-archive` version.
pub const RELEASE_BINARY: &str = "bin/hostkeeper";
/// The hostkeeper binary in a version of the service `hostkeeper` staged from its `.deb`
/// (`apt`): the package's `/usr/bin/hostkeeper` in the version's tree
/// (docs/design/packages-native.md, section 10.4, step 1).
pub const APT_RELEASE_BINARY: &str = "tree/usr/bin/hostkeeper";
/// The hostkeeper binary in a version of the service `hostkeeper` staged from its `.pkg`
/// (`macos-pkg`): the package's `/usr/local/bin/hostkeeper` in the version's tree.
pub const PKG_RELEASE_BINARY: &str = "tree/usr/local/bin/hostkeeper";
/// The binary directory's name in the state directory of the service `hostkeeper`.
pub const BINARY_DIR: &str = "bin";
/// The reason of the `Deferred` of an update whose new binary has not run yet.
pub const REASON_AWAITING_RESTART: &str = "awaiting_restart";
/// The reason of the `Failed` of a release this daemon cannot fall back to, or that could not
/// read this daemon's files: one older than [`OLDEST_READER`].
pub const REASON_PROTOCOL_UNSUPPORTED: &str = "protocol_unsupported";
/// The reason of the `Failed` of a release that has no hostkeeper binary to stage.
pub const REASON_SELF_BINARY_MISSING: &str = "self_binary_missing";
/// The alert of an update that waits for the restart and could not be settled at start, or of a
/// deferral a confirmed self-update could not clear.
pub const ALERT_SELF_UPDATE: &str = "self_update_unsettled";

/// The floor every release of hostkeeper before stage 2 of docs/design/two-programs.md declares
/// (`min_hostkeeper` of its package): a release whose floor is above this runs built-in steps
/// ([`builtin_steps`]); one whose floor is this or lower runs its hook's command.
pub const LEGACY_FLOOR: HostkeeperVersion = HostkeeperVersion {
    major: 0,
    minor: 1,
    patch: 53,
};

/// How step 7 judges a release of hostkeeper ([`judge`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Judgement {
    /// It goes on to its steps.
    Accepted,
    /// It is `Failed` before the commit, of class release ([`REASON_PROTOCOL_UNSUPPORTED`]).
    Unsupported,
}

/// How a daemon of version `running` judges the release `version` of hostkeeper (step 7 of the
/// module): the version that runs is [`Accepted`] with no check; a release older than
/// [`OLDEST_READER`], or whose version is not `major.minor.patch`, is [`Unsupported`] (the
/// backward rule, since it could not read this daemon's files), and any other is [`Accepted`].
/// The forward rule, a floor above `running`, is the manifest loader's, before this.
///
/// [`Accepted`]: Judgement::Accepted
/// [`Unsupported`]: Judgement::Unsupported
pub fn judge(running: HostkeeperVersion, version: &str) -> Judgement {
    match HostkeeperVersion::parse(version) {
        Ok(target) if (target == running) | (target >= OLDEST_READER) => Judgement::Accepted,
        _ => Judgement::Unsupported,
    }
}

/// The hostkeeper binary of a version of the service `hostkeeper` staged as `staged` (its
/// `target.json`, `None` for a `hostkeeper-archive` version), relative to the version's
/// directory: [`RELEASE_BINARY`], [`PKG_RELEASE_BINARY`] for a `macos-pkg` version, and
/// [`APT_RELEASE_BINARY`] for any other, which only `apt` stages.
pub fn release_binary(staged: Option<&Target>) -> &'static str {
    match staged.map(|entry| entry.installer.parse::<Installer>()) {
        None => RELEASE_BINARY,
        Some(Ok(Installer::MacosPkg)) => PKG_RELEASE_BINARY,
        Some(_) => APT_RELEASE_BINARY,
    }
}

/// Whether the update of `service` to a release whose floor is `floor` runs built-in steps
/// ([`hostkeeper_steps::UpdateOrder::builtin_steps`]): hostkeeper's own package whose floor is
/// above [`LEGACY_FLOOR`], whose hook's command no daemon runs (docs/design/two-programs.md,
/// section 4.4).
pub fn builtin_steps(service: &ServiceName, floor: HostkeeperVersion) -> bool {
    (service.as_str() == SELF_SERVICE) & (floor > LEGACY_FLOOR)
}

/// The binary directory of hostkeeper's own update under the state directory `state_dir`:
/// `<state-dir>/hostkeeper/bin`, with `versions/<version>`, `current` and `previous`
/// ([`Layout::new`]). Its parent is the state directory of the service `hostkeeper`, where the
/// step record and the list of bad versions are.
pub fn binary_dir(state_dir: &Path) -> PathBuf {
    state_dir.join(SELF_SERVICE).join(BINARY_DIR)
}

/// The daemon's request to exit with status 0 so that its unit starts the new binary: asked once
/// the follow-on switched `current`; [`crate::start::serve`] waits for it ([`Restart::wait`]),
/// tells the loops to stop and returns.
#[derive(Debug, Default)]
pub struct Restart {
    asked: AtomicBool,
    notify: Notify,
}

impl Restart {
    /// Asks for the exit.
    pub fn request(&self) {
        self.asked.store(true, Ordering::SeqCst);
        self.notify.notify_one();
    }

    /// Whether the exit was asked for.
    pub fn requested(&self) -> bool {
        self.asked.load(Ordering::SeqCst)
    }

    /// Returns once the exit was asked for.
    pub async fn wait(&self) {
        while !self.requested() {
            self.notify.notified().await;
        }
    }
}

/// The runner of `service` as the daemon makes it: for the service `hostkeeper`, the success of
/// an update is held back for the new binary to record ([`StepRunner::with_held_success`]).
pub(crate) fn runner_for(runner: StepRunner, service: &ServiceName) -> StepRunner {
    match service.as_str() == SELF_SERVICE {
        true => runner.with_held_success(),
        false => runner,
    }
}

/// The verdict a version that fell back is answered with: `Failed` after the commit, of class
/// release.
pub fn fallback_verdict() -> Verdict {
    Verdict::Failed {
        phase: Phase::AfterCommit,
        class: Class::Release,
    }
}

/// The reply of an update whose new binary has not run yet.
fn awaiting_restart(resent: bool) -> Reply {
    Reply::Finished {
        verdict: Verdict::Deferred {
            retry_after_secs: None,
        },
        reason: Some(REASON_AWAITING_RESTART.to_owned()),
        resent,
    }
}

/// The reply of a self-update refused before anything was staged or written.
fn failed(class: Class, reason: &str) -> Reply {
    Reply::Finished {
        verdict: Verdict::Failed {
            phase: Phase::BeforeCommit,
            class,
        },
        reason: Some(reason.to_owned()),
        resent: false,
    }
}

/// An `update` or an `install` (`verb`) of the service `hostkeeper`.
pub fn run(ctx: &Ctx, verb: HookedVerb, order: Hooked) -> Reply {
    update::locked(ctx, verb, &order, |guard| steps(ctx, verb, &order, guard))
}

fn steps(
    ctx: &Ctx,
    verb: HookedVerb,
    order: &Hooked,
    guard: &ServiceGuard<'_>,
) -> Result<Reply, Reply> {
    let service = &order.service;
    let version = order.version.as_str();
    ctx.halts
        .get(service)
        .map_err(|e| Reply::refused(Refusal::HaltUnreadable, e))?;
    let last = update::last_record(ctx, service)?;
    let bad = BadVersions::load(&ctx.service_dir(service))
        .map_err(|e| Reply::refused(Refusal::BadVersionsUnreadable, e))?;
    if bad.contains(version) {
        return Ok(Reply::Finished {
            verdict: fallback_verdict(),
            reason: Some(REASON_SELF_UPDATE_FALLBACK.to_owned()),
            resent: true,
        });
    }
    let awaiting = last
        .as_ref()
        .and_then(|rec| rec.progress.awaiting_restart.as_ref());
    if let Some(awaiting) = awaiting {
        return waiting_for(awaiting, version);
    }
    let activating = last.as_ref().and_then(|rec| rec.progress.activate.as_ref());
    if let Some(pending) = activating {
        return activate_waiting(ctx, guard, service, pending, version);
    }
    if let Some(answer) = update::answer(verb, order, last.as_deref())? {
        return Ok(answer);
    }
    others_idle(ctx, service)?;
    below_installer(ctx, version)?;
    let prepared = update::prepare(ctx, verb, order, last.as_deref())?;
    files::make_private(&ctx.service_dir(service))
        .map_err(|e| Reply::refused(Refusal::StoreFailed, e))?;
    let runner = ctx.runner(service);
    let _step = guard.step(&ctx.step_lock);
    others_idle(ctx, service)?;
    let release = prepared.dir.staged();
    let staged = target::read(prepared.dir.dir())
        .map_err(|e| Reply::refused(Refusal::ReleaseUnreadable, e))?;
    let binary = Path::new(release_binary(staged.as_ref()));
    match release.check(binary) {
        Ok(_) => {}
        Err(ArtifactError::BadHookPath(_) | ArtifactError::HookMissing(_)) => {
            return Ok(failed(Class::Release, REASON_SELF_BINARY_MISSING));
        }
        // The tree cannot be read now, or is not the one verified: nothing runs from it.
        Err(error) => return Err(Reply::refused(Refusal::ReleaseUnreadable, error)),
    }
    let running = ctx.own_version.to_string();
    if judge(ctx.own_version, version) == Judgement::Unsupported {
        return Ok(failed(Class::Release, REASON_PROTOCOL_UNSUPPORTED));
    }
    let finished = match runner.run(&prepared.update) {
        Ok(finished) => finished,
        Err(error) => return Ok(update::interrupted(error)),
    };
    let layout = Layout::new(&binary_dir(&ctx.dirs.state));
    files::make_private(layout.dir()).map_err(|e| Reply::refused(Refusal::StoreFailed, e))?;
    let store = ctx.store(service);
    let order_id = order.order_id.as_deref();
    let left = own_package::deferred_at(ctx, prepared.dir.dir())
        .map_err(|e| Reply::refused(Refusal::ReleaseUnreadable, e))?;
    if let (true, true, Verdict::Succeeded { promotion_pending }) =
        (running == version, left, &finished.verdict)
    {
        // The version that runs needs no restart: its package is activated now, the record
        // waiting for it first so that a crash in between is resumed by the next start.
        return match store.pend_activate(version, *promotion_pending, order_id) {
            Ok(true) => Ok(activated(ctx, service)),
            Ok(false) => Ok(update::ran(Ok(finished))),
            Err(error) => Err(Reply::refused(Refusal::StoreFailed, error)),
        };
    }
    match follow_on(
        &finished, &store, &layout, &running, version, release, binary, order_id,
    ) {
        Ok(FollowOn::Restart) => {
            ctx.restart().request();
            Ok(awaiting_restart(false))
        }
        Ok(FollowOn::NotNeeded) => Ok(update::ran(Ok(finished))),
        Err(error) => Ok(Reply::Interrupted {
            refusal: follow_refusal(&error),
            detail: error.to_string(),
        }),
    }
}

/// A rollback of hostkeeper below an installer of what the host holds
/// ([`rollback_floor::below`]): `Failed` before the commit, of class release,
/// [`REASON_ROLLBACK_BELOW_INSTALLER`], with nothing staged; `placed.json` that cannot be read
/// refuses the order (`store_failed`).
fn below_installer(ctx: &Ctx, version: &str) -> Result<(), Reply> {
    let record = ctx
        .placer()
        .store()
        .load()
        .map_err(|e| Reply::refused(Refusal::StoreFailed, e))?;
    match rollback_floor::below(&record, ctx.own_version, version) {
        None => Ok(()),
        Some(why) => {
            ctx.alert(ALERT_SELF_UPDATE, &format!("service={SELF_SERVICE}: {why}"));
            Err(failed(Class::Release, REASON_ROLLBACK_BELOW_INSTALLER))
        }
    }
}

/// The answer while the `activate` of hostkeeper's own package at `pending.version` waits: an
/// order for that version tries it again now, under the step lock, and is answered with how it
/// went; any other is `busy`.
fn activate_waiting(
    ctx: &Ctx,
    guard: &ServiceGuard<'_>,
    service: &ServiceName,
    pending: &PendingActivate,
    version: &str,
) -> Result<Reply, Reply> {
    if pending.version != version {
        return Err(Reply::refused(
            Refusal::Busy,
            format!(
                "{SELF_SERVICE} waits for the activate of its package {}",
                pending.version
            ),
        ));
    }
    let _step = guard.step(&ctx.step_lock);
    Ok(activated(ctx, service))
}

/// The `activate` of hostkeeper's own package the record waits for, run now, as the reply of
/// the order: its verdict, or `Deferred` [`REASON_ACTIVATE_PENDING`] while it is to be tried
/// again.
fn activated(ctx: &Ctx, service: &ServiceName) -> Reply {
    let deferred = Reply::Finished {
        verdict: Verdict::Deferred {
            retry_after_secs: Some(DEFER_SECS),
        },
        reason: Some(REASON_ACTIVATE_PENDING.to_owned()),
        resent: false,
    };
    let Some(OwnActivate::Ended(_)) = own_package::activate_pending(ctx, service) else {
        return deferred;
    };
    // The verdict as the record now holds it, the promotion it kept included.
    match ctx.store(service).load() {
        Ok(Loaded::Current(rec)) => match rec.state {
            State::Finished { verdict, reason } => Reply::Finished {
                verdict,
                reason,
                resent: false,
            },
            _ => deferred,
        },
        _ => deferred,
    }
}

/// The token of a follow-on that stopped: the release changed or cannot be read
/// (`package_failed`), the binary directory could not be changed (`link_failed`), and the record
/// or the list of bad versions could not be used (`store_failed`). A resend runs it again.
fn follow_refusal(error: &SelfUpdateError) -> Refusal {
    match error {
        SelfUpdateError::Artifact(_) => Refusal::PackageFailed,
        SelfUpdateError::Io(_) => Refusal::LinkFailed,
        _ => Refusal::StoreFailed,
    }
}

/// The answer while the update to `awaiting.version` waits for the restart: `Deferred`
/// `awaiting_restart` to its own order, `busy` to any other.
fn waiting_for(awaiting: &AwaitingRestart, version: &str) -> Result<Reply, Reply> {
    match awaiting.version == version {
        true => Ok(awaiting_restart(true)),
        false => Err(Reply::refused(
            Refusal::Busy,
            format!(
                "{SELF_SERVICE} waits for its restart into {}",
                awaiting.version
            ),
        )),
    }
}

/// `Ok` when no service but `own` has an unfinished update in its step record; `busy` naming the
/// first that has, or whose record cannot be read, or when the records cannot be listed.
fn others_idle(ctx: &Ctx, own: &ServiceName) -> Result<(), Reply> {
    let busy = |detail: String| Reply::refused(Refusal::Busy, detail);
    match unfinished_other(&ctx.dirs.state, own.as_str(), &|service| {
        ctx.store(service).load()
    }) {
        Ok(None) => Ok(()),
        Ok(Some(detail)) => Err(busy(detail)),
        Err(e) => Err(busy(format!("the step records cannot be listed: {e}"))),
    }
}

/// The first service of the state directory `state` but `own` whose step record, as `load`
/// reads it, holds an unfinished update (or cannot be read), in the words of the refusal; `None`
/// when there is none. `Err` when the records cannot be listed. The rule of the
/// self-update order ([`run`]).
pub(crate) fn unfinished_other(
    state: &Path,
    own: &str,
    load: &dyn Fn(&ServiceName) -> Result<Loaded, RecordError>,
) -> io::Result<Option<String>> {
    let names = installed::service_dirs(state)?;
    for service in names.values().filter(|service| service.as_str() != own) {
        let unfinished = match load(service) {
            Ok(Loaded::Absent) => None,
            Ok(Loaded::Current(rec)) => match rec.state {
                State::Finished { .. } => None,
                State::Pending => Some("pending"),
                State::Running { .. } => Some("running"),
                State::Done { .. } => Some("between two steps"),
            },
            Ok(Loaded::Newer { .. }) => Some("written by a newer daemon"),
            Err(_) => Some("unreadable"),
        };
        if let Some(why) = unfinished {
            return Ok(Some(format!(
                "the step record of {service} is {why}: {SELF_SERVICE} updates itself only while \
                 no other update is unfinished"
            )));
        }
    }
    Ok(None)
}

impl Daemon {
    /// At start, before the boot pass: settles the update of hostkeeper itself that waits for
    /// the restart, with this daemon's version as the binary that runs
    /// ([`hostkeeper_steps::selfupdate::settle`]). On a confirmation, every service's `Deferred`
    /// record of an old updater (`updater_old`, `daemon_too_old`) is removed first, so that the
    /// next order of it runs and `Status` no longer reports it; a record that cannot be cleared
    /// is an alert. [`Settled::NotPending`] when the service `hostkeeper` has no state directory.
    pub fn settle_self_update(&self) -> Result<Settled, SelfUpdateError> {
        let ctx = self.ctx();
        let services = installed::service_dirs(&ctx.dirs.state)?;
        let Some(service) = services.get(SELF_SERVICE) else {
            return Ok(Settled::NotPending);
        };
        let layout = Layout::new(&binary_dir(&ctx.dirs.state));
        let running = ctx.own_version.to_string();
        let mut clear = || -> Result<(), SelfUpdateError> {
            for service in services.values() {
                if let Err(error) = ctx.store(service).clear_updater_old() {
                    ctx.alert(
                        ALERT_SELF_UPDATE,
                        &format!("service={service}: a deferral was not cleared: {error}"),
                    );
                }
            }
            Ok(())
        };
        let activate = own_package::needs_activate(ctx, &running)?;
        settle_activating(&layout, &ctx.store(service), &running, &mut clear, activate)
    }

    /// The `activate` of hostkeeper's own package the step record waits for, run under the
    /// service's lock and the step lock ([`own_package::activate_pending`]): at start, after the
    /// settlement, and again every [`DEFER_SECS`] while it defers ([`crate::start`]). `None`
    /// when nothing waits; `Deferred` `busy`, with nothing run, while an order holds the
    /// service's lock (it runs the `activate` itself when it is for that version).
    pub fn activate_own_package(&self) -> Option<OwnActivate> {
        let ctx = self.ctx();
        let services = installed::service_dirs(&ctx.dirs.state).ok()?;
        let service = services.get(SELF_SERVICE)?;
        let guard = match ctx.locks.take_for(service, Holder::OwnActivate) {
            Ok(guard) => guard,
            Err(held) => {
                return Some(OwnActivate::Deferred {
                    reason: Refusal::Busy.token().to_owned(),
                    detail: format!("{service} is locked by {}", held.holder),
                });
            }
        };
        let _step = guard.step(&ctx.step_lock);
        own_package::activate_pending(ctx, service)
    }
}
