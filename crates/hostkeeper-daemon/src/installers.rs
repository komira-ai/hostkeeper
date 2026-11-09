//! The installer of each installed service (packages.md section 5.1): the daemon finds a
//! service's installer in `placed.json` (`installed`) and the backend that runs its steps in the
//! registry ([`hostkeeper_install::backend_of`]); a repository installer's service (`apt`) is
//! recorded with its package ([`crate::repository::PlacedRecords`]).
//!
//! A service with no entry has no installer recorded: a package manager owns no file of it, and
//! nothing but its package directory and records is to be taken away. A service recorded with an
//! installer this build has no backend of (one a newer binary installed before a fallback,
//! packages.md section 9.1) is refused with what is recorded: the daemon reports it and leaves
//! it as it is.

use hostkeeper_config::names::ServiceName;
use hostkeeper_install::backend_of;
use hostkeeper_install::repository::Backend;
use hostkeeper_placed::PlacedRecord;
use hostkeeper_release::Installer;

/// The installer `record` holds for `service`: `None` for a service with none recorded, `Err`
/// for one recorded under a name that is no installer.
pub fn installer_of(
    record: &PlacedRecord,
    service: &ServiceName,
) -> Result<Option<Installer>, String> {
    record
        .installer_of(service.as_str())
        .map(|name| {
            name.parse::<Installer>()
                .map_err(|_| unknown(service, name))
        })
        .transpose()
}

/// The backend of the installer `record` holds for `service` ([`installer_of`]): `None` for a
/// service with no installer recorded, `Err` for one whose installer this daemon has no backend
/// of.
pub fn backend_for(
    record: &PlacedRecord,
    service: &ServiceName,
) -> Result<Option<&'static dyn Backend>, String> {
    installer_of(record, service)?
        .map(|installer| backend_of(installer).ok_or_else(|| unknown(service, installer.as_str())))
        .transpose()
}

fn unknown(service: &ServiceName, name: &str) -> String {
    format!("{service} is installed by the installer {name:?}, which this daemon has no steps of")
}

#[cfg(test)]
#[path = "installers_tests.rs"]
mod tests;
