//! `placed.json`: what the daemon records of the services it installed (design section 10,
//! "Placed-path ownership record").
//!
//! ```json
//! {
//!   "schema": 1,
//!   "uninstalling": { "other": {}, "old": { "hook_done": true } },
//!   "installed": {
//!     "web": { "installer": "apt", "package": "web", "version": "1.4.0-1", "repository": "main",
//!              "links": ["/etc/systemd/system/multi-user.target.wants/web.service"] }
//!   }
//! }
//! ```
//!
//! `installed` maps a service to its installer and, for a repository package, also its package,
//! version, repository and the enablement links its `activate` wrote, `pending` while an install
//! is under way (packages-lifecycle.md section 6.5). The installer's name is kept as written: the
//! daemon refuses to act for a service of one it has no steps of, and the record is read whatever
//! it names. It is written only when not empty.
//!
//! Schema 1, changed additively only: a key this daemon does not know, at the top level, in an
//! entry or in a tombstone, is kept as it is when the record is written again. The `paths` and
//! `reload_pending` keys of the placed-path linking an earlier release wrote are such keys: they
//! are read as they are and written back as they were, so a release that still reads them finds
//! them. A record with a newer schema, one that does not parse, or one whose known fields break
//! their rules (schema 1, services in their grammar), is an error naming the file, and the file
//! is left as it is. A write needs the lock of the file ([`crate::PlacedLock`]). Every write is a
//! temporary file in the same directory, an `fsync` of the file, a rename and an `fsync` of the
//! directory.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use hostkeeper_config::names::is_service_name;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{PlacedError, io_at};
use crate::fs::Fs;
use crate::placer::PlacedLock;
use crate::roots::{LOCK_FILE, PLACED_FILE};

/// The schema this daemon reads and writes.
pub const SCHEMA: u64 = 1;
/// The temporary name `placed.json` is written under, beside it. `~` is outside the path
/// grammar, so no placed path can have this name.
pub const TEMPORARY_FILE: &str = ".hk~placed.json~tmp";

/// The tombstone of a service being uninstalled (design section 4.7, "Uninstall", step 1). Its
/// paths are not linked while it stands. Its fields say how far the uninstall got, so that a
/// restart resumes it from what is on disk; each is written only when set, so a tombstone of an
/// uninstall that has not run its hook is `{}`, as the release before these fields wrote it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Tombstone {
    /// The `uninstall` step of the hook exited 0: a resumed uninstall does not run it again.
    #[serde(default, skip_serializing_if = "is_false")]
    pub hook_done: bool,
    /// The `uninstall` step exited 76 (halt): nothing more of the uninstall runs until the halt
    /// is cleared.
    #[serde(default, skip_serializing_if = "is_false")]
    pub halted: bool,
    /// The keys of a newer writer, kept on rewrite.
    #[serde(flatten)]
    extra: Map<String, Value>,
}

/// How a service was installed. A repository package's entry also names the package, the version installed (or being installed while `pending`), the
/// repository and the enablement links its `activate` wrote (packages-repositories.md section
/// 4.3); a `macos-pkg` entry names the package's identifier, the version and the directories its
/// installs created (section 4.7). Each of these keys is written only when set, so an entry of
/// the installer alone is written as before.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InstalledEntry {
    /// The installer's name, as the entry spells it.
    pub installer: String,
    /// The package, as the package manager knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// The version the package manager installed, as it spells it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The entry of the host's repository configuration the version came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// The enablement links of the `run` units the installer wrote, absolute.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub links: BTreeSet<String>,
    /// Written before the install, cleared once the package manager shows `version`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub pending: bool,
    /// The directories a `macos-pkg` install created that were not on the host before, as
    /// logical paths: its `remove` deletes them once they are empty.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub created_dirs: BTreeSet<String>,
    /// The keys of a newer writer, kept on rewrite.
    #[serde(flatten)]
    extra: Map<String, Value>,
}

impl InstalledEntry {
    /// An entry for `installer`, naming nothing else.
    pub fn new(installer: &str) -> InstalledEntry {
        InstalledEntry {
            installer: installer.to_owned(),
            package: None,
            version: None,
            repository: None,
            links: BTreeSet::new(),
            pending: false,
            created_dirs: BTreeSet::new(),
            extra: Map::new(),
        }
    }
}

/// The contents of `placed.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlacedRecord {
    pub schema: u64,
    /// The tombstones, by service.
    #[serde(default)]
    pub uninstalling: BTreeMap<String, Tombstone>,
    /// The installer of each installed service; a service without an entry has none recorded.
    /// Written only when not empty.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub installed: BTreeMap<String, InstalledEntry>,
    /// The keys of a newer writer, kept on rewrite.
    #[serde(flatten)]
    extra: Map<String, Value>,
}

impl Default for PlacedRecord {
    fn default() -> PlacedRecord {
        PlacedRecord {
            schema: SCHEMA,
            uninstalling: BTreeMap::new(),
            installed: BTreeMap::new(),
            extra: Map::new(),
        }
    }
}

impl PlacedRecord {
    /// The keys of a newer or an earlier writer this daemon does not know, as read.
    pub(crate) fn extra(&self) -> &Map<String, Value> {
        &self.extra
    }

    /// [`PlacedRecord::extra`], to change before the record is written again.
    pub(crate) fn extra_mut(&mut self) -> &mut Map<String, Value> {
        &mut self.extra
    }

    /// The installer recorded for `service`; `None` for a service with none.
    pub fn installer_of(&self, service: &str) -> Option<&str> {
        self.installed
            .get(service)
            .map(|entry| entry.installer.as_str())
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// `Err(why())` unless `ok`.
fn require(ok: bool, why: impl FnOnce() -> String) -> Result<(), String> {
    ok.then_some(()).ok_or_else(why)
}

/// The rules on the known fields of a record that parsed: schema 1 and every service a service
/// name. A record that breaks one is never used to name a service.
fn validate(record: &PlacedRecord) -> Result<(), String> {
    require(record.schema == SCHEMA, || {
        format!("its schema is {}, not {SCHEMA}", record.schema)
    })?;
    record.uninstalling.keys().try_for_each(|service| {
        require(is_service_name(service), || {
            format!("the tombstone {service:?} is not a service name")
        })
    })?;
    record.installed.keys().try_for_each(|service| {
        require(is_service_name(service), || {
            format!("the installed service {service:?} is not a service name")
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
    /// A lock of another state directory, or of another root, is
    /// [`PlacedError::ForeignLock`], and a record whose known fields break their rules is
    /// [`PlacedError::Corrupt`], each before anything is written.
    pub fn save(&self, record: &PlacedRecord, lock: &PlacedLock) -> Result<(), PlacedError> {
        let path = self.path();
        let own = self.fs.host_path(&self.dir.join(LOCK_FILE));
        if lock.path() != own {
            return Err(PlacedError::ForeignLock {
                lock: lock.path().to_path_buf(),
                record: path,
            });
        }
        validate(record).map_err(|why| PlacedError::Corrupt {
            path: path.clone(),
            why,
        })?;
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
