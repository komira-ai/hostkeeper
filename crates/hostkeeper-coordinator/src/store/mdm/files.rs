//! The MDM tables as files: one family per file in a directory, each replaced whole as the other
//! state files are (`desired::write_whole_syncing`). A file is `{"schema": 1, "rows": [...]}`; a
//! file of another schema, or one that does not read, stops the start naming it, and a field of a
//! later release is kept as it was read and written back with its row (formats change additively,
//! B8). A family that has no file is empty.
//!
//! Every call makes its change on a copy of the memory ([`MdmState`]), writes the families that
//! changed, and keeps the copy only when every write succeeded; see [`FileMdm::change`] for a write
//! that fails.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::state::MdmState;
use super::{
    AbmBatch, AbmDevice, AbmState, Answer, Answered, Challenge, Command, Consumed, Enqueued,
    Enrollment, Identity, Issuance, MdmError, MdmStore, NewCommand,
};
use crate::desired::{self, StoreError};

/// The schema of every family's file.
const SCHEMA: u32 = 1;

const ENROLLMENTS: &str = "enrollments.json";
const IDENTITIES: &str = "identities.json";
const COMMANDS: &str = "commands.json";
const ABM_DEVICES: &str = "abm_devices.json";
const ABM_STATE: &str = "abm_state.json";
/// The files in the order [`encode`] makes their bytes.
const FAMILIES: [&str; 5] = [COMMANDS, IDENTITIES, ENROLLMENTS, ABM_DEVICES, ABM_STATE];

/// The fields of a row that this release does not know, by name: kept as read and written back.
type Extra = BTreeMap<String, serde_json::Value>;

/// A row as read: the fields this release knows and the ones of a later release.
#[derive(Deserialize)]
struct RowIn<T> {
    #[serde(flatten)]
    row: T,
    #[serde(flatten)]
    extra: Extra,
}

#[derive(Deserialize)]
struct FamilyIn<T> {
    schema: u32,
    rows: Vec<RowIn<T>>,
}

/// A row as written: its fields and the fields of a later release that were read with it.
#[derive(Serialize)]
struct RowOut<'a, T> {
    #[serde(flatten)]
    row: &'a T,
    #[serde(flatten)]
    extra: &'a Extra,
}

#[derive(Serialize)]
struct FamilyOut<'a, T> {
    schema: u32,
    rows: Vec<RowOut<'a, T>>,
}

/// The extra fields of the rows of one family, by the key of the row.
type Extras = BTreeMap<String, Extra>;

/// The MDM tables in the files of one directory.
pub(crate) struct FileMdm {
    dir: PathBuf,
    sync: desired::SyncDir,
    held: Mutex<Held>,
}

/// The memory, the bytes each family's file holds, so that a call writes only what changed, and the
/// fields of a later release that the rows came with.
struct Held {
    state: MdmState,
    written: Vec<Vec<u8>>,
    extras: [Extras; 5],
}

/// The bytes of the family of `rows`, each with its key, which is where its extra fields are
/// looked up.
pub(super) fn family_bytes<T: Serialize>(
    rows: Vec<(&str, &T)>,
    extras: &Extras,
) -> Result<Vec<u8>, MdmError> {
    let none = Extra::new();
    let family = FamilyOut {
        schema: SCHEMA,
        rows: rows
            .into_iter()
            .map(|(key, row)| RowOut {
                row,
                extra: extras.get(key).unwrap_or(&none),
            })
            .collect(),
    };
    serde_json::to_vec_pretty(&family).map_err(|e| MdmError::Store(e.to_string()))
}

/// The bytes of every family of `state`, in the order the files are written: the commands first
/// and the enrollments after them, so that a change that stops half way leaves commands cancelled
/// under an enrollment that still stands, which is harmless, and not the other way about.
fn encode(state: &MdmState, extras: &[Extras; 5]) -> Result<Vec<Vec<u8>>, MdmError> {
    let mut commands: Vec<&Command> = state.commands.values().collect();
    commands.sort_by_key(|c| c.seq);
    [
        family_bytes(
            commands.into_iter().map(|c| (c.id.as_str(), c)).collect(),
            &extras[0],
        ),
        family_bytes(
            state
                .identities
                .iter()
                .map(|(k, r)| (k.as_str(), r))
                .collect(),
            &extras[1],
        ),
        family_bytes(
            state
                .enrollments
                .iter()
                .map(|(k, r)| (k.as_str(), r))
                .collect(),
            &extras[2],
        ),
        family_bytes(
            state
                .abm_devices
                .iter()
                .map(|(k, r)| (k.as_str(), r))
                .collect(),
            &extras[3],
        ),
        family_bytes(
            state.abm_state.iter().map(|r| ("", r)).collect(),
            &extras[4],
        ),
    ]
    .into_iter()
    .collect()
}

