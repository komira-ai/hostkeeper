//! What the host has installed, read from the package directory (design section 4.7):
//! `<packages-dir>/<service>/<version>/` per version, with the links `current` (the version the
//! host runs) and `previous`.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

use hostkeeper_config::names::{ServiceName, Version};

/// The link to the version a service runs.
pub const CURRENT: &str = "current";
/// The link to the version it ran before.
pub const PREVIOUS: &str = "previous";

/// The version the link `link` of `service` names: the last component of its target. `None`
/// when the link is missing or its target's last component is not a version.
pub fn linked_version(packages_dir: &Path, service: &ServiceName, link: &str) -> Option<Version> {
    let target = fs::read_link(packages_dir.join(service.as_str()).join(link)).ok()?;
    Version::new(target.file_name()?.to_str()?).ok()
}

/// The directories under `dir` whose names are service names, by name; nothing when `dir` is
/// missing. Anything else there (the lock file, a stray name) is skipped.
pub fn service_dirs(dir: &Path) -> io::Result<BTreeMap<String, ServiceName>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(e),
    };
    let mut out = BTreeMap::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let service = name.to_str().and_then(|name| ServiceName::new(name).ok());
        let is_dir = entry.file_type()?.is_dir();
        out.extend(service.filter(|_| is_dir).map(|s| (s.to_string(), s)));
    }
    Ok(out)
}
