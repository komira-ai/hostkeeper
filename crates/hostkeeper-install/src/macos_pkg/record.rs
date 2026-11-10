//! What `macos-pkg` records of a service it installed (packages-lifecycle.md section 6.5): the
//! identifier, the version, and `created_dirs`, the directories its installs created, which
//! `remove` may delete once they are empty. The daemon keeps it as the service's `installed` entry
//! of `placed.json`, written before `installer` runs (marked pending) and again once the install
//! is proved; the [`Records`] port is that store.

use std::collections::BTreeSet;

use hostkeeper_config::names::ServiceName;
use serde::{Deserialize, Serialize};

/// The installer's name in a record.
pub const INSTALLER: &str = "macos-pkg";

/// A service's record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PkgRecord {
    /// Always [`INSTALLER`].
    pub installer: String,
    /// The package identifier `pkgutil` knows the receipt by.
    pub identifier: String,
    /// The version last installed (or, while `pending`, being installed).
    pub version: String,
    /// Logical paths of the directories hostkeeper's installs created, which were not on the host
    /// before.
    #[serde(default)]
    pub created_dirs: BTreeSet<String>,
    /// Set while an `activate` runs: the version is not proved installed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pending: bool,
}

impl PkgRecord {
    /// A record of `identifier` at `version`, created no directory yet.
    pub fn new(identifier: &str, version: &str) -> PkgRecord {
        PkgRecord {
            installer: INSTALLER.into(),
            identifier: identifier.into(),
            version: version.into(),
            created_dirs: BTreeSet::new(),
            pending: false,
        }
    }
}

/// Why the record store failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the installed record: {0}")]
pub struct RecordError(pub String);

/// The store of every service's record, durable when `save` returns.
pub trait Records: Send + Sync {
    /// `service`'s record, if it has one.
    fn load(&self, service: &ServiceName) -> Result<Option<PkgRecord>, RecordError>;
    /// Replaces `service`'s record.
    fn save(&self, service: &ServiceName, record: &PkgRecord) -> Result<(), RecordError>;
    /// The service, other than `service`, whose record names `identifier`.
    fn holder(
        &self,
        identifier: &str,
        service: &ServiceName,
    ) -> Result<Option<ServiceName>, RecordError>;
}

#[cfg(test)]
#[path = "record_tests.rs"]
mod tests;