fn failed(path: &Path, why: &dyn std::fmt::Display) -> MdmError {
    MdmError::Store(format!("{}: {why}", path.display()))
}

fn store(e: &StoreError) -> MdmError {
    MdmError::Store(e.to_string())
}

/// The rows of the family file `name` in `dir` by their `key`, and the fields of a later release
/// they came with; none when the file is not there.
fn read_family<T: DeserializeOwned>(
    dir: &Path,
    name: &str,
    key: fn(&T) -> String,
) -> Result<(BTreeMap<String, T>, Extras), MdmError> {
    let path = dir.join(name);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok((BTreeMap::new(), Extras::new()));
        }
        Err(e) => return Err(failed(&path, &e)),
    };
    let family: FamilyIn<T> = serde_json::from_slice(&bytes).map_err(|e| failed(&path, &e))?;
    if family.schema != SCHEMA {
        return Err(failed(
            &path,
            &format!("schema {} is not one this release reads", family.schema),
        ));
    }
    let mut rows = BTreeMap::new();
    let mut extras = Extras::new();
    for RowIn { row, extra } in family.rows {
        let key = key(&row);
        if !extra.is_empty() {
            extras.insert(key.clone(), extra);
        }
        rows.insert(key, row);
    }
    Ok((rows, extras))
}

impl FileMdm {
    /// The tables in `dir`, which is made when it is not there, read from the files it holds.
    ///
    /// # Errors
    /// [`MdmError::Store`]: the directory cannot be made, or a file does not read.
    pub(crate) fn open(dir: &Path) -> Result<Self, MdmError> {
        Self::open_syncing(dir, desired::sync_dir)
    }

    /// [`Self::open`], with `sync` as the directory sync of every file written.
    pub(crate) fn open_syncing(dir: &Path, sync: desired::SyncDir) -> Result<Self, MdmError> {
        fs::create_dir_all(dir).map_err(|e| failed(dir, &e))?;
        let (enrollments, e2) =
            read_family::<Enrollment>(dir, ENROLLMENTS, |r| r.enrollment_id.clone())?;
        let (identities, e1) =
            read_family::<Identity>(dir, IDENTITIES, |r| r.challenge_sha256.clone())?;
        let (commands, e0) = read_family::<Command>(dir, COMMANDS, |r| r.id.clone())?;
        let (abm_devices, e3) = read_family::<AbmDevice>(dir, ABM_DEVICES, |r| r.serial.clone())?;
        let (abm_state, e4) = read_family::<AbmState>(dir, ABM_STATE, |_| String::new())?;
        let state = MdmState {
            enrollments,
            identities,
            commands,
            abm_devices,
            abm_state: abm_state.into_values().next(),
        };
        // What the files hold now is what the memory encodes to, or the first change writes all.
        let written = FAMILIES
            .iter()
            .map(|name| fs::read(dir.join(name)).unwrap_or_default())
            .collect();
        Ok(Self {
            dir: dir.to_owned(),
            sync,
            held: Mutex::new(Held {
                state,
                written,
                extras: [e0, e1, e2, e3, e4],
            }),
        })
    }

    /// Runs `change` on a copy of the memory, writes the families it changed, and keeps the copy.
    ///
    /// A write that fails leaves the memory as it was and the call answers the error; the families
    /// written before it stay on disk, and the next change that writes them puts them back. A file
    /// that is in place but whose directory could not be synced counts as written (it is on disk
    /// and was read back by any start): the change is kept, the failure is logged, and the call
    /// answers its result, so that the memory and the disk do not part.
    fn change<R>(
        &self,
        change: impl FnOnce(&mut MdmState) -> Result<R, MdmError>,
    ) -> Result<R, MdmError> {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let mut next = held.state.clone();
        let answer = change(&mut next)?;
        self.keep(&mut held, next)?;
        Ok(answer)
    }

    /// Writes the families of `next` that differ from what the files hold, and makes it the memory.
    fn keep(&self, held: &mut Held, next: MdmState) -> Result<(), MdmError> {
        if next == held.state {
            return Ok(());
        }
        let bytes = encode(&next, &held.extras)?;
        for (index, name) in FAMILIES.iter().enumerate() {
            if bytes[index] != held.written[index] {
                match desired::write_whole_syncing(&self.dir.join(name), &bytes[index], self.sync) {
                    Ok(()) => {}
                    Err(unsynced @ StoreError::NotSynced { .. }) => {
                        tracing::error!(
                            %unsynced,
                            "an MDM file is in place but its directory was not synced; the change is kept"
                        );
                    }
                    Err(e) => return Err(store(&e)),
                }
                held.written[index].clone_from(&bytes[index]);
            }
        }
        held.state = next;
        Ok(())
    }

