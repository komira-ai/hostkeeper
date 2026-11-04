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
//! newer schema, one that does not parse, or one whose known fields break their rules (schema 1,
//! every path and link target absolute in the path grammar, services and versions in their
//! grammars), is an error naming the file, and the file is left as it is. `reload_pending`,
//! written only when set, says a `daemon-reload` is owed. A write needs the lock of the file
//! ([`crate::PlacedLock`]). Every write is a temporary file in the same directory, an `fsync` of the file, a
//! rename and an `fsync` of the directory.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use hostkeeper_config::names::{check_path, is_service_name, is_version};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{PlacedError, io_at};
use crate::fs::Fs;
use crate::placer::PlacedLock;
use crate::roots::PLACED_FILE;

/// The schema this daemon reads and writes.
pub const SCHEMA: u64 = 1;
/// The temporary name `placed.json` is written under, beside it. `~` is outside the path
/// grammar, so no placed path can have this name.
pub const TEMPORARY_FILE: &str = ".hk~placed.json~tmp";

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
    /// A `daemon-reload` is owed: set in the same write that records a change to placed links,
    /// cleared only after a reload succeeded, so a crash or a failed reload leaves it due.
    /// Written only when set.
    #[serde(default, skip_serializing_if = "is_false")]
    pub reload_pending: bool,
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
            reload_pending: false,
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

fn is_false(value: &bool) -> bool {
    !*value
}

/// `Err(why())` unless `ok`.
fn require(ok: bool, why: impl FnOnce() -> String) -> Result<(), String> {
    ok.then_some(()).ok_or_else(why)
}

/// Checks that `path` is an absolute path in the path grammar (no `.` or `..` component).
fn require_path(what: &str, path: &Path) -> Result<(), String> {
    let text = path.to_str().unwrap_or_default();
    check_path(text).map_err(|why| format!("{what} {} {why}", path.display()))
}

/// The rules on the known fields of a record that parsed: schema 1, every path and link
/// target absolute and in the path grammar, every service a service name, every version a
/// version. A record that breaks one is never used to name a path.
fn validate(record: &PlacedRecord) -> Result<(), String> {
    require(record.schema == SCHEMA, || {
        format!("its schema is {}, not {SCHEMA}", record.schema)
    })?;
    for (path, entry) in &record.paths {
        require_path("the path", path)?;
        require_path("the link target", &entry.link_target)?;
        require(is_service_name(&entry.service), || {
            format!("{:?} is not a service name", entry.service)
        })?;
        require(is_version(&entry.version), || {
            format!("{:?} is not a version", entry.version)
        })?;
    }
    record.uninstalling.keys().try_for_each(|service| {
        require(is_service_name(service), || {
            format!("the tombstone {service:?} is not a service name")
        })
    })
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
        let record: PlacedRecord = serde_json::from_value(value).map_err(corrupt(&path))?;
        validate(&record).map_err(|why| PlacedError::Corrupt { path, why })?;
        Ok(record)
    }

    /// Writes the record: the temporary file, its `fsync`, the rename, the directory's `fsync`.
    /// The caller holds the lock of `placed.json` ([`crate::Placer::lock`]), which the signature
    /// asks for, so that two writers never interleave a read and a write.
    pub fn save(&self, record: &PlacedRecord, _lock: &PlacedLock) -> Result<(), PlacedError> {
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
