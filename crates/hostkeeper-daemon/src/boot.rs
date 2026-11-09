//! The daemon's start pass (design section 6.5): every step record read against the boot id, each
//! unfinished update run to its verdict, the post-success watchers restarted, the manifest's
//! `[hook] boot` step run from `current`, and then the readiness notice.
//!
//! The placed links are not part of [`Daemon::boot_pass`]: before it ([`crate::start::startup`])
//! the daemon sweeps the leftovers of
//! crashed package fetches and unpacks ([`crate::packages::Packages::remove_orphans`]) and
//! reconciles the placed paths ([`hostkeeper_placed::Placer::reconcile`]), each switch a step
//! record says was begun finished or undone by [`switch_intent`]. It reads the records without
//! writing any, and does not write the boot id.
//!
//! [`Daemon::boot_pass`] does, in this order:
//!
//! 1. list every service state directory, and read each step record. A record that is corrupt,
//!    or that names another service, (`corrupt`) or of a newer schema (`record_newer`) holds its
//!    service, and only `clear_record` clears it; one that cannot be read at all (`unreadable`)
//!    holds its service too;
//! 2. compare the boot id with the one the last pass wrote ([`boot_changed`]), once;
//! 3. rewrite every unfinished record for its cell of the resume table
//!    ([`hostkeeper_steps::resume`]), writing it before anything of it runs;
//! 4. only after every record was read and rewritten, write the boot id ([`commit_boot`]). A
//!    daemon killed before that reads every record as after a boot on its next start. A record
//!    that could not be read, or whose rewrite could not be written, keeps the old boot id for
//!    the same reason;
//! 5. once the boot id is written, each uninstall a tombstone in `placed.json` left unfinished,
//!    resumed under its service's lock without waiting for an order ([`crate::uninstall`]); its
//!    reply is the service's outcome ([`Outcome::Uninstall`] for a verdict);
//! 6. run each force whose record holds no verdict again from its top ([`verbs::force::resume`],
//!    under the service's lock only); then run each unfinished update to its verdict, through the
//!    step runner, under the service's lock and the step lock, the order rebuilt from the record
//!    and the sealed version directory. An update that reaches the post-success watcher is left
//!    to step 7;
//! 7. restart the watchers, each on a thread the daemon owns ([`crate::watchers`]).
//!
//! [`Daemon::after_boot`] then runs the `boot` step of each service whose `current` manifest
//! names one, and sends the readiness notice, once, whatever the pass found. A `current` that is
//! not a link to a version directory beside it fails that service's boot step
//! (`boot_step_failed`) and runs nothing.
//!
//! A record that cannot be written means no hook of that service runs: not the step, not
//! `rollback`, not the boot step; its update is reported `Deferred` with the reason
//! `journal_unavailable`. A boot id that cannot be written (or is kept, above) means no hook runs
//! at all. A step that cannot be stopped (`unstopped`) leaves its record `running` and keeps its
//! service locked until `abort` ends it ([`crate::verbs::abort`]), or for the life of the daemon
//! when it was the boot step, whose unit no step record names; the daemon serves on. Each of these
//! is logged at error level with an `alert=` field.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::num::NonZeroU64;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};

use hostkeeper_config::manifest::{Expected, Hook, Manifest};
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_hook::{Params, STAGED_RELEASE_KEY, Scalar, Step, TARGET_VERSION_KEY, Verdict};
use hostkeeper_placed::{ReconcileReport, Side, SwitchIntent};
use hostkeeper_steps::record::{Loaded, Record, RecordError, State};
use hostkeeper_steps::runner::REASON_ARTIFACT_CHANGED;
use hostkeeper_steps::{
    AdditiveCall, ArtifactError, Bounds, Clock, Finished, ForcePhase, LaunchError, Layout,
    ResumeAction, RunError, Seal, Staged, StepExit, UpdateOrder, read_seal, resume, resume_action,
};

use crate::executor::{Ctx, Daemon, Reply};
use crate::files::{self, WriteStep};
use crate::installed::{self, CURRENT};
use crate::locks::{Package, ServiceGuard};
use crate::notify::ReadyNotifier;
use crate::refusal::Refusal;
use crate::verb::{HookedVerb, PackageDigest};
use crate::verbs;

