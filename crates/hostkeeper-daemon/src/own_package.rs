//! hostkeeper's own native package (docs/design/packages-native.md, section 10.4): the update of
//! the service `hostkeeper` through `apt`, its new binary installing its own package only after
//! it confirmed itself; and the adoption of the package the host was set up with.
//!
//! **The switch is left to the new binary.** An update of hostkeeper itself whose release is a
//! native version, or on a host where hostkeeper already runs from its native package
//! ([`deferred`]), switches nothing before its new binary runs: the step runner's switch does
//! nothing for it ([`crate::place::hooked_switch`]), its built-in steps do nothing
//! ([`crate::builtin`]), and the hand-over stages the new binary from the version's tree
//! ([`crate::selfupdate::release_binary`]). So `/usr/bin/hostkeeper`, the program the unit
//! starts, is replaced only by a binary that has started on this host, and a new binary that
//! cannot start leaves everything as it was.
//!
//! **The `activate` after the confirmation.** The new binary, once it confirmed itself, finds the
//! version it runs not yet `current` in the packages directory ([`needs_activate`]): its
//! confirmation leaves the `activate` pending in the step record
//! ([`hostkeeper_steps::record::Progress::activate`]), and [`activate_pending`] does it:
//!
//! the installer's `activate` ([`crate::repository::Repository::activate_installed`]:
//! `apt-get install hostkeeper=<V>`, the hold, the `run` units' enablement links,
//! `daemon-reload`, the record) and `current`. The running daemon is not restarted: its next
//! start runs `/usr/bin/hostkeeper`, which execs `current` of the binary directory, this binary.
//! A version with no `target.json` is none this daemon can activate: it ends the update
//! `Failed` ([`REASON_RELEASE_UNREADABLE`]).
//!
//! The `activate` ends the record: `Succeeded` (`drift_repaired` when the package was found at a
//! version hostkeeper did not set), or `Failed` after the commit with the installer's class and
//! reason (`dpkg_error`), the record of the package then written whole again so that the
//! heartbeat reports the drift; the binary keeps running either way, and is not rolled back. An
//! `activate` that defers (`package_manager_busy`), or whose host files failed, stays pending and
//! is tried again: by the start's retry every [`DEFER_SECS`] (`crate::start`), and by every order
//! for that version, which runs it again at once and is answered with how it went (`Deferred`
//! [`REASON_ACTIVATE_PENDING`] while it still defers; for an install of the version that runs,
//! which needs no restart, the coordinator's next order is the retry). Any other order is `busy`
//! meanwhile. A version that cannot be read ends it `Failed` ([`REASON_RELEASE_UNREADABLE`]).
//!
//! **The adoption** ([`adopt`]). A host set up from the `.deb` holds `hostkeeper` in its package
//! database with no record. A daemon that announces `apt` and finds, at its start, its own
//! package installed whole at the version it runs, no record of hostkeeper's installer and no
//! version of hostkeeper in the packages directory, holds the package and records it as
//! installed by `apt`. It is not drift, and nothing but hostkeeper replaces `/usr/bin/hostkeeper`
//! from then on. A Mac adopts nothing: its bootstrap install (the `.pkg` its MDM or operator
//! installed) has a receipt and no record, so the first update of hostkeeper through
//! `macos-pkg` finds it as drift, installs over it and ends `Succeeded`, `drift_repaired`.

use std::io;
use std::path::Path;

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_hook::Class;
use hostkeeper_install::repository::apt::Apt;
use hostkeeper_install::repository::{DRIFT_REPAIRED, Failure, Outcome, Reason};
use hostkeeper_placed::PlacedRecord;
use hostkeeper_proto::hostkeeper::release::v1::Target;
use hostkeeper_release::Installer;
use hostkeeper_steps::record::{Activated, Loaded};

use crate::executor::Ctx;
use crate::installed::{self, CURRENT};
pub use crate::repository::DEFER_SECS;
use crate::repository::native_of_entry;
use crate::target;
use crate::verbs::force::SELF_SERVICE;

