//! The pin of a service (docs/design/hostkeeper-design.md, Q14): the file `pins/<service>` beside
//! the state directory, `/var/lib/updater/pins/<service>` with the default `--state-dir`, which
//! names the one version of the service the host keeps whatever its age.
//!
//! The file holds the version and nothing else, with one newline after it or none. No file is no
//! pin. A file that cannot be read, or holds anything but a version, is an error: what it holds
//! may name a version to keep, so the prune that reads it ([`crate::prune`]) deletes nothing.
//! This daemon reads the pin and never writes it.

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use hostkeeper_config::names::{ServiceName, Version};

/// The directory of the pins, beside the state directory.
pub const PINS_DIR: &str = "pins";

/// The most bytes of a pin that are read: more than the longest version and its newline.
const MAX_PIN_BYTES: u64 = 1024;

/// Why the pin of a service could not be read.
#[derive(Debug, thiserror::Error)]
pub enum PinError {
    /// The file is there and could not be read.
    #[error("the pin {} cannot be read: {source}", path.display())]
    Read { path: PathBuf, source: io::Error },
    /// The file holds something other than a version and at most one newline.
    #[error("the pin {} does not hold a version", path.display())]
    Malformed { path: PathBuf },
}

/// The path of the pin of `service`, for the state directory `state_dir`: `pins/<service>`
/// beside it.
pub fn path(state_dir: &Path, service: &ServiceName) -> PathBuf {
    state_dir.with_file_name(PINS_DIR).join(service.as_str())
}

/// The version the pin at `path` names; `None` when there is no file.
pub fn read(path: &Path) -> Result<Option<Version>, PinError> {
    let failed = |source| PinError::Read {
        path: path.to_owned(),
        source,
    };
    let file = match File::open(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        opened => opened.map_err(failed)?,
    };
    let mut bytes = Vec::new();
    file.take(MAX_PIN_BYTES)
        .read_to_end(&mut bytes)
        .map_err(failed)?;
    let text = String::from_utf8(bytes).unwrap_or_default();
    let version = text.strip_suffix('\n').unwrap_or(&text);
    match Version::new(version) {
        Ok(version) => Ok(Some(version)),
        Err(_) => Err(PinError::Malformed {
            path: path.to_owned(),
        }),
    }
}