/// The file of the boot id the last start pass wrote, in the state directory beside the
/// services' directories (design section 10). It holds the id alone. Its name begins with `.`,
/// which no service name has, so it never takes the place of a service's directory.
pub const BOOT_ID_FILE: &str = ".boot-id";
/// The reason of the `Deferred` of an update whose step record cannot be written.
pub const REASON_JOURNAL_UNAVAILABLE: &str = "journal_unavailable";
/// The alert of a step the dead-man could not stop.
pub const ALERT_UNSTOPPABLE: &str = "unstoppable_unit";
/// The alert of a step record, or the boot id, that cannot be written.
pub const ALERT_JOURNAL: &str = "journal_unavailable";
/// The alert of a release that is not the one its seal or step record names: a halt.
pub const ALERT_ARTIFACT_CHANGED: &str = "artifact_changed";
/// The alert of a hooked order that stopped without a verdict, for any reason but a step that
/// could not be stopped ([`ALERT_UNSTOPPABLE`]): its service, its reason and what failed.
pub const ALERT_INTERRUPTED: &str = "order_interrupted";
/// The warning of a switch around a hooked step that failed and is tried again.
pub const WARN_SWITCH_RETRIED: &str = "switch_retried";
/// The alert of a post-success watcher that stopped without a verdict for another reason.
pub const ALERT_WATCHER: &str = "watcher_interrupted";
/// The alert of a `boot` step that failed or could not be run.
pub const ALERT_BOOT_STEP: &str = "boot_step_failed";
/// The alert of a readiness notice that could not be sent, a package directory that could not be
/// listed (for the sweep of leftovers or for the boot steps), a sweep that failed, or placed paths
/// that could not be reconciled.
pub const ALERT_START: &str = "start_incomplete";

/// The log line of an alert: error level, with its `alert=` field. The daemon logs it through
/// [`Ctx::alert`].
pub fn alert_line(alert: &str, detail: &str) -> String {
    format!("hostkeeper daemon: level=error alert={alert} {detail}")
}

/// The log line of a warning: a failure the daemon recovers from by itself, with its `alert=`
/// field, at warn level.
pub(crate) fn warn_line(alert: &str, detail: &str) -> String {
    format!("hostkeeper daemon: level=warn alert={alert} {detail}")
}

/// Whether the host may have booted since the last start pass wrote the boot id: the id in
/// [`BOOT_ID_FILE`] is not the clock's. A missing or unreadable file counts as a boot, and so
/// does a clock whose boot id cannot be read: an unknown boot is never the same boot.
pub fn boot_changed(state_dir: &Path, clock: &dyn Clock) -> bool {
    let before = fs::read_to_string(state_dir.join(BOOT_ID_FILE));
    match (before, clock.boot_id()) {
        (Ok(before), Ok(now)) => before.trim() != now.as_str(),
        (Err(_), _) | (_, Err(_)) => true,
    }
}

/// Writes the clock's boot id, once the start pass has read every record under it: temporary
/// file made new (never through a link left at its name), fsync, rename, directory fsync. A
/// boot id that cannot be read is an error and nothing is written: the old id stays, so the next
/// start too reads the records as after a boot.
pub fn commit_boot(state_dir: &Path, clock: &dyn Clock) -> io::Result<()> {
    commit_boot_stopping(state_dir, clock, &|_| false)
}

/// [`commit_boot`], stopped where `stop` says, as a crash would: the seam of the tests of a
/// daemon killed while it writes the boot id.
pub fn commit_boot_stopping(
    state_dir: &Path,
    clock: &dyn Clock,
    stop: &dyn Fn(WriteStep) -> bool,
) -> io::Result<()> {
    let id = clock.boot_id().map_err(io::Error::other)?;
    let tmp = state_dir.join(format!("{BOOT_ID_FILE}.tmp"));
    // A stale one goes; one that cannot go makes the create below fail.
    drop(fs::remove_file(&tmp));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)?;
    file.write_all(id.as_str().as_bytes())?;
    file.sync_all()?;
    files::crossed(stop, WriteStep::BeforeRename)?;
    fs::rename(&tmp, state_dir.join(BOOT_ID_FILE))?;
    File::open(state_dir)?.sync_all()?;
    files::crossed(stop, WriteStep::AfterRename)
}

/// A part of the start, in the order the start runs them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// A registration socket kbf's agent left is removed, before any unit is touched.
    UnlinkRegistrationSocket,
    /// The leftovers of crashed package fetches and unpacks are removed.
    BeforeRecords,
    /// The placed links are finished or undone, and compared with `current`.
    Reconcile,
    /// Every step record is read and rewritten for the resume table.
    ReadRecords,
    /// The boot id is written.
    WriteBootId,
    /// Tombstoned uninstalls are resumed.
    ResumeUninstalls,
    /// Each unfinished update runs to its verdict.
    RunUpdates,
    /// The post-success watchers restart.
    RestartWatchers,
    /// The `boot` step of each service runs.
    BootSteps,
    /// The readiness notice is sent.
    Ready,
}

