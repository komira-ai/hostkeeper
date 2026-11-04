//! The publish: the seal into place, then the tree, each durable before the next, so a version
//! directory never exists without its seal. A seal without its version (a crash between the two
//! renames) is an orphan, removed by [`crate::remove_orphans`].

use std::path::Path;

use hostkeeper_steps::Seal;

use crate::error::UnpackError;
use crate::fs::PackageFs;
use crate::seal;

/// Publishes the staging directory `stage` of `service` as `version`, sealed with `seal`:
///
/// 1. write `.<version>.seal.tmp` (mode 0444) and fsync it;
/// 2. rename it to `<version>.seal`; fsync the service directory;
/// 3. rename `stage` to `<version>`; fsync the service directory.
pub(crate) fn publish(
    fs: &dyn PackageFs,
    service: &Path,
    stage: &Path,
    version: &str,
    seal: &Seal,
) -> Result<(), UnpackError> {
    let io = |e| UnpackError::of_io("cannot publish the version", e);
    seal::write(fs, service, version, seal)?;
    let placed = seal::placed(service, version);
    fs.rename(&seal::temporary(service, version), &placed)
        .map_err(io)?;
    fs.sync_dir(service).map_err(io)?;
    fs.rename(stage, &service.join(version)).map_err(io)?;
    fs.sync_dir(service).map_err(io)
}
