//! The setup tables as files, and as memory alone. Each family is one file in a directory,
//! `secrets.json` and `settings.json`, each replaced whole as the other state files are
//! (`desired::write_whole_syncing`). A file is `{"schema": 1, ...}`; a file of another schema, or
//! one that does not read, stops the start naming it, and a field of a later release is kept as it
//! was read and written back with its row (formats change additively, B8). A family that has no
//! file is empty. The sealed bytes are lower-case hex in the file, and nothing else about a secret
//! is secret.
//!
//! Every call makes its change on a copy of the memory ([`SetupState`]), writes the families that
//! changed, and keeps the copy only when every write succeeded.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};

use super::state::SetupState;
use super::{SecretRow, SetupError, SetupStore};
use crate::desired::{self, StoreError};

/// The schema of both files.
const SCHEMA: u32 = 1;

const SECRETS: &str = "secrets.json";
const SETTINGS: &str = "settings.json";

/// The fields of a row that this release does not know, by name: kept as read and written back.
type Extra = BTreeMap<String, serde_json::Value>;

/// A secret row as the file holds it: the fields this release knows and the ones of a later one.
#[derive(Clone, Serialize, Deserialize)]
struct SecretLine {
    #[serde(flatten)]
    row: SecretRow,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Serialize, Deserialize)]
struct SecretsFile {
    schema: u32,
    rows: Vec<SecretLine>,
}

#[derive(Serialize, Deserialize)]
struct SettingsFile {
    schema: u32,
    settings: BTreeMap<String, String>,
}

/// The setup tables in the files of one directory, or in memory alone.
pub(crate) struct FileSetup {
    /// The directory and the directory sync; none for memory alone.
    dir: Option<(PathBuf, desired::SyncDir)>,
    held: Mutex<Held>,
}

/// The memory, the bytes each file holds so that a call writes only what changed, and the fields
/// of a later release that the rows came with, by kind and version.
struct Held {
    state: SetupState,
    written: [Vec<u8>; 2],
    extras: BTreeMap<(String, u32), Extra>,
}

fn failed(path: &Path, why: &dyn std::fmt::Display) -> SetupError {
    SetupError::Store(format!("{}: {why}", path.display()))
}

/// The bytes of `path`, or none when it is not there.
fn read_file(path: &Path) -> Result<Option<Vec<u8>>, SetupError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(failed(path, &e)),
    }
}

fn check_schema(path: &Path, schema: u32) -> Result<(), SetupError> {
    if schema == SCHEMA {
        Ok(())
    } else {
        Err(failed(
            path,
            &format!("schema {schema} is not one this release reads"),
        ))
    }
}

/// The bytes both files hold for `state`, with the later releases' fields of `extras`.
fn encode(
    state: &SetupState,
    extras: &BTreeMap<(String, u32), Extra>,
) -> Result<[Vec<u8>; 2], SetupError> {
    let rows = state
        .secrets
        .iter()
        .map(|row| SecretLine {
            row: row.clone(),
            extra: extras
                .get(&(row.kind.clone(), row.version))
                .cloned()
                .unwrap_or_default(),
        })
        .collect();
    Ok([
        encode_file(&SecretsFile {
            schema: SCHEMA,
            rows,
        })?,
        encode_file(&SettingsFile {
            schema: SCHEMA,
            settings: state.settings.clone(),
        })?,
    ])
}

/// The bytes of one file.
pub(super) fn encode_file<T: Serialize>(file: &T) -> Result<Vec<u8>, SetupError> {
    serde_json::to_vec_pretty(file).map_err(|e| SetupError::Store(e.to_string()))
}

impl FileSetup {
    /// The tables in `dir`, read from the files it holds; a directory that is not there holds
    /// nothing, and is made by the first change.
    ///
    /// # Errors
    /// [`SetupError::Store`]: a file does not read.
    pub(crate) fn open(dir: &Path) -> Result<Self, SetupError> {
        Self::open_syncing(dir, desired::sync_dir)
    }