/// What the start pass did with a service's update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// There was no step record, or its update had ended: nothing to resume.
    Idle,
    /// The record cannot be parsed: the service is held until `clear_record`.
    Corrupt(String),
    /// The record was written by a newer daemon: it is never changed, and holds the service.
    RecordNewer(u64),
    /// The record could not be read: the service is held, and the boot id is not written.
    Unreadable(String),
    /// The update could not be rebuilt from the record and its version directory: it is held.
    Unresumable(String),
    /// The record (or the boot id) could not be written: no hook of the service ran, and its
    /// update is `Deferred` with the reason `journal_unavailable`.
    JournalUnavailable(String),
    /// The update ran to this verdict.
    Finished(Finished),
    /// The uninstall a tombstone left unfinished was resumed and ran to this verdict. Its
    /// `boot` step does not run.
    Uninstall(Finished),
    /// The update is in the post-success watcher, which runs on a thread of the daemon.
    Watching,
    /// The runner stopped without a verdict; the record says where. After `unstopped` the
    /// service stays locked.
    Interrupted { refusal: Refusal, detail: String },
}

/// What became of a service's `boot` step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BootStep {
    /// Nothing to run: no `current`, or its manifest names no `boot` step.
    NotRun,
    /// Not run: the service's update is unfinished, its record is held or cannot be written, or
    /// the boot id could not be written.
    Held,
    /// It ran and exited 0.
    Succeeded,
    /// It failed, or could not be run: logged as an alert. Nothing is rolled back.
    Failed(String),
}

/// What the start did with one service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceBoot {
    /// The cell of the resume table its unfinished update was in.
    pub action: Option<ResumeAction>,
    pub outcome: Outcome,
    pub boot_step: BootStep,
}

impl ServiceBoot {
    /// A service with nothing to resume.
    pub fn idle() -> ServiceBoot {
        ServiceBoot::of(None, Outcome::Idle)
    }

    fn of(action: Option<ResumeAction>, outcome: Outcome) -> ServiceBoot {
        ServiceBoot {
            action,
            outcome,
            boot_step: BootStep::NotRun,
        }
    }

    /// The verdict the start reached for the service's update, if it reached one.
    pub fn verdict(&self) -> Option<Finished> {
        match &self.outcome {
            Outcome::Finished(finished) | Outcome::Uninstall(finished) => Some(finished.clone()),
            Outcome::JournalUnavailable(_) => Some(journal_unavailable()),
            _ => None,
        }
    }
}

/// The verdict of an update whose record cannot be written: `Deferred`, `journal_unavailable`
/// (the class is environment: the host, not the release, is at fault).
pub fn journal_unavailable() -> Finished {
    Finished {
        verdict: Verdict::Deferred {
            retry_after_secs: None,
        },
        reason: Some(REASON_JOURNAL_UNAVAILABLE.to_owned()),
    }
}

/// The switch of `current` the step record `rec` of `service` says was begun, for the
/// reconciliation of the placed paths to finish (`Forward`) or undo (`Back`); `None` when no
/// switch is begun, and `current` alone decides. `previous` is the version the update replaces,
/// from its params file (`None` for a first install); `rebooted` is whether the host booted since
/// the start pass last ended, as the resume table reads the record.
///
/// - A record that ended (a verdict) has none. A `prepare` has none: nothing was switched.
/// - A `rollback` is `Back`: the switch back comes before it. A `cutover`, `verify`, `cleanup` or
///   `uninstall` is `Forward`, as is a record in the post-success watcher, except where the
///   resume table rolls the update back after a reboot (nothing committed), which is `Back`. A
///   `cutover` counts whether it is pending or running, since the switch comes before it runs.
/// - A force that began on another version and passed the switch of `current`
///   ([`ForcePhase::Switched`]) is `Forward`; a force before it, or one that began on its target,
///   or one that has a verdict, has none, and a force is run again from its top.
pub fn switch_intent(
    service: &ServiceName,
    rec: &Record,
    rebooted: bool,
    previous: Option<&str>,
) -> Option<SwitchIntent> {
    if let Some(force) = &rec.force {
        let from = force
            .from_version
            .as_deref()
            .and_then(|version| Version::new(version).ok());
        let begun = force.verdict.is_none() & force.moves_current();
        let switched = begun & (force.phase >= ForcePhase::Switched);
        let target = Version::new(&force.target_version).ok()?;
        return switched.then(|| SwitchIntent {
            service: service.clone(),
            from,
            target,
            side: Side::Forward,
        });
    }
    let side = match (rec.step, resume_action(rec, rebooted)) {
        (_, ResumeAction::NothingFinished) | (Step::Prepare, _) => return None,
        (
            _,
            ResumeAction::RollbackThenDeferred { .. } | ResumeAction::RollbackThenFailed { .. },
        )
        | (Step::Rollback, _) => Side::Back,
        _ => Side::Forward,
    };
    Some(SwitchIntent {
        service: service.clone(),
        from: previous.and_then(|version| Version::new(version).ok()),
        target: Version::new(&rec.target_version).ok()?,
        side,
    })
}

