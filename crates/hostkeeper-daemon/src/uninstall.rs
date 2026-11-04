//! The uninstall of a service (design section 4.7, "Uninstall"), in the order of the design,
//! each step durable before the next:
//!
//! 1. refused before anything changes: the service `hostkeeper` (the package manager removes
//!    it), a release whose manifest has no `[hook.timeout_secs] uninstall`
//!    (`uninstall_unsupported`), an unfinished order in the step record (`busy`), a halt in it
//!    (`halted`), a held halt generation or a release that cannot be read;
//! 2. the tombstone `uninstalling` of the service written in `placed.json` (temporary file,
//!    fsync, rename, under the lock of `placed.json`);
//! 3. every unit of `[units] run` stopped; one still active after its stop bound ends the
//!    uninstall `Failed`, class environment, `unit_stuck`, with nothing removed;
//! 4. the hook's `uninstall` step, from `current`, bounded by `[hook.timeout_secs] uninstall`:
//!    exit 0 is recorded in the tombstone (`hook_done`) and the uninstall goes on; exit 1, a
//!    timeout or a signal is `Failed`; 75 is `Deferred`; 76 is a halt, recorded in the tombstone
//!    (`halted`), and nothing more of the uninstall runs;
//! 5. the units disabled, every placed link removed and `daemon-reload` run, and the paths
//!    dropped from `placed.json` ([`hostkeeper_placed::Placer::remove_all`], which records the
//!    drop in the write that owes the reload, so that a crash between the two leaves it owed);
//! 6. last, in this order: `current`, `previous`, the seals, the version directories and what
//!    else is in the package directory, then the step record and params file (the held halt
//!    generation stays: it is never lowered); then the tombstone.
//!
//! The position is read from disk only: no tombstone is step 1; a tombstone is step 3, or step 5
//! once it says `hook_done`. Every step is idempotent, so a crash anywhere resumes, at the next
//! order or at the daemon's start ([`resume_all`]), by doing again what is not known to be done.
//! The daemon never touches `/var/lib/<service>` or `/etc/<service>`: that is the hook's
//! business. A service with no release installed has no unit to stop and no hook to run: its
//! tombstone is written with `hook_done`, and what an install left goes.

use std::io;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use hostkeeper_config::manifest::{Expected, Manifest};
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_hook::{Class, Outcome, Phase, Report, Step, Verdict};
use hostkeeper_placed::{FileKind, Fs, PlacedError, RealFs, Tombstone};
use hostkeeper_steps::record::{Loaded, PARAMS_FILE, RECORD_FILE};
use hostkeeper_steps::{AdditiveCall, Finished, Layout, Seal, Staged, StepExit, read_seal};

use crate::boot::boot_params;
use crate::checks;
use crate::executor::{Ctx, Reply};
use crate::files;
use crate::halt::HELD_HALT_FILE;
use crate::installed::{self, CURRENT, PREVIOUS};
use crate::refusal::Refusal;
use crate::verbs::update::last_record;

/// The service no uninstall removes: the package manager does.
pub const HOSTKEEPER: &str = "hostkeeper";
/// The reason of an uninstall that a unit of `[units] run` would not stop.
pub const REASON_UNIT_STUCK: &str = "unit_stuck";

fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn interrupted(refusal: Refusal, detail: impl ToString) -> Reply {
    Reply::Interrupted {
        refusal,
        detail: detail.to_string(),
    }
}

fn finished(verdict: Verdict, reason: Option<String>) -> Reply {
    Reply::Finished {
        verdict,
        reason,
        resent: false,
    }
}

/// A failure of `placed.json` or of the host's files after the tombstone: the uninstall stops
/// where it is, and the next order or start resumes it.
fn link_failed(error: impl std::fmt::Display) -> Reply {
    interrupted(Refusal::LinkFailed, error)
}