    /// [`Self::open`], with `sync` as the directory sync of every file written.
    pub(crate) fn open_syncing(dir: &Path, sync: desired::SyncDir) -> Result<Self, SetupError> {
        let mut state = SetupState::default();
        let mut extras = BTreeMap::new();
        let secrets = dir.join(SECRETS);
        if let Some(bytes) = read_file(&secrets)? {
            let file: SecretsFile =
                serde_json::from_slice(&bytes).map_err(|e| failed(&secrets, &e))?;
            check_schema(&secrets, file.schema)?;
            for SecretLine { row, extra } in file.rows {
                if !extra.is_empty() {
                    extras.insert((row.kind.clone(), row.version), extra);
                }
                state.secrets.push(row);
            }
            state
                .secrets
                .sort_by(|a, b| (&a.kind, a.version).cmp(&(&b.kind, b.version)));
        }
        let settings = dir.join(SETTINGS);
        if let Some(bytes) = read_file(&settings)? {
            let file: SettingsFile =
                serde_json::from_slice(&bytes).map_err(|e| failed(&settings, &e))?;
            check_schema(&settings, file.schema)?;
            state.settings = file.settings;
        }
        // What the files hold now is what the memory encodes to, or the first change writes both.
        let written = [
            read_file(&secrets)?.unwrap_or_default(),
            read_file(&settings)?.unwrap_or_default(),
        ];
        Ok(Self {
            dir: Some((dir.to_owned(), sync)),
            held: Mutex::new(Held {
                state,
                written,
                extras,
            }),
        })
    }

    /// Tables in memory alone: nothing is written, and a start finds them empty.
    pub(crate) fn volatile() -> Self {
        Self {
            dir: None,
            held: Mutex::new(Held {
                state: SetupState::default(),
                written: [Vec::new(), Vec::new()],
                extras: BTreeMap::new(),
            }),
        }
    }

    /// Runs `change` on a copy of the memory, writes the files it changed, and keeps the copy.
    ///
    /// A write that fails leaves the memory as it was and the call answers the error. A file that
    /// is in place but whose directory could not be synced counts as written (it is on disk and
    /// any start reads it): the change is kept and the failure is logged, so that the memory and
    /// the disk do not part.
    fn change<R>(
        &self,
        change: impl FnOnce(&mut SetupState) -> Result<R, SetupError>,
    ) -> Result<R, SetupError> {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let mut next = held.state.clone();
        let answer = change(&mut next)?;
        if next == held.state {
            return Ok(answer);
        }
        if let Some((dir, sync)) = &self.dir {
            let bytes = encode(&next, &held.extras)?;
            fs::create_dir_all(dir).map_err(|e| failed(dir, &e))?;
            for (index, name) in [SECRETS, SETTINGS].into_iter().enumerate() {
                // A family with no row and no file is as the file would say; it is not made.
                let empty = if index == 0 {
                    next.secrets.is_empty()
                } else {
                    next.settings.is_empty()
                };
                if bytes[index] == held.written[index] || (held.written[index].is_empty() && empty)
                {
                    continue;
                }
                match desired::write_whole_syncing(&dir.join(name), &bytes[index], *sync) {
                    Ok(()) => {}
                    Err(unsynced @ StoreError::NotSynced { .. }) => {
                        tracing::error!(
                            %unsynced,
                            "an MDM setup file is in place but its directory was not synced; the change is kept"
                        );
                    }
                    Err(e) => return Err(SetupError::Store(e.to_string())),
                }
                held.written[index].clone_from(&bytes[index]);
            }
        }
        held.state = next;
        Ok(answer)
    }

    fn read<R>(&self, read: impl FnOnce(&SetupState) -> R) -> R {
        read(
            &self
                .held
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .state,
        )
    }
}

impl SetupStore for FileSetup {
    fn put_secret(&self, row: &SecretRow) -> Result<(), SetupError> {
        self.change(|s| s.put_secret(row))
    }

    fn secrets(&self) -> Result<Vec<SecretRow>, SetupError> {
        Ok(self.read(|s| s.secrets.clone()))
    }

    fn prune_superseded(&self, superseded_unix: i64) -> Result<u64, SetupError> {
        self.change(|s| Ok(s.prune_superseded(superseded_unix)))
    }

    fn settings(&self) -> Result<BTreeMap<String, String>, SetupError> {
        Ok(self.read(|s| s.settings.clone()))
    }

    fn set_setting(&self, name: &str, value: &str) -> Result<(), SetupError> {
        self.change(|s| {
            s.settings.insert(name.to_owned(), value.to_owned());
            Ok(())
        })
    }
}