/// What the start did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BootReport {
    /// The host booted since the last start pass wrote the boot id.
    pub rebooted: bool,
    /// The boot id was written.
    pub boot_id_written: bool,
    /// Every service with a state directory or a package directory, by name.
    pub services: BTreeMap<String, ServiceBoot>,
    /// The parts of the start, in the order they ran.
    pub stages: Vec<Stage>,
    /// What reconciling the placed paths did, or why it could not run: `None` before it ran.
    pub reconciled: Option<Result<ReconcileReport, String>>,
    /// How the readiness notice went: `None` before it was sent.
    pub ready: Option<Result<(), String>>,
    /// What the start settled of the update of hostkeeper itself that waited for the restart
    /// ([`Daemon::settle_self_update`]), or why it could not: `None` before the start did it.
    pub self_update: Option<Result<hostkeeper_steps::Settled, String>>,
}

/// Why the start pass could not run: the state directory cannot be listed.
#[derive(Debug, thiserror::Error)]
#[error("cannot list the step records under {}: {source}", dir.display())]
pub struct BootError {
    pub dir: PathBuf,
    #[source]
    pub source: io::Error,
}

/// Why an update could not be rebuilt from its record.
enum NotRebuilt {
    /// The release is not the one the record verified: the update halts.
    Changed(String),
    /// Something it needs cannot be read now: the update is held.
    Unresumable(String),
}

fn text(error: impl fmt::Display) -> String {
    error.to_string()
}

fn unresumable(error: impl fmt::Display) -> NotRebuilt {
    NotRebuilt::Unresumable(error.to_string())
}

/// A release that cannot be read now is held; one that is not what was verified halts.
fn sealed(error: ArtifactError) -> NotRebuilt {
    match &error {
        ArtifactError::Io(_) => NotRebuilt::Unresumable(error.to_string()),
        _ => NotRebuilt::Changed(error.to_string()),
    }
}

/// The record of `service` cannot be written: an alert, and the outcome that runs no hook.
fn journal(ctx: &Ctx, service: &ServiceName, error: &dyn fmt::Display) -> Outcome {
    ctx.alert(ALERT_JOURNAL, &format!("service={service}: {error}"));
    Outcome::JournalUnavailable(error.to_string())
}

/// The outcome of a force the start pass ran again: its verdict, or why it did not reach one.
fn forced(service: &ServiceName, reply: Reply) -> Outcome {
    match reply {
        Reply::Finished {
            verdict, reason, ..
        } => Outcome::Finished(Finished { verdict, reason }),
        other => Outcome::Interrupted {
            refusal: other.refusal().unwrap_or(Refusal::Busy),
            detail: format!("service={service}: the force reached no verdict: {other:?}"),
        },
    }
}

/// The bounds of a manifest's `[hook]`, as an order of it has them.
fn bounds(hook: &Hook) -> Bounds {
    let timeouts = &hook.timeout_secs;
    Bounds {
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
    }
}

/// The order of the update `rec` holds, rebuilt from the record, its params file and the sealed
/// version directory `packages/<service>/<version>`, with the service's lock it runs under. The
/// staged release and the one it replaces must still have the digests the record verified.
fn rebuild(
    ctx: &Ctx,
    service: &ServiceName,
    rec: &Record,
) -> Result<(UpdateOrder, Package), NotRebuilt> {
    let version = Version::new(&rec.target_version).map_err(unresumable)?;
    let digest = PackageDigest::new(&rec.tarball_digest).ok_or(NotRebuilt::Unresumable(
        "the step record's package digest is not 64 lower-case hex digits".to_owned(),
    ))?;
    let dir = ctx
        .dirs
        .packages
        .join(service.as_str())
        .join(version.as_str());
    let staged = Staged::seal(&dir).map_err(sealed)?;
    let params = Params::read(&rec.params).map_err(unresumable)?;
    let previous = params
        .previous_release()
        .map(|dir| Staged::seal(&dir))
        .transpose()
        .map_err(sealed)?;
    let found = (staged.digest(), previous.as_ref().map(Staged::digest));
    let recorded = (rec.artifact_digest.as_str(), rec.previous_digest.as_deref());
    if found != recorded {
        return Err(NotRebuilt::Changed(format!(
            "the release of {service} {version} on the host is not the one its step record verified"
        )));
    }
    let expected = Expected {
        name: service.clone(),
        version: version.clone(),
        platform: hostkeeper_config::platform::Platform::current(),
    };
    let manifest =
        crate::target::manifest(&dir, &expected, &ctx.own_version).map_err(unresumable)?;
    let order = UpdateOrder {
        service: service.to_string(),
        target_version: version.to_string(),
        previous_version: params.previous_version().map(str::to_owned),
        artifact_digest: rec.tarball_digest.clone(),
        staged,
        previous,
        hook_binary: manifest
            .hook
            .command
            .first()
            .map(PathBuf::from)
            .unwrap_or_default(),
        bounds: bounds(&manifest.hook),
        // Written to the params file when the update began; a resumed update does not write it.
        declared_params: Params::default(),
        min_updater_protocol: None,
        order_try: rec.order_try,
        // As the update made it: a version with no hook program runs the built-in steps.
        builtin_steps: crate::selfupdate::builtin_steps(service, manifest.min_hostkeeper)
            | manifest.hook.command.is_empty(),
    };
    let package = Package {
        verb: HookedVerb::Update,
        version,
        digest,
    };
    Ok((order, package))
}

