//! The publish: the seal into place, then the tree, each durable before the next, so a version
//! directory never exists without its seal. A seal without its version (a crash between the two
//! renames) is an orphan, removed by [`super::remove_orphans`].
//!
//! The tree is renamed in while its top directory is still writable by its owner (0700), and made
//! 0555 after: macOS refuses to rename a directory that its caller cannot write, even to a new
//! name in the same parent (it rewrites the directory's `..` entry), so a non-root publish of a
//! tree already 0555 fails there; root passes that check, and Linux makes it only when the
//! parent changes. The top directory's mode is not part of the tree digest, so the seal proves
//! the same tree either way.

use std::path::Path;

use hostkeeper_steps::Seal;

use super::error::UnpackError;
use super::fs::PackageFs;
use super::seal;

/// Publishes the staging directory `stage` of `service` as `version`, sealed with `seal`:
///
/// 1. write `.<version>.seal.tmp` (mode 0444) and fsync it;
/// 2. rename it to `<version>.seal`; fsync the service directory;
/// 3. rename `stage` (mode 0700, the tree below it read-only) to `<version>`; fsync the service
///    directory;
/// 4. [`settle`] the version: mode 0555, made durable.
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
    fs.sync_dir(service).map_err(io)?;
    settle(fs, service, version)
}

/// Makes the directory of the published `version` of `service` 0555 and the mode durable: the
/// last step of [`publish`], and what a publish that finds the version already sealed for its
/// package does, so that a crash between the rename and this step is finished by the next one.
pub(crate) fn settle(fs: &dyn PackageFs, service: &Path, version: &str) -> Result<(), UnpackError> {
    let io = |e| UnpackError::of_io("cannot make the version read-only", e);
    let dir = service.join(version);
    fs.set_mode(&dir, 0o555).map_err(io)?;
    fs.sync_dir(&dir).map_err(io)
}
