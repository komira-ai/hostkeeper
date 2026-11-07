//! The update of hostkeeper itself (design section 6.6): an `update` or an `install` of the
//! service `hostkeeper` followed by the self-update follow-on, recorded `Succeeded` only once the
//! new binary has run.
//!
//! [`run`] is the executor's path for an `update` and an `install` of the service `hostkeeper`;
//! every other service's orders are untouched. An `install` is what a daemon the deb or the pkg
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
//! 4. the step record of every other service: while any holds an unfinished update (`pending`,
//!    `running`, or `done` between two steps), or cannot be read, the order is `busy`, and
//!    nothing is written;
//! 5. the checks of every `update` (or `install`: none of hostkeeper is installed yet), and the
//!    package;
//! 6. the step lock, waited for (a running step of another service is never interrupted), and
//!    the records of step 4 read again under it;
//! 7. the protocol of the new binary ([`ProtocolProbe`]), asked of the file of the verified release
//!    before anything is staged, unless the release is the version that runs, whose binary is never
//!    staged and so is not asked (an install of the version a pkg or a hand upgrade put on the host
//!    goes on to its steps without running the probe): a protocol this daemon cannot fall back from
//!    is `Failed` before the commit, of class release (`protocol_unsupported`), and a probe that
//!    cannot be read is `Failed` before the commit, of class environment (`protocol_unreadable`); a
//!    release without the binary is `Failed` before the commit, of class release
//!    (`self_binary_missing`), and a release that changed since it was verified, or cannot be read
//!    now, is refused (`release_unreadable`). Nothing is written for any of them;
//! 8. the hook's steps through the step runner, which holds the success back
//!    ([`hostkeeper_steps::StepRunner::with_held_success`]);
//! 9. the follow-on ([`hostkeeper_steps::follow_on`]): the new binary staged in the binary
//!    directory ([`binary_dir`]), the record written as waiting for the restart, `current`
//!    switched. The reply is `Deferred` `awaiting_restart`, never `Succeeded`, and the daemon is
//!    asked to exit with status 0 ([`Restart`]). Its unit (`packaging/linux/hostkeeper.service`)
//!    starts `current` of the binary directory and has `Restart=always`, so that clean exit
//!    starts the new binary; the drop-in and fallback unit the daemon writes at its start
//!    (`crate::selfunits`) put the previous one back if the new one fails its start limit. The
//!    step lock is released only after the switch, so a step of another service that waits for
//!    it starts after the swap.
//!
//! At start, after the lock of the state directory and before the boot pass,
//! [`Daemon::settle_self_update`] settles an update that waits for the restart
//! ([`hostkeeper_steps::selfupdate::settle`]): the new binary confirms it `Succeeded` and clears
//! the deferrals an old updater caused in every service's record (`updater_old`,
//! `daemon_too_old`); any other binary records it `Failed` and its version bad. The binary that
//! ran the order exited before its `Deferred` reached a heartbeat, and the coordinator ends the
//! order once the host runs its target, so it never orders it again for a resend to be
//! answered: the wait keeps the order's id, the settlement (by this start, or by the fallback
//! verb before the previous binary starts) keeps it in the finished record, and every start
//! of a daemon lists the record's verdict under that id and the record's try for the heartbeat
//! to send until the coordinator acknowledges it (`crate::link_adapters::recorded`). A wait
//! written by a daemon from before the id was kept names no order, and its verdict is reported
//! by no binary.
//!
//! The protocol rule: a new binary of protocol `P` is accepted when
//! `SELF_UPDATE_PROTOCOL <= P <= SELF_UPDATE_PROTOCOL + 1`. "Can fall back from" means that the
//! previous binary reads every file the new one writes, which holds when a release of protocol
//! `N + 1` adds readers and writes only formats protocol `N` reads, and only the release after it
//! writes the formats of `N + 1`.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use hostkeeper_config::names::ServiceName;
use hostkeeper_hook::{Class, Phase, Verdict};
use hostkeeper_steps::record::{
    AwaitingRestart, Loaded, REASON_SELF_UPDATE_FALLBACK, RecordError, State,
};
use hostkeeper_steps::selfupdate::settle_with;
use hostkeeper_steps::{
    ArtifactError, BadVersions, FollowOn, LaunchError, Layout, ProbeCall, ProbeExit,
    SelfUpdateError, Settled, StepLauncher, StepRunner, follow_on,
};
use tokio::sync::Notify;