/// The outcome of a run of the step runner.
fn ran(ctx: &Ctx, service: &ServiceName, result: Result<Finished, RunError>) -> Outcome {
    match result {
        // The update reached the post-success watcher, which is left to its own thread.
        Ok(Finished {
            verdict: Verdict::Succeeded {
                promotion_pending: true,
            },
            ..
        }) => Outcome::Watching,
        Ok(finished) => Outcome::Finished(finished),
        Err(RunError::Record(error)) => journal(ctx, service, &error),
        Err(error) => {
            let detail = format!("service={service}: {error}");
            let refusal = verbs::update::interrupted(error)
                .refusal()
                .unwrap_or(Refusal::StoreFailed);
            if refusal == Refusal::Unstopped {
                ctx.alert(ALERT_UNSTOPPABLE, &detail);
            }
            Outcome::Interrupted { refusal, detail }
        }
    }
}

/// The version `current` names in the package directory `dir`: `None` when there is no
/// `current`. A `current` that is not a link, or not a link to a version directory beside it (one
/// plain name that is a version), is refused: its tree is never run.
fn current_version(dir: &Path) -> Result<Option<Version>, String> {
    let target = match fs::read_link(dir.join(CURRENT)) {
        Ok(target) => target,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("`current` is not a link: {error}")),
    };
    let mut parts = target.components();
    let name = match (parts.next(), parts.next()) {
        (Some(Component::Normal(name)), None) => name.to_string_lossy().into_owned(),
        _ => {
            return Err(format!(
                "`current` links to {}, not to a version directory beside it",
                target.display()
            ));
        }
    };
    Version::new(&name).map(Some).map_err(text)
}

/// The params of a `boot` step: the manifest's `[params]` and the release it runs from.
pub(crate) fn boot_params(
    manifest: &Manifest,
    dir: &Path,
    version: &Version,
) -> Result<Params, String> {
    let mut params = Params::parse_declared(&manifest.params.to_string()).map_err(text)?;
    params.insert(
        STAGED_RELEASE_KEY,
        Scalar::Text(dir.to_string_lossy().into_owned()),
    );
    params.insert(TARGET_VERSION_KEY, Scalar::Text(version.to_string()));
    Ok(params)
}

impl Daemon {
    /// The switches the step records of the state directory say were begun ([`switch_intent`]),
    /// for the reconciliation that runs before [`Daemon::boot_pass`]. `rebooted` is
    /// [`boot_changed`]. A record that cannot be read gives none, and one whose params file
    /// cannot be read gives none either: without it a first install cannot be told from an
    /// update, and an intent that guessed would undo the wrong one. Nothing is written.
    pub fn switch_intents(&self, rebooted: bool) -> Result<Vec<SwitchIntent>, BootError> {
        let ctx = self.ctx();
        let names = installed::service_dirs(&ctx.dirs.state).map_err(|source| BootError {
            dir: ctx.dirs.state.clone(),
            source,
        })?;
        let mut intents = Vec::new();
        for service in names.into_values() {
            let Ok(Loaded::Current(rec)) = ctx.store(&service).load() else {
                continue;
            };
            let previous = match rec.force {
                Some(_) => Ok(None),
                None => Params::read(&rec.params)
                    .map(|params| params.previous_version().map(str::to_owned)),
            };
            let intent = previous
                .ok()
                .and_then(|previous| switch_intent(&service, &rec, rebooted, previous.as_deref()));
            intents.extend(intent.filter(|_| rec.service == service.as_str()));
        }
        Ok(intents)
    }

    /// The start pass: steps 1 to 7 of the module's list. `Err` only when the state directory
    /// cannot be listed; everything that goes wrong for one service is in its entry of the
    /// report, and the other services go on.
    pub fn boot_pass(&self) -> Result<BootReport, BootError> {
        let mut report = BootReport::default();
        self.boot_pass_into(&mut report, &|_| false)?;
        Ok(report)
    }

