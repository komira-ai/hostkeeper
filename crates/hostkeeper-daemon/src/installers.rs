//! The installer of each installed service, and the host's side of its steps (packages.md
//! section 5.1): the daemon finds a service's installer in `placed.json` (`installed`; a service
//! with no entry is a `hostkeeper-archive` one, as every service installed before the key
//! existed is) and runs its steps through the registry ([`hostkeeper_install::steps_of`]), the
//! placer doing what the steps ask of the host ([`PlacerHost`]).
//!
//! A repository installer's service (`apt`) is recorded with its package
//! ([`crate::repository::PlacedRecords`]), and its steps are the repository installer's over the
//! installer's backend ([`Steps::Repository`]); a `macos-pkg` service's are the `macos-pkg`
//! installer's ([`Steps::MacosPkg`], [`crate::macos_pkg`]). A service recorded with an installer this build
//! has no steps of (one a newer binary installed before a fallback, packages.md section 9.1) is
//! refused with what is recorded: the daemon reports it and leaves it as it is.

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_install::repository::Backend;
use hostkeeper_install::{Host, HostError, InstallerSteps, backend_of, steps_of};
use hostkeeper_placed::{PlacedRecord, Placer};
use hostkeeper_release::Installer;

/// The installer `record` holds for `service`: the one recorded, or `hostkeeper-archive` for a
/// service with none.
pub fn installer_of(record: &PlacedRecord, service: &ServiceName) -> Result<Installer, String> {
    match record.installer_of(service.as_str()) {
        None => Ok(Installer::HostkeeperArchive),
        Some(name) => name
            .parse::<Installer>()
            .map_err(|_| unknown(service, name)),
    }
}

/// The steps of an installer this daemon has.
#[derive(Clone, Copy)]
pub enum Steps {
    /// An artifact installer's four steps.
    Artifact(&'static dyn InstallerSteps),
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
            Steps::Artifact(steps) => steps.installer(),
            Steps::Repository(backend) => backend.installer(),
            Steps::MacosPkg => Installer::MacosPkg,
        }
    }
}

/// The steps of the installer `record` holds for `service` ([`installer_of`]).
pub fn steps_for(record: &PlacedRecord, service: &ServiceName) -> Result<Steps, String> {
    let installer = installer_of(record, service)?;
    steps_of(installer)
        .map(Steps::Artifact)
        .or_else(|| backend_of(installer).map(Steps::Repository))
        .or_else(|| (installer == Installer::MacosPkg).then_some(Steps::MacosPkg))
        .ok_or_else(|| unknown(service, installer.as_str()))
}

fn unknown(service: &ServiceName, name: &str) -> String {
    format!("{service} is installed by the installer {name:?}, which this daemon has no steps of")
}

/// What a [`PlacerHost`] switches with: the runner's switch toward an order's target.
pub type SwitchTo<'a> = &'a dyn Fn(&Version) -> Result<(), String>;

/// The host's side of the steps for one service: `switch` is the switch the caller gives (the
/// runner's switch toward an order's target), `unplace` the placer's removal of every path the
/// service placed ([`Placer::remove_all`]).
pub struct PlacerHost<'a> {
    /// The placer.
    pub placer: &'a Placer<'a>,
    /// The service.
    pub service: &'a ServiceName,
    /// What `switch` does; `None` for a caller that switches nothing (the uninstall).
    pub switch: Option<SwitchTo<'a>>,
}

impl Host for PlacerHost<'_> {
    fn switch(&self, version: &Version) -> Result<(), HostError> {
        let switch = self.switch.ok_or_else(|| {
            HostError(format!(
                "nothing switches {} of {} here",
                version, self.service
            ))
        })?;
        switch(version).map_err(HostError)
    }

    fn unplace(&self) -> Result<(), HostError> {
        self.placer
            .remove_all(self.service)
            .map(drop)
            .map_err(|error| HostError(error.to_string()))
    }
}

#[cfg(test)]
#[path = "installers_tests.rs"]
mod tests;
