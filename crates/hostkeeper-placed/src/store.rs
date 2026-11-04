//! `placed.json`: which service owns each placed path (design section 10, "Placed-path ownership
//! record").
//!
//! ```json
//! {
//!   "schema": 1,
//!   "paths": {
//!     "/etc/tierstore/tierstore.conf": {
//!       "service": "tierstore",
//!       "version": "1.5.0",
//!       "kind": "file",
//!       "state": "placed",
//!       "link_target": "/var/lib/hostkeeper/packages/tierstore/current/files/tierstore.conf"
//!     }
//!   },
//!   "uninstalling": { "other": {} }
//! }
//! ```
//!
//! Schema 1, changed additively only: a key this daemon does not know, at the top level, in an
//! entry or in a tombstone, is kept as it is when the record is written again. A record with a
//! newer schema, or one that does not parse, is an error naming the file, and the file is left
//! as it is. Every write is a temporary file in the same directory, an `fsync` of the file, a
//! rename and an `fsync` of the directory.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{PlacedError, io_at};
use crate::fs::Fs;
use crate::roots::PLACED_FILE;

/// The schema this daemon reads and writes.
pub const SCHEMA: u64 = 1;
/// The temporary name `placed.json` is written under, beside it.
pub const TEMPORARY_FILE: &str = ".hk-placed.json.tmp";

/// What a placed path is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathKind {
    /// A file of `units/`, at `<unit dir>/<name>`.
    Unit,
    /// A `[[files]]` entry, at its `path`.
    File,
    /// The `WantedBy=` link of a `run` unit, at `<unit dir>/<target>.wants/<unit>`.
    WantsLink,
}

/// Where a placed path is in the write-ahead protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathState {
    /// Recorded before its link is made; the link may or may not be there yet.
    Pending,
    /// Linked and confirmed.
    Placed,
}

/// One placed path.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub service: String,
    pub version: String,
    pub kind: PathKind,
    pub state: PathState,
    /// The target the link at the path has: a path into `<packages>/<service>/current/`, or for
    /// a `wants_link` the placed unit.
    pub link_target: PathBuf,
    /// The keys of a newer writer, kept on rewrite.
    #[serde(flatten)]
    extra: Map<String, Value>,
}

impl Entry {
    pub fn new(
        service: &str,
        version: &str,
        kind: PathKind,
        state: PathState,
        link_target: &Path,
    ) -> Entry {
        Entry {
            service: service.to_owned(),
            version: version.to_owned(),
            kind,
            state,
            link_target: link_target.to_path_buf(),
            extra: Map::new(),
        }
    }

    /// Takes every field of `wanted` but the keys of a newer writer, which are kept.
    pub(crate) fn assign(&mut self, wanted: Entry) {
        let extra = std::mem::take(&mut self.extra);
        *self = Entry { extra, ..wanted };
    }

    /// Whether `other` records the same link of the same version, whatever the state of each.
    pub fn same_link(&self, other: &Entry) -> bool {
        (self.service == other.service)
            & (self.version == other.version)
            & (self.kind == other.kind)
            & (self.link_target == other.link_target)
    }
}

/// The tombstone of a service being uninstalled (design section 4.7, "Uninstall", step 1). Its
/// paths are not linked while it stands.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Tombstone {
    /// The keys of a newer writer, kept on rewrite.
    #[serde(flatten)]
    extra: Map<String, Value>,
}

/// The contents of `placed.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlacedRecord {
    pub schema: u64,
    /// Each placed path, by its absolute system path.
    #[serde(default)]
    pub paths: BTreeMap<PathBuf, Entry>,
    /// The tombstones, by service.
    #[serde(default)]
    pub uninstalling: BTreeMap<String, Tombstone>,
    /// The keys of a newer writer, kept on rewrite.
    #[serde(flatten)]
    extra: Map<String, Value>,
}

impl Default for PlacedRecord {
    fn default() -> PlacedRecord {
        PlacedRecord {
            schema: SCHEMA,
            paths: BTreeMap::new(),
            uninstalling: BTreeMap::new(),
            extra: Map::new(),
        }
    }
}

impl PlacedRecord {
    /// The paths of `service`, with their entries.
    pub fn of_service<'a>(
        &'a self,
        service: &'a str,
    ) -> impl Iterator<Item = (&'a PathBuf, &'a Entry)> + 'a {
        self.paths
            .iter()
            .filter(move |(_, entry)| entry.service == service)
    }
}

/// The error of a record that is not valid JSON of the schema at `path`.
fn corrupt(path: &Path) -> impl FnOnce(serde_json::Error) -> PlacedError {
    let path = path.to_path_buf();
    move |why| PlacedError::Corrupt {
        path,
        why: why.to_string(),
    }
}

/// `placed.json` in a state directory, read and written through an [`Fs`].
pub struct Store<'a> {
    fs: &'a dyn Fs,
    dir: PathBuf,
}

impl<'a> Store<'a> {
    /// The record in the state directory `dir`.
    pub fn new(fs: &'a dyn Fs, dir: &Path) -> Store<'a> {
        Store {
            fs,
            dir: dir.to_path_buf(),
        }
    }

    /// `<dir>/placed.json`.
    pub fn path(&self) -> PathBuf {
        self.dir.join(PLACED_FILE)
    }

    /// Reads the record; a missing file is an empty record.
    pub fn load(&self) -> Result<PlacedRecord, PlacedError> {
        let path = self.path();
        let bytes = match self.fs.read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(PlacedRecord::default()),
            Err(e) => return Err(io_at("read", &path)(e)),
        };
        let value: Value = serde_json::from_slice(&bytes).map_err(corrupt(&path))?;
        let schema = value.get("schema").and_then(Value::as_u64);
        if let Some(schema) = schema.filter(|schema| *schema > SCHEMA) {
            return Err(PlacedError::NewerSchema { path, schema });
        }
        serde_json::from_value(value).map_err(corrupt(&path))
    }

    /// Writes the record: the temporary file, its `fsync`, the rename, the directory's `fsync`.
    pub fn save(&self, record: &PlacedRecord) -> Result<(), PlacedError> {
        let path = self.path();
        let tmp = self.dir.join(TEMPORARY_FILE);
        let mut bytes = serde_json::to_vec_pretty(record).map_err(corrupt(&path))?;
        bytes.push(b'\n');
        self.fs.write(&tmp, &bytes).map_err(io_at("write", &tmp))?;
        self.fs.sync_file(&tmp).map_err(io_at("fsync", &tmp))?;
        self.fs.rename(&tmp, &path).map_err(io_at("rename", &tmp))?;
        self.fs
            .sync_dir(&self.dir)
            .map_err(io_at("fsync", &self.dir))
    }
}