    /// [`Daemon::boot_pass`] into `report`, the write of the boot id stopped where `stop` says,
    /// as a crash would.
    pub fn boot_pass_into(
        &self,
        report: &mut BootReport,
        stop: &dyn Fn(WriteStep) -> bool,
    ) -> Result<(), BootError> {
        let ctx = self.ctx();
        let state = &ctx.dirs.state;
        report.stages.push(Stage::ReadRecords);
        let names = installed::service_dirs(state).map_err(|source| BootError {
            dir: state.clone(),
            source,
        })?;
        let mut loaded = Vec::new();
        for service in names.into_values() {
            let outcome = match ctx.store(&service).load() {
                // A record of another service is not this service's: it is held as corrupt.
                Ok(Loaded::Current(rec)) if rec.service != service.as_str() => Outcome::Corrupt(
                    format!("the step record names the service {:?}", rec.service),
                ),
                Ok(Loaded::Current(rec)) => {
                    loaded.push((service, *rec));
                    continue;
                }
                Ok(Loaded::Absent) => Outcome::Idle,
                Ok(Loaded::Newer { schema }) => Outcome::RecordNewer(schema),
                Err(RecordError::Corrupt(why)) => Outcome::Corrupt(why),
                Err(error) => Outcome::Unreadable(error.to_string()),
            };
            report
                .services
                .insert(service.to_string(), ServiceBoot::of(None, outcome));
        }
        let rebooted = boot_changed(state, &*ctx.clock);
        report.rebooted = rebooted;
        let mut unfinished = Vec::new();
        // A force under way is not in the resume table: it runs again from the top of its record.
        let mut forces = Vec::new();
        for (service, mut rec) in loaded {
            if let Some(force) = rec.force.clone().filter(|force| force.verdict.is_none()) {
                forces.push((service, rec, force));
                continue;
            }
            let (action, changed) = resume(&mut rec, rebooted, &*ctx.clock);
            let written = match changed {
                true => ctx.store(&service).save(&rec),
                false => Ok(()),
            };
            let outcome = match (written, action) {
                (Err(error), _) => journal(ctx, &service, &error),
                (Ok(()), ResumeAction::NothingFinished) => Outcome::Idle,
                (Ok(()), _) => {
                    unfinished.push((service, rec, action));
                    continue;
                }
            };
            report
                .services
                .insert(service.to_string(), ServiceBoot::of(Some(action), outcome));
        }
        report.stages.push(Stage::WriteBootId);
        // A record not read, or not rewritten, must be read as after a boot by the next start.
        let unsettled = report.services.values().any(|entry| {
            matches!(
                entry.outcome,
                Outcome::Unreadable(_) | Outcome::JournalUnavailable(_)
            )
        });
        let written = match unsettled {
            true => Err(io::Error::other(
                "a step record could not be read or rewritten, so the old boot id is kept",
            )),
            false => commit_boot_stopping(state, &*ctx.clock, stop),
        };
        report.boot_id_written = written.is_ok();
        let unfinished = match written {
            Ok(()) => unfinished,
            Err(error) => {
                let why = format!("the boot id is not written: {error}");
                ctx.alert(ALERT_JOURNAL, &format!("{why}; no hook runs in this start"));
                for (service, _, action) in unfinished {
                    let outcome = journal(ctx, &service, &why);
                    report
                        .services
                        .insert(service.to_string(), ServiceBoot::of(Some(action), outcome));
                }
                for (service, _, _) in forces.drain(..) {
                    let outcome = journal(ctx, &service, &why);
                    report
                        .services
                        .insert(service.to_string(), ServiceBoot::of(None, outcome));
                }
                Vec::new()
            }
        };
        report.stages.push(Stage::ResumeUninstalls);
        if report.boot_id_written {
            self.resume_uninstalls(report);
        }
        report.stages.push(Stage::RunUpdates);
        for (service, rec, force) in forces {
            let reply = verbs::force::resume(ctx, &service, rec, force);
            let outcome = forced(&service, reply);
            report
                .services
                .insert(service.to_string(), ServiceBoot::of(None, outcome));
        }
        let mut watch = Vec::new();
        for (service, rec, action) in unfinished {
            let outcome = match action {
                ResumeAction::WatcherRestart => Outcome::Watching,
                _ => self.run_update(&service, &rec),
            };
            match outcome {
                Outcome::Watching => watch.push((service, rec, action)),
                outcome => {
                    report
                        .services
                        .insert(service.to_string(), ServiceBoot::of(Some(action), outcome));
                }
            }
        }
        report.stages.push(Stage::RestartWatchers);
        for (service, rec, action) in watch {
            let outcome = self.restart_watcher(&service, &rec);
            report
                .services
                .insert(service.to_string(), ServiceBoot::of(Some(action), outcome));
        }
        Ok(())
    }