/// Whether `placed.json` holds a tombstone for `service`.
pub fn tombstoned(ctx: &Ctx, service: &ServiceName) -> Result<bool, PlacedError> {
    let record = ctx.placer().store().load()?;
    Ok(record.uninstalling.contains_key(service.as_str()))
}

/// The uninstall of `service`, under its lock (the caller holds it): begun, or resumed from its
/// tombstone. The reply is the uninstall's verdict, a refusal before anything changed, or an
/// interruption that a later order or start resumes.
pub fn uninstall(ctx: &Ctx, service: &ServiceName) -> Reply {
    let record = match ctx.placer().store().load() {
        Ok(record) => record,
        Err(error) => return Reply::refused(Refusal::from(&error), error),
    };
    match record.uninstalling.get(service.as_str()) {
        Some(tombstone) => resume(ctx, service, tombstone),
        None => match begin(ctx, service) {
            Ok(Some(tombstone)) => resume(ctx, service, &tombstone),
            Ok(None) => finished(
                Verdict::Succeeded {
                    promotion_pending: false,
                },
                None,
            ),
            Err(refused) => refused,
        },
    }
}

/// Steps 1 and 2: the refusals, then the tombstone. `None` when nothing of the service is on the
/// host: there is nothing to uninstall, and nothing is written.
fn begin(ctx: &Ctx, service: &ServiceName) -> Result<Option<Tombstone>, Reply> {
    let unsupported = |why: String| Reply::refused(Refusal::UninstallUnsupported, why);
    if service.as_str() == HOSTKEEPER {
        return Err(unsupported(format!(
            "{HOSTKEEPER} is not uninstalled by its daemon: the package manager removes it"
        )));
    }
    ctx.halts
        .get(service)
        .map_err(|e| Reply::refused(Refusal::HaltUnreadable, e))?;
    if let Some(rec) = last_record(ctx, service)? {
        match checks::finished_of(&rec) {
            None => {
                return Err(Reply::refused(
                    Refusal::Busy,
                    format!(
                        "the step record of {service} holds an unfinished order to {}",
                        rec.target_version
                    ),
                ));
            }
            Some(Finished {
                verdict: Verdict::Halt,
                ..
            }) => {
                return Err(Reply::refused(
                    Refusal::Halted,
                    format!("{service} is held by a halt until `clear_halt`"),
                ));
            }
            Some(_) => {}
        }
    }
    let hook_done = match installed::linked_version(&ctx.dirs.packages, service, CURRENT) {
        Some(version) => {
            let release = current_release(ctx, service, &version)
                .map_err(|why| Reply::refused(Refusal::ReleaseUnreadable, why))?;
            release.bound().ok_or_else(|| {
                unsupported(format!(
                    "the manifest of {service} {version} has no [hook.timeout_secs] uninstall"
                ))
            })?;
            false
        }
        None => {
            let left = [
                ctx.dirs.packages.join(service.as_str()),
                ctx.service_dir(service),
            ];
            match left.iter().any(|dir| dir.exists()) {
                true => true,
                false => return Ok(None),
            }
        }
    };
    let tombstone = write_tombstone(ctx, service, |tombstone| tombstone.hook_done |= hook_done)
        .map_err(|e| Reply::refused(Refusal::from(&e), e))?;
    Ok(Some(tombstone))
}

/// Writes the tombstone of `service`, made when it has none and changed by `change`, under the
/// lock of `placed.json`; the tombstone as written.
fn write_tombstone(
    ctx: &Ctx,
    service: &ServiceName,
    change: impl FnOnce(&mut Tombstone),
) -> Result<Tombstone, PlacedError> {
    let placer = ctx.placer();
    let lock = placer.lock()?;
    let mut record = placer.store().load()?;
    let tombstone = record.uninstalling.entry(service.to_string()).or_default();
    change(tombstone);
    let written = tombstone.clone();
    placer.store().save(&record, &lock)?;
    Ok(written)
}

