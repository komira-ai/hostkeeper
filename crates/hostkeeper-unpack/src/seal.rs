//! The seal of a version: its format is [`hostkeeper_steps::seal`] (the step runner reads it
//! too); this module writes it through the port, under its temporary name.
//!
//! ```json
//! {"schema":1,"tree_digest":"<64 hex>","package_digest":"<64 hex>"}
//! ```
//!
//! Root-owned, mode 0444, at `packages/<service>/<version>.seal`, written as
//! `.<version>.seal.tmp` and renamed into place before the version directory is.

use std::path::{Path, PathBuf};

pub use hostkeeper_steps::seal::{SEAL_SCHEMA, Seal, SealError};

use crate::error::UnpackError;
use crate::fs::PackageFs;

/// The temporary name of the seal of `version` in `service`'s directory.
pub(crate) fn temporary(service: &Path, version: &str) -> PathBuf {
    service.join(format!(".{version}.seal.tmp"))
}

/// The name of the seal of `version` in `service`'s directory.
pub(crate) fn placed(service: &Path, version: &str) -> PathBuf {
    service.join(format!("{version}.seal"))
}

/// Writes `seal` as the temporary seal of `version`, mode 0444, durably.
pub(crate) fn write(
    fs: &dyn PackageFs,
    service: &Path,
    version: &str,
    seal: &Seal,
) -> Result<(), UnpackError> {
    let io = |e| UnpackError::of_io("cannot write the seal", e);
    let mut file = fs.create_file(&temporary(service, version)).map_err(io)?;
    file.write(seal.to_json().as_bytes()).map_err(io)?;
    file.set_mode(0o444).map_err(io)?;
    file.sync().map_err(io)
}