    /// The uninstalls a tombstone in `placed.json` left unfinished, resumed before any update
    /// (design section 4.7), each reply the service's outcome.
    fn resume_uninstalls(&self, report: &mut BootReport) {
        for (service, reply) in crate::uninstall::resume_all(self.ctx()) {
            let outcome = match reply {
                Reply::Finished {
                    verdict, reason, ..
                } => Outcome::Uninstall(Finished { verdict, reason }),
                other => Outcome::Interrupted {
                    refusal: other.refusal().unwrap_or(Refusal::Busy),
                    detail: format!("service={service}: the uninstall did not finish: {other:?}"),
                },
            };
            report
                .services
                .insert(service.to_string(), ServiceBoot::of(None, outcome));
        }
    }

    /// Runs the unfinished update `rec` holds to its verdict, or to the post-success watcher.
    fn run_update(&self, service: &ServiceName, rec: &Record) -> Outcome {
        let ctx = self.ctx();
        let (order, package) = match rebuild(ctx, service, rec) {
            Ok(rebuilt) => rebuilt,
            Err(NotRebuilt::Changed(why)) => return self.halt_changed(service, rec, &why),
            Err(NotRebuilt::Unresumable(why)) => return Outcome::Unresumable(why),
        };
        let guard = match ctx.locks.take(service, package) {
            Ok(guard) => guard,
            Err(held) => {
                return Outcome::Interrupted {
                    refusal: Refusal::Busy,
                    detail: format!("service={service}: locked by {}", held.holder),
                };
            }
        };
        let result = {
            let _step = guard.step(&ctx.step_lock);
            ctx.runner(service).run_until_watching(&order)
        };
        let outcome = ran(ctx, service, result);
        match &outcome {
            // The step could not be stopped: the service stays locked, and its record running.
            Outcome::Interrupted {
                refusal: Refusal::Unstopped,
                ..
            } => guard.keep(),
            _ => drop(guard),
        }
        outcome
    }

    /// Starts the post-success watcher of the update `rec` holds.
    fn restart_watcher(&self, service: &ServiceName, rec: &Record) -> Outcome {
        match rebuild(self.ctx(), service, rec) {
            Ok((order, package)) => {
                self.ctx().start_watcher(service, order, package);
                Outcome::Watching
            }
            Err(NotRebuilt::Changed(why)) => self.halt_changed(service, rec, &why),
            Err(NotRebuilt::Unresumable(why)) => Outcome::Unresumable(why),
        }
    }

    /// Ends the update `rec` holds as the runner ends one whose release changed after it was
    /// verified: `Halt` with the reason `artifact_changed`, an alert. The unit of its step is
    /// stopped first, in case it runs.
    fn halt_changed(&self, service: &ServiceName, rec: &Record, why: &str) -> Outcome {
        let ctx = self.ctx();
        if let Err(error) = ctx.launcher.stop(&rec.unit(rec.step)) {
            return Outcome::Interrupted {
                refusal: Refusal::LauncherFailed,
                detail: format!("service={service}: {error}"),
            };
        }
        ctx.alert(ALERT_ARTIFACT_CHANGED, &format!("service={service}: {why}"));
        let finished = Finished {
            verdict: Verdict::Halt,
            reason: Some(REASON_ARTIFACT_CHANGED.to_owned()),
        };
        let mut halted = rec.clone();
        halted.state = State::Finished {
            verdict: finished.verdict.clone(),
            reason: finished.reason.clone(),
        };
        match ctx.store(service).save(&halted) {
            Ok(()) => Outcome::Finished(finished),
            Err(error) => journal(ctx, service, &error),
        }
    }

    /// After the start pass: the `boot` step of every service with a package directory whose
    /// update is not unfinished or held, from `current`, one after another; then the readiness
    /// notice, exactly once.
    pub fn after_boot(&self, report: &mut BootReport, notifier: &dyn ReadyNotifier) {
        let ctx = self.ctx();
        report.stages.push(Stage::BootSteps);
        let names = installed::service_dirs(&ctx.dirs.packages).unwrap_or_else(|error| {
            ctx.alert(
                ALERT_START,
                &format!("the package directory cannot be listed, so no boot step runs: {error}"),
            );
            BTreeMap::new()
        });
        let written = report.boot_id_written;
        for service in names.into_values() {
            let entry = report
                .services
                .entry(service.to_string())
                .or_insert_with(ServiceBoot::idle);
            let free = matches!(entry.outcome, Outcome::Idle | Outcome::Finished(_));
            entry.boot_step = match written & free {
                true => self.boot_step(&service),
                false => BootStep::Held,
            };
        }
        report.stages.push(Stage::Ready);
        let sent = notifier.ready().map_err(text);
        if let Err(error) = &sent {
            ctx.alert(
                ALERT_START,
                &format!("the readiness notice was not sent: {error}"),
            );
        }
        report.ready = Some(sent);
    }

