//! The archive version a native service left as `previous` (docs/design/packages-native.md,
//! section 10.7): at its start, after the boot pass, the daemon deletes the version directory of
//! a service it runs natively whose `previous` link names a version with no `target.json`, a
//! `hostkeeper-archive` version the move left (the move's rollback is gone: no archive version
//! is activatable), and nothing else.
//!
//! A service is swept only when everything says the move is over:
//!
//! - `placed.json` records a native installer for it ([`Installer::is_native`]), with no
//!   `pending` install, and holds no tombstone for it;
//! - no order holds its lock, and its step record is absent or ended, with no `activate` of its
//!   package still pending (hostkeeper's own package, whose `current` names its native version
//!   only once its new binary confirmed it);
//! - `current` names a version with a `target.json` and `previous` a different one, a directory
//!   with none.
//!
//! The `previous` link is removed first, so that no link names a version that is being deleted,
//! and then the version's directory and seal ([`hostkeeper_steps::Layout::delete_version`]). A
//! crash in between leaves a version no link names, which the next confirmed switch's prune
//! deletes; a crash inside the deletion leaves a `.stage-prune-*` directory or a seal with no
//! tree, which the sweep of leftovers before the boot pass removes at the next start. A
//! deletion that fails is an alert, and the start goes on.

use std::fs::{self, File};
use std::io;
use std::path::Path;

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_placed::PlacedRecord;
use hostkeeper_release::Installer;
use hostkeeper_steps::Layout;
use hostkeeper_steps::record::Loaded;

use crate::boot::ALERT_START;
use crate::checks;
use crate::executor::Ctx;
use crate::installed::{self, CURRENT, PREVIOUS};
use crate::target;

/// One archive version the start swept, and how it went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchiveVersion {
    /// The service whose `previous` it was.
    pub service: String,
    /// The version.
    pub version: String,
    /// `Ok` once the link and the directory are gone, else what failed.
    pub deleted: Result<(), String>,
}

/// Deletes the archive version `previous` of each native service that is quiet
/// ([the module](self)). Each version found is in the result with how its deletion went; one
/// that failed is also the alert [`ALERT_START`]. A `placed.json` or a package directory that
/// cannot be read is an alert and nothing is swept.
pub fn sweep(ctx: &Ctx) -> Vec<ArchiveVersion> {
    let alert = |detail: String| ctx.alert(ALERT_START, &detail);
    let record = match ctx.placer().store().load() {
        Ok(record) => record,
        Err(error) => {
            alert(format!(
                "placed.json cannot be read, so no archive version is deleted: {error}"
            ));
            return Vec::new();
        }
    };
    let services = match installed::service_dirs(&ctx.dirs.packages) {
        Ok(services) => services,
        Err(error) => {
            alert(format!(
                "the package directory cannot be listed, so no archive version is deleted: {error}"
            ));
            return Vec::new();
        }
    };
    let mut swept = Vec::new();
    for service in services.into_values() {
        let Some(version) = archive_previous(ctx, &record, &service) else {
            continue;
        };
        let deleted = delete(&ctx.dirs.packages, &service, &version);
        if let Err(why) = &deleted {
            alert(format!(
                "service={service}: the archive version {version} was not deleted: {why}"
            ));
        }
        swept.push(ArchiveVersion {
            service: service.to_string(),
            version: version.to_string(),
            deleted,
        });
    }
    swept
}

/// The archive version `previous` names for `service`, when the service is quiet and runs
/// natively ([the module](self)); `None` otherwise.
fn archive_previous(ctx: &Ctx, record: &PlacedRecord, service: &ServiceName) -> Option<Version> {
    let entry = record.installed.get(service.as_str())?;
    let native = entry
        .installer
        .parse::<Installer>()
        .is_ok_and(Installer::is_native);
    let quiet = native
        & !entry.pending
        & !record.uninstalling.contains_key(service.as_str())
        & ctx.locks.holder(service).is_none();
    if !quiet || !record_ended(ctx, service) {
        return None;
    }
    let packages = &ctx.dirs.packages;
    let current = installed::linked_version(packages, service, CURRENT)?;
    let previous = installed::linked_version(packages, service, PREVIOUS)?;
    let dir_of = |version: &Version| packages.join(service.as_str()).join(version.as_str());
    let runs_native = matches!(target::read(&dir_of(&current)), Ok(Some(_)));
    let is_archive =
        dir_of(&previous).is_dir() & matches!(target::read(&dir_of(&previous)), Ok(None));
    (runs_native & is_archive & (previous != current)).then_some(previous)
}

/// Whether the step record of `service` is absent, or ended with no `activate` of its package
/// pending.
fn record_ended(ctx: &Ctx, service: &ServiceName) -> bool {
    match ctx.store(service).load() {
        Ok(Loaded::Absent) => true,
        Ok(Loaded::Current(rec)) => {
            checks::finished_of(&rec).is_some() & rec.progress.activate.is_none()
        }
        Ok(Loaded::Newer { .. }) | Err(_) => false,
    }
}

/// The `previous` link of `service` removed, then the directory and seal of `version`.
fn delete(packages: &Path, service: &ServiceName, version: &Version) -> Result<(), String> {
    let dir = packages.join(service.as_str());
    let text = |error: io::Error| error.to_string();
    fs::remove_file(dir.join(PREVIOUS)).map_err(text)?;
    File::open(&dir)
        .and_then(|dir| dir.sync_all())
        .map_err(text)?;
    Layout::package(&dir)
        .delete_version(version.as_str())
        .map_err(text)
}
