//! The MDM tables as files: one family per file in a directory, each replaced whole as the other
//! state files are (`desired::write_whole_syncing`). A file is `{"schema": 1, "rows": [...]}`; a
//! file of another schema, or one that does not read, stops the start naming it, and a field of a
//! later release is skipped (formats change additively, B8). A family that has no file is empty.
//!
//! Every call makes its change on a copy of the memory ([`MdmState`]), writes the families that
//! changed, and keeps the copy only when every write succeeded, so a failed change is not made.

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

#[derive(Serialize, Deserialize)]
struct Family<T> {
    schema: u32,
    rows: Vec<T>,
}

/// The MDM tables in the files of one directory.
pub(crate) struct FileMdm {
    dir: PathBuf,
    held: Mutex<Held>,
}

/// The memory, and the bytes each family's file holds, so that a call writes only what changed.
struct Held {
    state: MdmState,
    written: [Vec<u8>; 5],
}

/// The bytes of every family of `state`, in the order the files are written: the commands first
/// and the enrollments after them, so that a change that stops half way leaves commands cancelled
/// under an enrollment that still stands, which is harmless, and not the other way about.
fn encode(state: &MdmState) -> Result<[Vec<u8>; 5], MdmError> {
    fn one<T: Serialize>(rows: Vec<&T>) -> Result<Vec<u8>, MdmError> {
        serde_json::to_vec_pretty(&Family {
            schema: SCHEMA,
            rows,
        })
        .map_err(|e| MdmError::Store(e.to_string()))
    }
    Ok([
        one({
            let mut all: Vec<&Command> = state.commands.values().collect();
            all.sort_by_key(|c| c.seq);
            all
        })?,
        one(state.identities.values().collect())?,
        one(state.enrollments.values().collect())?,
        one(state.abm_devices.values().collect())?,
        one(state.abm_state.iter().collect())?,
    ])
}

fn failed(path: &Path, why: &dyn std::fmt::Display) -> MdmError {
    MdmError::Store(format!("{}: {why}", path.display()))
}

fn store(e: &StoreError) -> MdmError {
    MdmError::Store(e.to_string())
}

/// The rows of the family file `name` in `dir`; none when the file is not there.
fn read_family<T: DeserializeOwned>(dir: &Path, name: &str) -> Result<Vec<T>, MdmError> {
    let path = dir.join(name);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(failed(&path, &e)),
    };
    let family: Family<T> = serde_json::from_slice(&bytes).map_err(|e| failed(&path, &e))?;
    if family.schema == SCHEMA {
        Ok(family.rows)
    } else {
        Err(failed(
            &path,
            &format!("schema {} is not one this release reads", family.schema),
        ))
    }
}

impl FileMdm {
    /// The tables in `dir`, which is made when it is not there, read from the files it holds.
    ///
    /// # Errors
    /// [`MdmError::Store`]: the directory cannot be made, or a file does not read.
    pub(crate) fn open(dir: &Path) -> Result<Self, MdmError> {
        fs::create_dir_all(dir).map_err(|e| failed(dir, &e))?;
        let state = MdmState {
            enrollments: read_family::<Enrollment>(dir, ENROLLMENTS)?
                .into_iter()
                .map(|r| (r.enrollment_id.clone(), r))
                .collect(),
            identities: read_family::<Identity>(dir, IDENTITIES)?
                .into_iter()
                .map(|r| (r.challenge_sha256.clone(), r))
                .collect(),
            commands: read_family::<Command>(dir, COMMANDS)?
                .into_iter()
                .map(|r| (r.id.clone(), r))
                .collect(),
            abm_devices: read_family::<AbmDevice>(dir, ABM_DEVICES)?
                .into_iter()
                .map(|r| (r.serial.clone(), r))
                .collect(),
            abm_state: read_family::<AbmState>(dir, ABM_STATE)?.into_iter().next(),
        };
        // What the files hold now is what the memory encodes to, or the first change writes all.
        let written = [
            fs::read(dir.join(COMMANDS)).unwrap_or_default(),
            fs::read(dir.join(IDENTITIES)).unwrap_or_default(),
            fs::read(dir.join(ENROLLMENTS)).unwrap_or_default(),
            fs::read(dir.join(ABM_DEVICES)).unwrap_or_default(),
            fs::read(dir.join(ABM_STATE)).unwrap_or_default(),
        ];
        Ok(Self {
            dir: dir.to_owned(),
            held: Mutex::new(Held { state, written }),
        })
    }

    /// Runs `change` on a copy of the memory, writes the families it changed, and keeps the copy.
    fn change<R>(
        &self,
        change: impl FnOnce(&mut MdmState) -> Result<R, MdmError>,
    ) -> Result<R, MdmError> {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let mut next = held.state.clone();
        let answer = change(&mut next)?;
        if next == held.state {
            return Ok(answer);
        }
        let bytes = encode(&next)?;
        let names = [COMMANDS, IDENTITIES, ENROLLMENTS, ABM_DEVICES, ABM_STATE];
        for (index, name) in names.iter().enumerate() {
            if bytes[index] != held.written[index] {
                desired::write_whole_syncing(
                    &self.dir.join(name),
                    &bytes[index],
                    desired::sync_dir,
                )
                .map_err(|e| store(&e))?;
                held.written[index].clone_from(&bytes[index]);
            }
        }
        held.state = next;
        Ok(answer)
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