use crate::executor::{Ctx, Daemon, Reply};
use crate::files;
use crate::installed;
use crate::locks::ServiceGuard;
use crate::refusal::Refusal;
use crate::verb::{Hooked, HookedVerb};
use crate::verbs::force::SELF_SERVICE;
use crate::verbs::update;

/// The self-update protocol of this binary: what the verb `self-update-protocol` prints, and
/// what a new binary's protocol is compared with ([`accepted`]). It is apart from the step
/// runner's `UPDATER_PROTOCOL`, which an order's `min_updater_protocol` is compared with.
pub const SELF_UPDATE_PROTOCOL: u32 = 1;
/// The verb a new binary is asked for its protocol with.
pub const PROTOCOL_VERB: &str = "self-update-protocol";
/// The transient unit the probe runs in.
pub const PROBE_UNIT: &str = "hostkeeper-self-update-protocol";
/// The bound of the probe, in seconds (`RuntimeMaxSec=`).
pub const PROBE_TIMEOUT_SECS: u64 = 10;
/// The most bytes of the probe's output that are read.
pub const PROBE_OUTPUT_BYTES: usize = 64;
/// The hostkeeper binary in a release of the service `hostkeeper`.
pub const RELEASE_BINARY: &str = "bin/hostkeeper";
/// The binary directory's name in the state directory of the service `hostkeeper`.
pub const BINARY_DIR: &str = "bin";
/// The reason of the `Deferred` of an update whose new binary has not run yet.
pub const REASON_AWAITING_RESTART: &str = "awaiting_restart";
/// The reason of the `Failed` of a new binary whose protocol could not be read.
pub const REASON_PROTOCOL_UNREADABLE: &str = "protocol_unreadable";
/// The reason of the `Failed` of a new binary whose protocol this daemon cannot fall back from.
pub const REASON_PROTOCOL_UNSUPPORTED: &str = "protocol_unsupported";
/// The reason of the `Failed` of a release that has no hostkeeper binary to stage.
pub const REASON_SELF_BINARY_MISSING: &str = "self_binary_missing";
/// The alert of an update that waits for the restart and could not be settled at start, or of a
/// deferral a confirmed self-update could not clear.
pub const ALERT_SELF_UPDATE: &str = "self_update_unsettled";

/// What the verb `self-update-protocol` prints: [`SELF_UPDATE_PROTOCOL`] and a newline.
pub fn protocol_line() -> String {
    format!("{SELF_UPDATE_PROTOCOL}\n")
}

/// Whether a new binary of protocol `protocol` is one this daemon can fall back from:
/// [`SELF_UPDATE_PROTOCOL`] or the one after it.
pub fn accepted(protocol: u32) -> bool {
    (SELF_UPDATE_PROTOCOL..=SELF_UPDATE_PROTOCOL.saturating_add(1)).contains(&protocol)
}

/// The binary directory of hostkeeper's own update under the state directory `state_dir`:
/// `<state-dir>/hostkeeper/bin`, with `versions/<version>`, `current` and `previous`
/// ([`Layout::new`]). The fallback unit's `--dir` is this directory, and its parent is the state
/// directory of the service `hostkeeper`, where the step record and the list of bad versions are.
pub fn binary_dir(state_dir: &Path) -> PathBuf {
    state_dir.join(SELF_SERVICE).join(BINARY_DIR)
}

/// Asks a binary for its self-update protocol.
pub trait ProtocolProbe: Send + Sync {
    /// The protocol of `binary`, a file of a verified release.
    fn protocol(&self, binary: &Path) -> Result<u32, ProbeError>;
}

/// Why a probe gave no protocol.
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("the probe could not run: {0}")]
    Launch(#[from] LaunchError),
    /// It ended with another status than 0, or was killed (`None`): by its bound, or a signal.
    #[error("the probe ended with the status {0:?}")]
    Exit(Option<i32>),
    /// It printed something else than one line of decimal digits.
    #[error("the probe printed {0:?}, which is not one line of a decimal number")]
    Output(String),
}

/// The probe of the host: `<binary> self-update-protocol`, run through the step launcher in a
/// transient unit bounded by [`PROBE_TIMEOUT_SECS`], at most [`PROBE_OUTPUT_BYTES`] of its output
/// read.
pub struct LauncherProbe {
    launcher: Arc<dyn StepLauncher>,
}