/// The reason of the `Deferred` an order for hostkeeper's version gets while the `activate` of
/// its package waits.
pub const REASON_ACTIVATE_PENDING: &str = "activate_pending";
/// The alert of an `activate` of hostkeeper's own package that deferred, failed, or could not
/// be recorded, and of an adoption that could not be done.
pub const ALERT_OWN_PACKAGE: &str = "own_package";
/// The package hostkeeper's own `.deb` installs.
pub const OWN_PACKAGE: &str = "hostkeeper";

/// Whether an update of hostkeeper itself to a version staged as `staged` (its `target.json`,
/// `None` for a tree this daemon did not stage) leaves its switch to the new binary: a native
/// version, or a host where `record` names an installer of hostkeeper (it runs from its native package).
pub fn deferred(staged: Option<&Target>, record: &PlacedRecord) -> bool {
    staged.is_some() | record.installer_of(SELF_SERVICE).is_some()
}

/// [`deferred`] for the version directory `dir` of hostkeeper on the host of `ctx`.
pub(crate) fn deferred_at(ctx: &Ctx, dir: &Path) -> Result<bool, String> {
    let staged = target::read(dir).map_err(|e| e.to_string())?;
    let record = ctx.placer().store().load().map_err(|e| e.to_string())?;
    Ok(deferred(staged.as_ref(), &record))
}

/// Whether the binary of version `running`, confirming itself, has its own package still to
/// activate: its version is staged in the packages directory, its switch was left to it
/// ([`deferred`]), and `current` there does not name it yet.
pub(crate) fn needs_activate(ctx: &Ctx, running: &str) -> io::Result<bool> {
    let dir = ctx.dirs.packages.join(SELF_SERVICE).join(running);
    if !dir.is_dir() {
        return Ok(false);
    }
    let left = deferred_at(ctx, &dir).map_err(io::Error::other)?;
    let current = ctx
        .placer()
        .current(SELF_SERVICE)
        .map_err(io::Error::other)?;
    Ok(left & (current.as_deref() != Some(running)))
}

/// How an `activate` of hostkeeper's own package went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnActivate {
    /// It ended, and the record says so.
    Ended(Activated),
    /// It is to be tried again: the package manager was busy (`reason`), or the host's files
    /// failed (`io_error`). The record still waits for it.
    Deferred { reason: String, detail: String },
}

/// The `activate` of hostkeeper's own package (`service`) the step record waits for, done and
/// recorded; `None` when the record waits for none (or cannot be read: the order path and the
/// start say why).
pub(crate) fn activate_pending(ctx: &Ctx, service: &ServiceName) -> Option<OwnActivate> {
    let store = ctx.store(service);
    let Ok(Loaded::Current(rec)) = store.load() else {
        return None;
    };
    let pending = rec.progress.activate?;
    let ended = activate(ctx, service, &pending.version);
    let alert =
        |detail: &str| ctx.alert(ALERT_OWN_PACKAGE, &format!("service={service}: {detail}"));
    match &ended {
        OwnActivate::Ended(activated) => {
            if matches!(activated, Activated::Failed { .. })
                && let Err(error) = ctx.repository().unpend(service)
            {
                alert(&format!("the record of the failed activate: {error}"));
            }
            if let Err(error) = store.settle_activate(activated.clone()) {
                alert(&format!(
                    "the end of the activate was not recorded: {error}"
                ));
            }
            if let Activated::Failed { reason, .. } = activated {
                alert(&format!(
                    "the activate of {} failed: {reason}",
                    pending.version
                ));
            }
        }
        OwnActivate::Deferred { reason, detail } => alert(&format!(
            "the activate of {} is tried again in {DEFER_SECS}s: {reason}: {detail}",
            pending.version
        )),
    }
    Some(ended)
}

/// One try of the `activate` of `version` of hostkeeper's own package. A version that cannot be
/// read as one, whose `target.json` or manifest does not read, or staged for an installer this
/// daemon has no steps of, will not read on any try: it ends the update `Failed`, of class
/// release, [`REASON_RELEASE_UNREADABLE`], and orders of hostkeeper run again.
fn activate(ctx: &Ctx, service: &ServiceName, version: &str) -> OwnActivate {
    let version = match Version::new(version) {
        Ok(version) => version,
        Err(_) => return unreadable(),
    };
    let dir = ctx
        .dirs
        .packages
        .join(service.as_str())
        .join(version.as_str());
    match target::read(&dir) {
        Ok(Some(entry)) if native_of_entry(&entry).is_some() => {
            native(ctx, service, &version, &dir, &entry)
        }
        Ok(Some(_) | None) | Err(_) => unreadable(),
    }
}