    fn read<R>(&self, read: impl FnOnce(&MdmState) -> R) -> R {
        read(
            &self
                .held
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .state,
        )
    }
}

impl MdmStore for FileMdm {
    fn replace_live(&self, new: &Enrollment) -> Result<Vec<String>, MdmError> {
        self.change(|s| s.replace_live(new))
    }

    fn check_out(&self, enrollment_id: &str, now_unix: i64) -> Result<bool, MdmError> {
        self.change(|s| Ok(s.check_out(enrollment_id, now_unix)))
    }

    fn set_push(
        &self,
        enrollment_id: &str,
        token: &[u8],
        magic: &str,
        topic: &str,
    ) -> Result<bool, MdmError> {
        self.change(|s| Ok(s.set_push(enrollment_id, token, magic, topic)))
    }

    fn record_push(&self, enrollment_id: &str, now_unix: i64) -> Result<(), MdmError> {
        self.change(|s| {
            s.record_push(enrollment_id, now_unix);
            Ok(())
        })
    }

    fn record_contact(&self, enrollment_id: &str, now_unix: i64) -> Result<(), MdmError> {
        self.change(|s| {
            s.record_contact(enrollment_id, now_unix);
            Ok(())
        })
    }

    fn enrollment(&self, enrollment_id: &str) -> Result<Option<Enrollment>, MdmError> {
        Ok(self.read(|s| s.enrollments.get(enrollment_id).cloned()))
    }

    fn live_enrollment(&self, serial: &str) -> Result<Option<Enrollment>, MdmError> {
        Ok(self.read(|s| s.live_enrollment(serial)))
    }

    fn issue_challenge(&self, challenge: &Challenge) -> Result<(), MdmError> {
        self.change(|s| s.issue_challenge(challenge))
    }

    fn consume_challenge(
        &self,
        challenge_sha256: &str,
        key_sha256: &str,
        issuance: &Issuance,
        now_unix: i64,
    ) -> Result<Consumed, MdmError> {
        self.change(|s| Ok(s.consume_challenge(challenge_sha256, key_sha256, issuance, now_unix)))
    }

    fn challenge(&self, challenge_sha256: &str) -> Result<Option<Identity>, MdmError> {
        Ok(self.read(|s| s.identities.get(challenge_sha256).cloned()))
    }

    fn identity(&self, cert_sha256: &str) -> Result<Option<Identity>, MdmError> {
        Ok(self.read(|s| s.identity(cert_sha256)))
    }

    fn kill_identity(&self, cert_sha256: &str) -> Result<bool, MdmError> {
        self.change(|s| Ok(s.kill_identity(cert_sha256)))
    }

    fn enqueue(&self, command: &NewCommand) -> Result<Enqueued, MdmError> {
        self.change(|s| Ok(s.enqueue(command)))
    }

    fn claim(
        &self,
        enrollment_id: &str,
        tried: &[String],
        now_unix: i64,
    ) -> Result<Option<Command>, MdmError> {
        self.change(|s| Ok(s.claim(enrollment_id, tried, now_unix)))
    }

    fn answer(
        &self,
        enrollment_id: &str,
        id: &str,
        answer: Answer,
        response: Option<&[u8]>,
        now_unix: i64,
    ) -> Result<Answered, MdmError> {
        self.change(|s| Ok(s.answer(enrollment_id, id, answer, response, now_unix)))
    }

    fn requeue_timed_out(&self, sent_before_unix: i64) -> Result<Vec<(String, String)>, MdmError> {
        self.change(|s| Ok(s.requeue_timed_out(sent_before_unix)))
    }

    fn command(&self, id: &str) -> Result<Option<Command>, MdmError> {
        Ok(self.read(|s| s.commands.get(id).cloned()))
    }

    fn commands(&self, enrollment_id: &str) -> Result<Vec<Command>, MdmError> {
        Ok(self.read(|s| s.commands_of(enrollment_id)))
    }

    fn abm_apply(&self, batch: &AbmBatch) -> Result<(), MdmError> {
        self.change(|s| {
            s.abm_apply(batch);
            Ok(())
        })
    }

    fn abm_device(&self, serial: &str) -> Result<Option<AbmDevice>, MdmError> {
        Ok(self.read(|s| s.abm_devices.get(serial).cloned()))
    }

    fn abm_devices(&self) -> Result<Vec<AbmDevice>, MdmError> {
        Ok(self.read(|s| s.abm_devices.values().cloned().collect()))
    }

    fn abm_state(&self) -> Result<Option<AbmState>, MdmError> {
        Ok(self.read(|s| s.abm_state.clone()))
    }
}
