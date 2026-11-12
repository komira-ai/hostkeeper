//! `remove` and the observer's drift check (packages-repositories.md section 4.4): the package
//! is released and purged whatever version is installed, and a version hostkeeper did not set
//! is reported, never refused. The release and the purge wait for the package manager's lock
//! ([`super::wait`]).

use hostkeeper_config::names::ServiceName;

use super::activate::{drift_of, links_of};
use super::backend::Purged;
use super::enable;
use super::outcome::Failure;
use super::policy;
use super::tools::Tool;
use super::wait::Try;
use super::{Drift, Removed, RepositoryInstaller, io_failure, record_failure};

impl RepositoryInstaller<'_> {
    /// Takes `service`'s package off the host, as its record names it, each tool run within
    /// `remove_secs` and each wait for the lock within `lock_wait_secs`. Nothing to do without a
    /// record: nothing was installed.
    pub fn remove(&self, service: &ServiceName, remove_secs: u64) -> Result<Removed, Failure> {
        let (backend, tools) = (self.backend, self.tools);
        let _held = self
            .lock
            .hold()
            .map_err(|e| io_failure("the installer lock", e))?;
        let Some(record) = self.records.load(service).map_err(record_failure)? else {
            return Ok(Removed { drift: None });
        };
        let package = record.package.as_str();
        let installed = backend.installed(tools, package, remove_secs)?;
        let drift = drift_of(Some(&record), installed.as_ref().map(|i| &i.version));
        if installed.is_some() {
            self.hold_waiting(package, false, remove_secs)?;
        }
        self.purge_or_hold(package, remove_secs, installed.is_some())?;
        for link in &links_of(Some(&record)) {
            enable::remove_link(&self.dirs.root, link)
                .map_err(|e| io_failure("the units' enablement links", e))?;
        }
        self.reload(remove_secs)?;
        self.records.clear(service).map_err(record_failure)?;
        Ok(Removed { drift })
    }

    /// Purges `package`, tried again while the lock is held. A purge that fails leaves the
    /// package installed, so a package that was held is held again, in one try (a lock still held
    /// is not waited for a second time): nothing but hostkeeper may move it while its removal
    /// waits.
    fn purge_or_hold(&self, package: &str, remove_secs: u64, held: bool) -> Result<(), Failure> {
        let failure = match self.purge(package, remove_secs) {
            Ok(()) => return Ok(()),
            Err(failure) => failure,
        };
        // A database that cannot be read may still hold the package: held again all the same.
        let still = !matches!(
            self.backend.installed(self.tools, package, remove_secs),
            Ok(None)
        );
        if held & still {
            self.backend.hold(self.tools, package, true, remove_secs)?;
        }
        Err(failure)
    }

    /// The purge and its tries (dpkg has no lock timeout of its own).
    fn purge(&self, package: &str, remove_secs: u64) -> Result<(), Failure> {
        let (backend, tools) = (self.backend, self.tools);
        // Held across every try, the pauses between them included.
        let _policy = policy::Guard::hold(&self.dirs.root)
            .map_err(|e| io_failure("the policy-rc.d file", e))?;
        self.waiting(Tool::Dpkg, || {
            Ok(match backend.purge(tools, package, remove_secs)? {
                Purged::Done => Try::Done(()),
                Purged::Busy(line) => Try::Busy(line),
            })
        })
    }

    /// The drift of `service`, whose manifest names `package`, at this heartbeat: what the
    /// database shows against what the record names. `None` while an install is pending.
    pub fn drift(
        &self,
        service: &ServiceName,
        package: &str,
        bound: u64,
    ) -> Result<Option<Drift>, Failure> {
        let record = self.records.load(service).map_err(record_failure)?;
        let package = record.as_ref().map_or(package, |r| r.package.as_str());
        let installed = self.backend.installed(self.tools, package, bound)?;
        Ok(drift_of(
            record.as_ref(),
            installed.as_ref().map(|i| &i.version),
        ))
    }
}