/// The reason of an `activate` of hostkeeper's own package whose version cannot be read.
pub const REASON_RELEASE_UNREADABLE: &str = "release_unreadable";

/// The end of an `activate` whose version cannot be read.
fn unreadable() -> OwnActivate {
    OwnActivate::Ended(Activated::Failed {
        class: Class::Release,
        reason: REASON_RELEASE_UNREADABLE.to_owned(),
    })
}

/// The `activate` of a native version: the installer's.
fn native(
    ctx: &Ctx,
    service: &ServiceName,
    version: &Version,
    dir: &Path,
    entry: &Target,
) -> OwnActivate {
    let repository = ctx.repository();
    drop(repository.take_repaired(service));
    match repository.activate_installed(service, version, dir, entry) {
        Ok(()) => OwnActivate::Ended(Activated::Succeeded {
            reason: repository
                .take_repaired(service)
                .map(|_| DRIFT_REPAIRED.to_owned()),
        }),
        Err(failure) => of_failure(&failure),
    }
}

/// What a failed `activate` of hostkeeper's own package is: a failure of the host's files, and
/// one the installer defers, are tried again; any other ends the update `Failed`.
fn of_failure(failure: &Failure) -> OwnActivate {
    let reason = failure.reason();
    match (reason, reason.outcome()) {
        (Reason::IoError, _) | (_, Outcome::Deferred) => OwnActivate::Deferred {
            reason: reason.token().to_owned(),
            detail: failure.detail().to_owned(),
        },
        (_, Outcome::Failed) => OwnActivate::Ended(Activated::Failed {
            class: match reason.class() {
                hostkeeper_install::repository::Class::Release => Class::Release,
                hostkeeper_install::repository::Class::Environment => Class::Environment,
            },
            reason: reason.token().to_owned(),
        }),
    }
}

/// The adoption of hostkeeper's own package at the start: the version adopted, `None` when there
/// is nothing to adopt (the host does not announce `apt`, hostkeeper's installer is recorded,
/// a version of hostkeeper is in the packages directory, or the package is not installed whole
/// at the version that runs).
pub(crate) fn adopt(ctx: &Ctx) -> Result<Option<String>, String> {
    if !ctx.installers().contains(&Installer::Apt) {
        return Ok(None);
    }
    let record = ctx.placer().store().load().map_err(|e| e.to_string())?;
    if record.installer_of(SELF_SERVICE).is_some() {
        return Ok(None);
    }
    let service = ServiceName::new(SELF_SERVICE).map_err(|e| e.to_string())?;
    if has_version(&ctx.dirs.packages.join(SELF_SERVICE)).map_err(|e| e.to_string())?
        | installed::linked_version(&ctx.dirs.packages, &service, CURRENT).is_some()
    {
        return Ok(None);
    }
    let own = ctx.own_version.to_string();
    let accept = |found: &str| at_version(found, &own);
    ctx.repository()
        .adopt(&Apt, &service, OWN_PACKAGE, &accept)
        .map_err(|failure| failure.to_string())
}

/// Whether the package version `found` is the release `own`: the same text, or it with a
/// Debian revision (`<own>-<revision>`).
pub fn at_version(found: &str, own: &str) -> bool {
    found
        .strip_prefix(own)
        .is_some_and(|rest| rest.is_empty() | rest.starts_with('-'))
}

/// Whether the service directory `dir` of the packages directory holds a version: any entry but
/// its links, seals and the leftovers of a stage (names that start with `.`).
fn has_version(dir: &Path) -> io::Result<bool> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let name = entry?.file_name();
        let name = name.to_string_lossy();
        let version = !(name.starts_with('.')
            | name.ends_with(".seal")
            | (name == CURRENT)
            | (name == installed::PREVIOUS));
        if version {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
#[path = "own_package_tests.rs"]
mod tests;