/// Steps 3 to 6, from where the tombstone says the uninstall is.
fn resume(ctx: &Ctx, service: &ServiceName, tombstone: &Tombstone) -> Reply {
    match (tombstone.halted, tombstone.hook_done) {
        (true, _) => Reply::refused(
            Refusal::Halted,
            format!(
                "the uninstall of {service} halted; nothing more of it runs until `clear_halt`"
            ),
        ),
        (false, true) => remove(ctx, service),
        (false, false) => match stop_and_run_hook(ctx, service) {
            Ok(()) => match write_tombstone(ctx, service, |t| t.hook_done = true) {
                Ok(_) => remove(ctx, service),
                Err(error) => link_failed(error),
            },
            Err(ended) => ended,
        },
    }
}

/// The release `current` names, checked against its seal, with its manifest and its hook.
struct Release {
    version: Version,
    dir: PathBuf,
    manifest: Manifest,
    executable: PathBuf,
}

impl Release {
    /// The manifest's bound of the `uninstall` step: `None` when it has no way to run it.
    fn bound(&self) -> Option<NonZeroU64> {
        self.manifest.hook.timeout_secs.uninstall
    }
}

/// The release of `service` at `version`: its manifest read for the service and version, its
/// tree the one its seal names, its hook a file of it. A release that is not what was verified
/// never runs.
fn current_release(ctx: &Ctx, service: &ServiceName, version: &Version) -> Result<Release, String> {
    let layout = Layout::package(&ctx.dirs.packages.join(service.as_str()));
    let dir = layout.version_path(version.as_str());
    let expected = Expected {
        name: service.clone(),
        version: version.clone(),
    };
    let manifest = Manifest::load(&dir, &expected, &ctx.own_version).map_err(text)?;
    let seal = read_seal(&layout.seal_path(version.as_str())).map_err(text)?;
    let seal = Seal::parse(&seal).map_err(text)?;
    let staged = Staged::seal(&dir).map_err(text)?;
    if staged.digest() != seal.tree_digest() {
        return Err(format!("{} is not the tree its seal names", dir.display()));
    }
    let hook = manifest.hook.command.first().map(PathBuf::from);
    let executable = staged.check(&hook.unwrap_or_default()).map_err(text)?;
    Ok(Release {
        version: version.clone(),
        dir,
        manifest,
        executable,
    })
}

/// Steps 3 and 4: the `run` units stopped, then the hook's `uninstall` step. `Ok` when it exited
/// 0; otherwise the reply the uninstall ends with.
fn stop_and_run_hook(ctx: &Ctx, service: &ServiceName) -> Result<(), Reply> {
    let version =
        installed::linked_version(&ctx.dirs.packages, service, CURRENT).ok_or_else(|| {
            interrupted(
                Refusal::ReleaseUnreadable,
                format!("{service} has no `current` to run its uninstall step from"),
            )
        })?;
    let release = current_release(ctx, service, &version)
        .map_err(|why| interrupted(Refusal::ReleaseUnreadable, why))?;
    let bound = release.bound().ok_or_else(|| {
        interrupted(
            Refusal::UninstallUnsupported,
            format!("the manifest of {service} {version} has no [hook.timeout_secs] uninstall"),
        )
    })?;
    for unit in &release.manifest.run {
        ctx.units.stop_unit(unit).map_err(|error| {
            ctx.alert(REASON_UNIT_STUCK, &format!("service={service}: {error}"));
            let environment = Verdict::Failed {
                phase: Phase::BeforeCommit,
                class: Class::Environment,
            };
            finished(environment, Some(REASON_UNIT_STUCK.to_owned()))
        })?;
    }
    let exit = run_hook(ctx, service, &release, bound)?;
    let report = Report::read(exit.last_line.as_deref().unwrap_or(""));
    // How each end of the step is read: the `uninstall` row of `hostkeeper_hook::next`.
    match Outcome::from_exit(exit.code) {
        Outcome::Success => Ok(()),
        Outcome::Failure => Err(finished(
            Verdict::Failed {
                phase: Phase::BeforeCommit,
                class: report.class,
            },
            report.reason,
        )),
        Outcome::Deferred => Err(finished(
            Verdict::Deferred {
                retry_after_secs: report.retry_after_secs,
            },
            report.reason,
        )),
        // The halt is recorded in the tombstone; a tombstone that cannot be written leaves the
        // uninstall interrupted, to run the step again.
        Outcome::Halt => Err(
            write_tombstone(ctx, service, |tombstone| tombstone.halted = true)
                .map_or_else(link_failed, |_| finished(Verdict::Halt, report.reason)),
        ),
    }
}