    /// The `boot` step of `service`, run and judged.
    fn boot_step(&self, service: &ServiceName) -> BootStep {
        let failed = |why: String| {
            self.ctx()
                .alert(ALERT_BOOT_STEP, &format!("service={service}: {why}"));
            BootStep::Failed(why)
        };
        match self.run_boot_step(service) {
            Ok(None) => BootStep::NotRun,
            Ok(Some(StepExit { code: Some(0), .. })) => BootStep::Succeeded,
            Ok(Some(exit)) => failed(format!(
                "the boot step ended with code {:?}: {}",
                exit.code,
                exit.last_line.unwrap_or_default()
            )),
            Err(why) => failed(why),
        }
    }

    /// Runs the `boot` step of `service` from `current`: a tree that still has the digest of its
    /// seal, the manifest's hook, the params file of the update mechanism, the unit
    /// `updater-<service>-boot-<generation>` (a unit of that name left by an earlier start is
    /// stopped first), bounded by the manifest's `cutover` bound. `None` when there is nothing to
    /// run.
    ///
    /// A launcher that fails after it may have started the unit (anything but `NoStart`) leaves
    /// the unit possibly alive: it is stopped before its params file goes, and when it cannot be
    /// stopped the params file stays and the service is locked for good (`unstoppable_unit`), so
    /// that no order of it runs beside the step.
    fn run_boot_step(&self, service: &ServiceName) -> Result<Option<StepExit>, String> {
        let ctx = self.ctx();
        let package_dir = ctx.dirs.packages.join(service.as_str());
        let Some(version) = current_version(&package_dir)? else {
            return Ok(None);
        };
        let layout = Layout::package(&package_dir);
        let dir = layout.version_path(version.as_str());
        let expected = Expected {
            name: service.clone(),
            version: version.clone(),
            platform: hostkeeper_config::platform::Platform::current(),
        };
        let manifest = crate::target::manifest(&dir, &expected, &ctx.own_version).map_err(text)?;
        let Some(step) = manifest.hook.boot.as_deref() else {
            return Ok(None);
        };
        let seal = read_seal(&layout.seal_path(version.as_str())).map_err(text)?;
        let seal = Seal::parse(&seal).map_err(text)?;
        let staged = Staged::seal(&dir).map_err(text)?;
        if staged.digest() != seal.tree_digest() {
            return Err(format!(
                "{REASON_ARTIFACT_CHANGED}: {} is not the tree its seal names",
                dir.display()
            ));
        }
        let command = &manifest.hook.command;
        let hook = command.first().map(PathBuf::from).unwrap_or_default();
        let hook_args = command.get(1..).unwrap_or_default();
        let executable = staged.check(&hook).map_err(text)?;
        let params = boot_params(&manifest, &dir, &version)?;
        files::make_private(&ctx.service_dir(service)).map_err(text)?;
        let store = ctx.store(service);
        let generation = match store.load() {
            Ok(Loaded::Current(rec)) => rec.generation,
            _ => 0,
        };
        let unit = format!("updater-{service}-boot-{generation}");
        ctx.launcher.stop(&unit).map_err(text)?;
        store.save_params(&params.to_toml()).map_err(text)?;
        let ran = ctx.launcher.run_additive(&AdditiveCall {
            service: service.as_str(),
            step,
            hook_args,
            params: &store.params_path(),
            executable: &executable,
            unit: &unit,
            timeout_secs: manifest.hook.timeout_secs.cutover.get(),
        });
        let ended = match ran {
            Ok(exit) => Ok(Some(exit)),
            Err(error @ LaunchError::NoStart(_)) => Err(error.to_string()),
            // The unit may have started and run on: it is stopped before its params file goes.
            Err(error) => match ctx.launcher.stop(&unit) {
                Ok(()) => Err(format!("{error}; its unit {unit} was stopped")),
                Err(stop) => {
                    // The seal's package digest was checked when the seal was read.
                    let package = PackageDigest::new(seal.package_digest()).map(|digest| Package {
                        verb: HookedVerb::Update,
                        version,
                        digest,
                    });
                    let locked = package.and_then(|package| ctx.locks.take(service, package).ok());
                    locked.into_iter().for_each(ServiceGuard::keep);
                    let why = format!(
                        "{error}; its unit {unit} could not be stopped ({stop}), so {service} stays locked"
                    );
                    ctx.alert(ALERT_UNSTOPPABLE, &format!("service={service}: {why}"));
                    return Err(why);
                }
            },
        };
        store.remove_params().map_err(text)?;
        ended
    }
}
