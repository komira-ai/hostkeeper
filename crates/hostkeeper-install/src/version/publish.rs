//! The publish: the seal into place, then the tree, each durable before the next, so a version
//! directory never exists without its seal. A seal without its version (a crash between the two
//! renames) is an orphan, removed by [`super::remove_orphans`].
//!
//! The tree is renamed in as the staging directory was made, mode 0700, and stays so: what is
//! kept of a package (its file, the files it installs, any secret among them) is read by root
//! alone. Root's own rename is never refused for the mode (macOS refuses to rename a directory
//! its caller cannot write, even to a new name in the same parent, and 0700 is writable by its
//! owner). The top directory's mode is not part of the tree digest, so the seal proves the same
//! tree whatever mode an earlier release gave the directory (0555).

use std::path::Path;

use hostkeeper_steps::Seal;

use super::VERSION_MODE;
use super::error::UnpackError;
use super::fs::PackageFs;
use super::seal;

/// Publishes the staging directory `stage` of `service` as `version`, sealed with `seal`:
///
/// 1. write `.<version>.seal.tmp` (mode 0400) and fsync it;
/// 2. rename it to `<version>.seal`; fsync the service directory;
/// 3. rename `stage` (mode 0700) to `<version>`; fsync the service directory;
/// 4. [`settle`] the version: mode [`VERSION_MODE`], made durable.
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

/// Makes the directory of the published `version` of `service` [`VERSION_MODE`] and the mode
/// durable: the last step of [`publish`], and what a publish that finds the version already
/// sealed for its package does when the directory has another mode (an earlier release's 0555),
/// so that a crash between the rename and this step is finished by the next one.
pub(crate) fn settle(fs: &dyn PackageFs, service: &Path, version: &str) -> Result<(), UnpackError> {
    let io = |e| UnpackError::of_io("cannot close the version to other users", e);
    let dir = service.join(version);
    fs.set_mode(&dir, VERSION_MODE).map_err(io)?;
    fs.sync_dir(&dir).map_err(io)
}
