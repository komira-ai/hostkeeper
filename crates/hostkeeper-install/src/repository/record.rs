//! What the repository installer records of a service: its `installed` entry of `placed.json`
//! (packages-lifecycle.md section 6.5, packages-repositories.md section 4.3), behind a port the
//! daemon gives it.

use std::collections::BTreeSet;

use hostkeeper_config::names::ServiceName;
use serde::{Deserialize, Serialize};

/// A service's `installed` entry for a repository package.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageRecord {
    /// The installer's name, the backend's (`apt`).
    pub installer: String,
    /// The package, as the package manager knows it.
    pub package: String,
    /// The version installed, or being installed while `pending`.
    pub version: String,
    /// The repository the version came from.
    pub repository: String,
    /// The enablement links of the `run` units hostkeeper wrote, each `<link>` path the
    /// service manager reads, absolute.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub links: BTreeSet<String>,
    /// Written before the install, cleared once the package manager shows the version: a crash
    /// between the two runs `activate` again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pending: bool,
}

/// Why the record could not be read or written.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the installed record: {0}")]
pub struct RecordError(pub String);

/// The services' records, as `placed.json` keeps them.
pub trait Records: Send + Sync {
    /// The record of `service`, if any.
    fn load(&self, service: &ServiceName) -> Result<Option<PackageRecord>, RecordError>;
    /// Writes the record of `service`, durably before it returns.
    fn save(&self, service: &ServiceName, record: &PackageRecord) -> Result<(), RecordError>;
    /// Drops the record of `service`, durably before it returns.
    fn clear(&self, service: &ServiceName) -> Result<(), RecordError>;
    /// The service other than `service` whose record names `package`, if any.
    fn holder(
        &self,
        package: &str,
        service: &ServiceName,
    ) -> Result<Option<ServiceName>, RecordError>;
}
