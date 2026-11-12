//! `activate` (packages-repositories.md sections 4.3 and 4.4): the install of a staged
//! version from its own copy of the index and the package, with no fetch. An update, a rollback
//! (the kept previous version's directory) and a repair of drift are all this one step.

use std::collections::BTreeSet;
use std::path::Path;

use super::enable;
use super::outcome::{Failure, Reason};
use super::policy;
use super::record::PackageRecord;
use super::stage::TREE;
use super::tools::{Run, Tool};
use super::{Activated, Drift, Order, RepositoryInstaller, io_failure, record_failure};

impl RepositoryInstaller<'_> {
    /// Makes the version of `order`, staged in `dir`, the installed one. Idempotent: run again
    /// after a crash, it finds the version installed and does the hold and the enablement again.
    pub fn activate(&self, order: &Order, dir: &Path) -> Result<Activated, Failure> {
        let (backend, tools, bound) = (self.backend, self.tools, order.bounds.activate);
        let (package, version) = (order.package.as_str(), order.package_version.as_str());
        let _held = self
            .lock
            .hold()
            .map_err(|e| io_failure("the installer lock", e))?;
        let record = self.records.load(&order.service).map_err(record_failure)?;
        let installed = backend.installed(tools, package, bound)?;
        let drift = drift_of(record.as_ref(), installed.as_ref().map(|i| &i.version));
        let work = self.fresh_work(&order.service)?;
        backend
            .restore(&dir.join(backend.meta_dir()), &work)
            .map_err(|e| io_failure("the version's copy of the index", e))?;
        let wanted = enable::wanted(&dir.join(TREE), &order.run)
            .map_err(|e| io_failure("the units' unit files", e))?;
        // The pending record claims the links this run is about to write, so a run found
        // pending after a crash owns the links the crashed run wrote; a link already there is
        // not claimed.
        let mut links = links_of(record.as_ref());
        links.extend(enable::missing(&self.dirs.root, &wanted));
        let mut next = PackageRecord {
            installer: backend.installer().as_str().to_owned(),
            package: package.to_owned(),
            version: version.to_owned(),
            repository: order.repository.clone(),
            links,
            pending: true,
        };
        self.records
            .save(&order.service, &next)
            .map_err(record_failure)?;
        {
            // The policy file is in place for the install alone, and out again before the lock
            // is let go.
            let _policy = policy::Guard::hold(&self.dirs.root)
                .map_err(|e| io_failure("the policy-rc.d file", e))?;
            backend.install(tools, &work, package, version, bound, self.lock_wait_secs)?;
        }
        match backend.installed(tools, package, bound)? {
            Some(found) if found.complete & (found.version == version) => {}
            found => {
                let shown = found.map_or("no version".to_owned(), |f| {
                    format!(
                        "{} ({})",
                        f.version,
                        if f.complete {
                            "installed"
                        } else {
                            "not fully installed"
                        }
                    )
                });
                return Err(Failure::new(
                    Reason::DpkgError,
                    format!(
                        "after the install the database shows {package} at {shown}, not {version}"
                    ),
                ));
            }
        }
        self.hold_waiting(package, true, bound)?;
        next.links = enable::apply(&self.dirs.root, &wanted, &next.links)
            .map_err(|e| io_failure("the units' enablement links", e))?;
        self.reload(bound)?;
        next.pending = false;
        self.records
            .save(&order.service, &next)
            .map_err(record_failure)?;
        Ok(Activated { drift })
    }

    /// Reloads the service manager after the units or their links changed.
    pub(super) fn reload(&self, bound: u64) -> Result<(), Failure> {
        let run = Run::offline(Tool::Systemctl, vec!["daemon-reload".to_owned()], bound);
        let output = self
            .tools
            .run(&run)
            .map_err(|e| io_failure("systemctl daemon-reload", e))?;
        match output.succeeded() {
            true => Ok(()),
            false => Err(Failure::new(
                Reason::IoError,
                format!("systemctl daemon-reload failed: {}", output.last_line()),
            )),
        }
    }
}

/// The drift between what the record names and what the database shows, if any (section 4.4).
/// A pending record names a version being installed, which is not drift: the install it belongs
/// to is being run again.
pub(super) fn drift_of(record: Option<&PackageRecord>, found: Option<&String>) -> Option<Drift> {
    if record.is_some_and(|r| r.pending) {
        return None;
    }
    let recorded = record.map(|r| r.version.clone());
    let found = found.cloned();
    (recorded != found).then_some(Drift { found, recorded })
}

/// The links of `record`, none without one.
pub(super) fn links_of(record: Option<&PackageRecord>) -> BTreeSet<String> {
    record.map(|r| r.links.clone()).unwrap_or_default()
}
