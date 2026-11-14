//! The adoption of a package the host's package manager already has when an `install` order
//! names it (docs/design/packages-lifecycle.md section 6.1, "A package that is already
//! installed is adopted").
//!
//! A host whose package was installed by hand (`dpkg -i`, a package set up with the host) has the
//! package installed and no record of it in `placed.json`, so the coordinator orders its first
//! install. Hostkeeper did not install that package, so a failed install does not remove it: instead of a first
//! install, whose rollback purges the package, the order is an update from the installed version.
//!
//! - [`Repository::adoptable`], in the order's checks and so writing nothing but staged version
//!   directories: the version the package manager shows (the package installed whole), staged from the order's repository as a version of the service
//!   ([`Adoption`]), pinned by the signed index that lists it, and its `target.json` the order's
//!   entry for that package version with no hook command (the built-in steps). An order for the
//!   version that is installed adopts that version, and rolls back to it.
//! - [`Repository::adopt_installed`], once the order is checked and before its step runs: the
//!   package held and recorded as installed by the installer ([`Repository::adopt`]) and `current`
//!   made to name the adopted version, so the order's switch finds a release to replace and puts
//!   it back when the update fails.
//!
//! A package `placed.json` already records for the service from a repository is hostkeeper's
//! own, left by an earlier try of the install, and is not adopted; one it records with no
//! repository is the adoption's own record, left by an adoption that stopped before `current`
//! was made, and is adopted again.
//!
//! A version the repository does not list, or whose name is no version of the service, cannot be
//! staged: the order ends `Failed` with the stage's reason and the package is left as it is.

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_install::repository::backend::Installed;
use hostkeeper_install::repository::launcher::LauncherTools;
use hostkeeper_install::repository::{Backend, Failure, InstallerLock, Order, Reason, Records};
use hostkeeper_proto::hostkeeper::release::v1::Target;
use hostkeeper_steps::Staged;

use super::{DRIFT_QUERY_SECS, Native, Repository};
use crate::executor::Reply;
use crate::refusal::Refusal;

/// The installed package an `install` order adopts, staged as a version of its service.
pub(crate) struct Adoption {
    /// The package.
    package: String,
    /// The version the package manager shows, as it spells it.
    package_version: String,
    /// The version of the service the installed package is.
    pub(crate) version: Version,
    /// Its sealed version directory.
    pub(crate) staged: Staged,
}

impl Repository {
    /// The package of `entry`, when the host's package manager shows it installed whole, staged as the version of `service` it is; `None` for a package
    /// that is not installed (a first install), for one `placed.json` records for the service from a repository, for an entry that is not a repository entry's, and
    /// for an order whose own checks fail (the stage reports them). `target` is the order's
    /// staged version directory: the version installed may be the order's.
    pub(crate) fn adoptable(
        &self,
        backend: &'static dyn Backend,
        service: &ServiceName,
        version: &Version,
        entry: &Target,
        target: &Staged,
    ) -> Result<Option<Adoption>, Reply> {
        let Ok(order) = self.order(backend, service, version, entry) else {
            return Ok(None);
        };
        let found = self
            .installed_unrecorded(backend, service, &order.package)
            .map_err(|failure| self.stage_failed(service, &failure))?;
        // A package `placed.json` records for the service from a repository is one hostkeeper
        // installed, an earlier try of this install whose rollback did not take it off: its own
        // to remove.
        let Some(installed) = found else {
            return Ok(None);
        };
        let package = order.package.clone();
        let adopt = |version: Version, staged: Staged| Adoption {
            package: package.clone(),
            package_version: installed.version.clone(),
            version,
            staged,
        };
        if installed.version == order.package_version {
            return Ok(Some(adopt(version.clone(), target.clone())));
        }
        let adopted = Version::new(&installed.version).map_err(|e| {
            self.stage_failed(
                service,
                &Failure::new(
                    Reason::ManifestInvalid,
                    format!(
                        "the installed {} {} is no version of a service: {e}",
                        package, installed.version
                    ),
                ),
            )
        })?;
        let mut sealed_for = entry.clone();
        sealed_for.package_version = Some(installed.version.clone());
        // An apt entry's manifest always names a hook (the order's checks refuse one without);
        // the adopted version runs the built-in steps, so its command is empty.
        sealed_for
            .manifest
            .iter_mut()
            .filter_map(|manifest| manifest.hook.as_mut())
            .for_each(|hook| hook.command.clear());
        // The installed version of the order's own package, asked of the index for its digest: the
        // order's pin names another version and is not read.
        let pinning = Order {
            version: adopted.clone(),
            package_version: installed.version.clone(),
            pin: String::new(),
            ..order
        };
        sealed_for.digest = self
            .with(backend, service, |installer| installer.pin_of(&pinning))
            .map_err(|failure| self.stage_failed(service, &failure))?;
        let staged = self.stage(Native::Repository(backend), service, &adopted, &sealed_for)?;
        Ok(Some(adopt(adopted, staged)))
    }

    /// The adoption of `adoption`: its package held and recorded as installed by the installer
    /// for `service`, then `current` made to name its version.
    pub(crate) fn adopt_installed(
        &self,
        backend: &'static dyn Backend,
        service: &ServiceName,
        adoption: &Adoption,
    ) -> Result<(), Reply> {
        let adopted = self
            .adopt(backend, service, &adoption.package, &|found| {
                found == adoption.package_version
            })
            .map_err(|failure| self.stage_failed(service, &failure))?;
        if adopted.is_none() {
            let failure = Failure::new(
                Reason::DpkgError,
                format!(
                    "{} was no longer installed at {} when it was adopted",
                    adoption.package, adoption.package_version
                ),
            );
            return Err(self.stage_failed(service, &failure));
        }
        self.placer()
            .switch_current(service, &adoption.version)
            .map(drop)
            .map_err(|e| Reply::refused(Refusal::LinkFailed, e))
    }

    /// What the package manager shows of `package`, installed whole, under the installer lock,
    /// when `placed.json` records no package for `service`; otherwise `None`.
    fn installed_unrecorded(
        &self,
        backend: &dyn Backend,
        service: &ServiceName,
        package: &str,
    ) -> Result<Option<Installed>, Failure> {
        let io = |e: &dyn std::fmt::Display| Failure::new(Reason::IoError, e.to_string());
        let _held = InstallerLock::new(&self.ports.lock)
            .hold()
            .map_err(|e| io(&e))?;
        let tools = LauncherTools::sharing(&*self.launcher, service.as_str(), &self.tool_runs);
        let found = backend
            .installed(&tools, package, DRIFT_QUERY_SECS)?
            .filter(|found| found.complete);
        // Read only for a package that is installed, so a first install reads nothing more.
        let recorded = match found {
            Some(_) => self.records().load(service).map_err(|e| io(&e))?,
            None => None,
        };
        // An entry with no repository is the adoption's own (below): an adoption that stopped
        // before `current` was made is done again.
        let own = recorded.is_some_and(|record| !record.repository.is_empty());
        Ok(found.filter(|_| !own))
    }
}
