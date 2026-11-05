//! The local list of bad versions of the updater itself (design section 6.6): a version whose
//! new binary could not start, and that the fallback put the previous binary back from, is kept
//! here so that the daemon never runs an order for it again, whatever order replaced its step
//! record since.
//!
//! The file is `<state-dir>/<service>/bad-versions`, a JSON object, mode 0600, written durably
//! (temporary file, fsync, rename, directory fsync):
//!
//! ```text
//! {"schema":1,"versions":["1.5.0"]}
//! ```
//!
//! The format is additive (AGENTS.md B8): a key this reader does not know is kept as it is when
//! the file is written again, and a newer `schema` is read for its `versions` all the same. A
//! file that is not such an object is an error naming the file, never read as an empty list. A
//! version is never removed here but by the cap: the list keeps the last [`MAX_BAD_VERSIONS`]
//! versions added, the oldest dropped first.

use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Map, Value};

/// The file's name in the state directory.
pub const BAD_VERSIONS_FILE: &str = "bad-versions";
/// The schema this binary writes.
pub const BAD_VERSIONS_SCHEMA: u32 = 1;
/// The most versions the list keeps.
pub const MAX_BAD_VERSIONS: usize = 64;

/// Why the list could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum BadVersionsError {
    #[error("cannot use the list of bad versions {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The file is there and is not a list of bad versions.
    #[error("{} is not a list of bad versions: {why}", path.display())]
    Garbage { path: PathBuf, why: String },
}

/// The file as it is read: its versions, and every other key kept for the next write.
#[derive(Debug, Default, Deserialize)]
struct Shape {
    versions: Vec<String>,
    #[serde(flatten)]
    rest: Map<String, Value>,
}

/// The local list of bad versions in one state directory.
#[derive(Debug)]
pub struct BadVersions {
    path: PathBuf,
    shape: Shape,
}

impl BadVersions {
    /// The list in the state directory `dir`; empty when the file is not there.
    pub fn load(dir: &Path) -> Result<BadVersions, BadVersionsError> {
        let path = dir.join(BAD_VERSIONS_FILE);
        let shape = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| BadVersionsError::Garbage {
                path: path.clone(),
                why: e.to_string(),
            })?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Shape::default(),
            Err(source) => return Err(BadVersionsError::Io { path, source }),
        };
        Ok(BadVersions { path, shape })
    }

    /// The file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The versions, oldest first.
    pub fn versions(&self) -> &[String] {
        &self.shape.versions
    }

    /// Whether `version` is bad here.
    pub fn contains(&self, version: &str) -> bool {
        self.shape.versions.iter().any(|bad| bad == version)
    }

    /// Adds `version` and writes the list, durably: `false`, and nothing written, when it is
    /// already there. Past [`MAX_BAD_VERSIONS`], the oldest versions are dropped.
    pub fn add(&mut self, version: &str) -> Result<bool, BadVersionsError> {
        if self.contains(version) {
            return Ok(false);
        }
        let versions = &mut self.shape.versions;
        versions.push(version.to_owned());
        let excess = versions.len().saturating_sub(MAX_BAD_VERSIONS);
        versions.drain(..excess);
        let mut object = self.shape.rest.clone();
        // A newer file keeps its schema: this binary changed only `versions` in it.
        object
            .entry("schema")
            .or_insert(Value::from(BAD_VERSIONS_SCHEMA));
        object.insert("versions".to_owned(), Value::from(versions.clone()));
        let text = Value::Object(object).to_string();
        write_private(&self.path, text.as_bytes()).map_err(|source| BadVersionsError::Io {
            path: self.path.clone(),
            source,
        })?;
        Ok(true)
    }
}

/// Writes `bytes` to `path` durably, mode 0600: a temporary file beside it, `<name>.tmp` (its
/// mode set again, whatever a crash left there), fsync, rename, directory fsync.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = dir.join(format!("{name}.tmp"));
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    file.set_permissions(Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&tmp, path)?;
    File::open(dir)?.sync_all()
}
