//! The installer of each installed service (packages.md section 5.1): the daemon finds a
//! service's installer in `placed.json` (`installed`) and the steps that run it in the registry
//! ([`hostkeeper_install::backend_of`] for a repository installer); a repository installer's
//! service (`apt`) is recorded with its package ([`crate::repository::PlacedRecords`]).
//!
//! A service with no entry has no installer recorded: a package manager owns no file of it, and
//! nothing but its package directory and records is to be taken away. A repository installer's
//! steps are its backend's ([`Steps::Repository`]); a `macos-pkg` service's are the `macos-pkg`
//! installer's ([`Steps::MacosPkg`], [`crate::macos_pkg`]). A service recorded with an installer
//! this build has no steps of (one a newer binary installed before a fallback, packages.md
//! section 9.1) is refused with what is recorded: the daemon reports it and leaves it as it is.

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

/// The steps of an installer this daemon has.
#[derive(Clone, Copy)]
pub enum Steps {
    /// A repository installer, whose steps the daemon runs over its backend
    /// ([`crate::repository`]).
    Repository(&'static dyn Backend),
    /// The `macos-pkg` installer, whose steps the daemon runs over its own ports
    /// ([`crate::macos_pkg`]).
    MacosPkg,
}

impl Steps {
    /// The installer the steps are of.
    pub fn installer(self) -> Installer {
        match self {
            Steps::Repository(backend) => backend.installer(),
            Steps::MacosPkg => Installer::MacosPkg,
        }
    }
}

/// The steps of the installer `record` holds for `service` ([`installer_of`]): `None` for a
/// service with no installer recorded, `Err` for one whose installer this daemon has no steps of.
pub fn steps_for(record: &PlacedRecord, service: &ServiceName) -> Result<Option<Steps>, String> {
    installer_of(record, service)?
        .map(|installer| {
            backend_of(installer)
                .map(Steps::Repository)
                .or_else(|| (installer == Installer::MacosPkg).then_some(Steps::MacosPkg))
                .ok_or_else(|| unknown(service, installer.as_str()))
        })
        .transpose()
}

fn unknown(service: &ServiceName, name: &str) -> String {
    format!("{service} is installed by the installer {name:?}, which this daemon has no steps of")
}

#[cfg(test)]
#[path = "installers_tests.rs"]
mod tests;