/// Runs the hook's `uninstall` step from `release`: the params of `current` in the service's
/// params file, the unit `updater-<service>-uninstall-<generation>` (one of that name left by a
/// crash is stopped first), bounded by the manifest's `uninstall` bound. A launcher that fails
/// after it may have started the unit has it stopped; one that cannot be stopped is
/// `unstopped`, and the caller keeps the service locked.
fn run_hook(
    ctx: &Ctx,
    service: &ServiceName,
    release: &Release,
    bound: NonZeroU64,
) -> Result<StepExit, Reply> {
    let params = boot_params(&release.manifest, &release.dir, &release.version)
        .map_err(|why| Reply::refused(Refusal::ManifestMismatch, why))?;
    files::make_private(&ctx.service_dir(service))
        .map_err(|e| interrupted(Refusal::StoreFailed, e))?;
    let store = ctx.store(service);
    let generation = match store.load() {
        Ok(Loaded::Current(rec)) => rec.generation,
        _ => 0,
    };
    let unit = format!("updater-{service}-{}-{generation}", Step::Uninstall.name());
    ctx.launcher
        .stop(&unit)
        .map_err(|e| interrupted(Refusal::LauncherFailed, e))?;
    store
        .save_params(&params.to_toml())
        .map_err(|e| interrupted(Refusal::StoreFailed, e))?;
    let command = &release.manifest.hook.command;
    let ran = ctx.launcher.run_additive(&AdditiveCall {
        service: service.as_str(),
        step: Step::Uninstall.name(),
        hook_args: command.get(1..).unwrap_or_default(),
        params: &store.params_path(),
        executable: &release.executable,
        unit: &unit,
        timeout_secs: bound.get(),
    });
    let exit = ran.map_err(|error| match ctx.launcher.stop(&unit) {
        Ok(()) => interrupted(
            Refusal::LauncherFailed,
            format!("{error}; its unit {unit} was stopped"),
        ),
        Err(stop) => interrupted(
            Refusal::Unstopped,
            format!("{error}; its unit {unit} could not be stopped ({stop})"),
        ),
    })?;
    store
        .remove_params()
        .map_err(|e| interrupted(Refusal::StoreFailed, e))?;
    Ok(exit)
}

/// Steps 5 and 6, then the tombstone goes: the service is gone.
fn remove(ctx: &Ctx, service: &ServiceName) -> Reply {
    if let Err(error) = ctx.placer().remove_all(service) {
        return link_failed(error);
    }
    // The package directory as the placement's filesystem reaches it; the daemon's own state
    // directory, which is no placed path, as this process sees it.
    let packages = ctx.placement.roots.packages().dir();
    let removed = remove_package(&*ctx.placement.fs, packages, service)
        .and_then(|()| remove_records(&RealFs::new(Path::new("/")), &ctx.dirs.state, service));
    if let Err(error) = removed {
        return link_failed(format!(
            "the files of {service} could not be removed: {error}"
        ));
    }
    let dropped = {
        let placer = ctx.placer();
        placer.lock().and_then(|lock| {
            let mut record = placer.store().load()?;
            record.uninstalling.remove(service.as_str());
            placer.store().save(&record, &lock)
        })
    };
    match dropped {
        Ok(()) => finished(
            Verdict::Succeeded {
                promotion_pending: false,
            },
            None,
        ),
        Err(error) => link_failed(error),
    }
}

