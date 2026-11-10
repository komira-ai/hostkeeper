//! The start's repair of a unit link an earlier move left (docs/design/packages-native.md,
//! section 10.6): a release that moved a service from its archive to its native package without
//! taking away the archive's unit link left `<unit dir>/<unit>` a link into the service's package
//! directory, which dangles once `current` names the native version and hides the package's unit
//! of the same name, so systemd finds no unit at all.
//!
//! At its start, after the `activate` of its own package and its adoption and before the boot
//! pass starts any unit, the daemon removes each link directly in the unit directory that points
//! into the package directory of a service `placed.json` records with a native installer and
//! whose target is gone, then reloads the service manager
//! ([`hostkeeper_placed::Placer::remove_dangling_unit_links`]), and says what it removed in one
//! log line. A link pointing anywhere else, or whose target is there, is left as it is. A record
//! that cannot be read, or a repair that fails, is the alert [`ALERT_START`], and the start goes
//! on.

use std::collections::BTreeSet;
use std::path::PathBuf;

use hostkeeper_release::Installer;

use crate::boot::ALERT_START;
use crate::executor::Ctx;

/// The dangling unit links the start removed ([the module](self)); an empty list when there was
/// none, or when the repair could not run, which is then an alert.
pub fn repair(ctx: &Ctx) -> Vec<PathBuf> {
    let alert = |detail: String| ctx.alert(ALERT_START, &detail);
    let placer = ctx.placer();
    let record = match placer.store().load() {
        Ok(record) => record,
        Err(error) => {
            alert(format!(
                "placed.json cannot be read, so no dangling unit link is removed: {error}"
            ));
            return Vec::new();
        }
    };
    let native: BTreeSet<String> = record
        .installed
        .iter()
        .filter(|(_, entry)| {
            entry
                .installer
                .parse::<Installer>()
                .is_ok_and(Installer::is_native)
        })
        .map(|(service, _)| service.clone())
        .collect();
    if native.is_empty() {
        return Vec::new();
    }
    match placer.remove_dangling_unit_links(&native) {
        Ok(removed) => {
            if !removed.is_empty() {
                let links: Vec<String> = removed.iter().map(|p| p.display().to_string()).collect();
                ctx.log(&format!(
                    "level=info removed the dangling unit links an earlier move left, then \
                     reloaded the service manager: {}",
                    links.join(" ")
                ));
            }
            removed
        }
        Err(error) => {
            alert(format!(
                "the dangling unit links were not all removed: {error}"
            ));
            Vec::new()
        }
    }
}