impl LauncherProbe {
    pub fn new(launcher: Arc<dyn StepLauncher>) -> LauncherProbe {
        LauncherProbe { launcher }
    }
}

impl ProtocolProbe for LauncherProbe {
    fn protocol(&self, binary: &Path) -> Result<u32, ProbeError> {
        let exit = self.launcher.run_probe(&ProbeCall {
            service: SELF_SERVICE,
            executable: binary,
            args: &[PROTOCOL_VERB],
            unit: PROBE_UNIT,
            timeout_secs: PROBE_TIMEOUT_SECS,
            output_bytes: PROBE_OUTPUT_BYTES,
        })?;
        read_protocol(&exit)
    }
}

/// The protocol a probe printed: it exited 0, and its whole output is one line of decimal digits,
/// the newline at its end optional, that is a `u32`.
pub fn read_protocol(exit: &ProbeExit) -> Result<u32, ProbeError> {
    if exit.code != Some(0) {
        return Err(ProbeError::Exit(exit.code));
    }
    let text = String::from_utf8_lossy(&exit.output);
    let line = text.strip_suffix('\n').unwrap_or(&text);
    let digits = !exit.truncated & !line.is_empty() & line.bytes().all(|b| b.is_ascii_digit());
    let printed = || ProbeError::Output(text.to_string());
    match digits {
        true => line.parse().map_err(|_| printed()),
        false => Err(printed()),
    }
}

/// The daemon's request to exit with status 0 so that its unit starts the new binary: asked once
/// the follow-on switched `current`; the admin socket's server waits for it ([`Restart::wait`]),
/// answers what is in flight and returns.
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
    if let Some(answer) = update::answer(verb, order, last.as_deref())? {
        return Ok(answer);
    }
    others_idle(ctx, service)?;
    let prepared = update::prepare(ctx, verb, order, last.as_deref())?;
    files::make_private(&ctx.service_dir(service))
        .map_err(|e| Reply::refused(Refusal::StoreFailed, e))?;
    let runner = ctx.runner(service);
    let _step = guard.step(&ctx.step_lock);
    others_idle(ctx, service)?;
    let release = prepared.dir.staged();
    let binary = match release.check(Path::new(RELEASE_BINARY)) {
        Ok(binary) => binary,
        Err(ArtifactError::BadHookPath(_) | ArtifactError::HookMissing(_)) => {
            return Ok(failed(Class::Release, REASON_SELF_BINARY_MISSING));
        }
        // The tree cannot be read now, or is not the one verified: nothing runs from it.
        Err(error) => return Err(Reply::refused(Refusal::ReleaseUnreadable, error)),
    };
    let running = ctx.own_version.to_string();
    if let Some(refused) = probed(ctx.probe(), &binary, &running, version) {
        return Ok(refused);
    }
    let finished = match runner.run(&prepared.update) {
        Ok(finished) => finished,
        Err(error) => return Ok(update::interrupted(error)),
    };
    let layout = Layout::new(&binary_dir(&ctx.dirs.state));
    files::make_private(layout.dir()).map_err(|e| Reply::refused(Refusal::StoreFailed, e))?;
    let store = ctx.store(service);
    let binary = Path::new(RELEASE_BINARY);
    let order_id = order.order_id.as_deref();
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

/// Step 7: the refusal of the new binary `binary` of `version` by its protocol, asked of `probe`;
/// `None` when it is accepted. A release of the version that runs (`running`) is not asked: the
/// follow-on stages no binary for it ([`FollowOn::NotNeeded`]), so nothing of it is ever run, and
/// the order goes on as the module says (an install of it is `Succeeded` once its steps ran).
fn probed(probe: &dyn ProtocolProbe, binary: &Path, running: &str, version: &str) -> Option<Reply> {
    if running == version {
        return None;
    }
    match probe.protocol(binary) {
        Ok(protocol) if accepted(protocol) => None,
        Ok(_) => Some(failed(Class::Release, REASON_PROTOCOL_UNSUPPORTED)),
        Err(_) => Some(failed(Class::Environment, REASON_PROTOCOL_UNREADABLE)),
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
/// when there is none. `Err` when the records cannot be listed. The one rule of both the
/// self-update order ([`run`]) and the fallback verb on macOS (`crate::fallback`).
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
        settle_with(&layout, &ctx.store(service), &running, &mut clear)
    }
}