/// The names in `dir`, sorted; none when it is gone.
fn names(fs: &dyn Fs, dir: &Path) -> io::Result<Vec<String>> {
    let listed = match fs.list(dir) {
        Ok(listed) => listed,
        Err(error) => match error.kind() {
            io::ErrorKind::NotFound => Vec::new(),
            _ => return Err(error),
        },
    };
    Ok(listed
        .into_iter()
        .map(|name| name.to_string_lossy().into_owned())
        .collect())
}

/// Removes what is at `path`, a directory with everything in it; nothing there is no error.
fn remove_tree(fs: &dyn Fs, path: &Path) -> io::Result<()> {
    match fs.kind(path)? {
        None => Ok(()),
        Some(FileKind::Dir) => {
            for name in names(fs, path)? {
                remove_tree(fs, &path.join(name))?;
            }
            fs.remove_dir(path)
        }
        Some(FileKind::File | FileKind::Symlink | FileKind::Other) => fs.remove_file(path),
    }
}

/// Step 6 in the package directory of `service`: `current`, `previous`, the seals, the version
/// directories, what else is there, the directory itself; then an fsync of its parent.
fn remove_package(fs: &dyn Fs, packages: &Path, service: &ServiceName) -> io::Result<()> {
    let dir = packages.join(service.as_str());
    remove_tree(fs, &dir.join(CURRENT))?;
    remove_tree(fs, &dir.join(PREVIOUS))?;
    let listed = names(fs, &dir)?;
    let seals = listed.iter().filter(|name| name.ends_with(".seal"));
    let versions = listed
        .iter()
        .filter(|name| Version::new(name.as_str()).is_ok());
    for name in seals.chain(versions) {
        remove_tree(fs, &dir.join(name))?;
    }
    remove_tree(fs, &dir)?;
    sync_if_there(fs, packages)
}

/// An fsync of the directory `dir`, when there is one.
fn sync_if_there(fs: &dyn Fs, dir: &Path) -> io::Result<()> {
    match fs.kind(dir)? {
        Some(_) => fs.sync_dir(dir),
        None => Ok(()),
    }
}

/// Step 6 in the state directory of `service`: everything but the held halt generation, which is
/// never lowered, and the directory itself when nothing else is left; then an fsync of its
/// parent.
fn remove_records(fs: &dyn Fs, state: &Path, service: &ServiceName) -> io::Result<()> {
    let dir = state.join(service.as_str());
    remove_tree(fs, &dir.join(RECORD_FILE))?;
    remove_tree(fs, &dir.join(PARAMS_FILE))?;
    let rest = names(fs, &dir)?;
    for name in rest.iter().filter(|name| name.as_str() != HELD_HALT_FILE) {
        remove_tree(fs, &dir.join(name))?;
    }
    if names(fs, &dir)?.is_empty() & fs.kind(&dir)?.is_some() {
        fs.remove_dir(&dir)?;
    }
    sync_if_there(fs, state)
}

/// At the daemon's start, after the boot id is written: every tombstoned uninstall resumed
/// under its service's lock, without waiting for an order. Each reply, by service.
pub fn resume_all(ctx: &Ctx) -> Vec<(ServiceName, Reply)> {
    let record = match ctx.placer().store().load() {
        Ok(record) => record,
        Err(error) => {
            ctx.alert(
                crate::boot::ALERT_START,
                &format!("placed.json cannot be read, so no uninstall resumes: {error}"),
            );
            return Vec::new();
        }
    };
    record
        .uninstalling
        .keys()
        .filter_map(|name| ServiceName::new(name).ok())
        .map(|service| {
            let reply = crate::verbs::uninstall::locked(ctx, &service, 0);
            (service, reply)
        })
        .collect()
}
