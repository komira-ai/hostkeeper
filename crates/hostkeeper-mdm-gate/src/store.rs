//! Where the gate's device records are kept: [`DeviceStore`], the one interface the gate reads
//! and writes them through, and [`FileDeviceStore`], a file per device.
//!
//! The coordinator's state moves to a database; the records move with it as another
//! implementation of [`DeviceStore`], and the gate does not change.
//!
//! [`FileDeviceStore`] keeps `<dir>/<enrollment id>.pb`, mode 0600, written whole by
//! [`write_atomic`] (a temporary file, fsync, rename, fsync of the directory). The file is a
//! protobuf message: the schema, the record's own message, and the SHA-256 of that message. A
//! file that does not decode, has no schema or a newer one, or whose digest is not its record's
//! is refused when the store is opened, so a torn write never reads as an enrolled device: the
//! coordinator does not start until the file is fixed or removed. Every cut of a file is
//! refused: one that ends before the digest has none, and one inside it has a short one. A
//! field the reader does not know is skipped (protobuf), so a record a later release writes
//! with more fields is read by this one; an unknown phase is refused.
//!
//! Under the coordinator, the directory is `mdm-devices` in the enrollment store
//! (`--enrollment-dir`), so `hostkeeper-coordinator backup` and `restore` copy it with the
//! store's other `.pb` files.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_enrollment::fs::{Fs, write_atomic};
use prost::Message as _;
use sha2::{Digest as _, Sha256};

use crate::command::COMMAND_TYPES;
use crate::ids::{CommandUuid, EnrollmentId};
use crate::record::{DeviceRecord, Phase, Sent};

/// The schema this release writes and the newest it reads.
pub const SCHEMA: u32 = 1;

/// The mode of a record file.
const FILE_MODE: u32 = 0o600;

/// Why the device records cannot be read or written.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// A file or the directory could not be read or written.
    #[error("{path}: {source}")]
    Io {
        /// The file or directory.
        path: PathBuf,
        /// What the system said.
        #[source]
        source: std::io::Error,
    },
    /// A record file is not a whole record this release reads.
    #[error(
        "{path}: {reason}; the gate does not guess what a device's record said, so fix or remove the file to start"
    )]
    Corrupt {
        /// The file.
        path: PathBuf,
        /// What is wrong with it.
        reason: String,
    },
    /// A record file is of a newer schema than this release reads.
    #[error("{path}: schema {schema} is newer than this release reads ({SCHEMA})")]
    NewerSchema {
        /// The file.
        path: PathBuf,
        /// Its schema.
        schema: u32,
    },
}

/// Where the gate's device records are kept.
pub trait DeviceStore: Send + Sync {
    /// Every record kept.
    ///
    /// # Errors
    /// [`StoreError`]: a record cannot be read, or is not whole.
    fn load(&self) -> Result<Vec<DeviceRecord>, StoreError>;

    /// Keeps `record`, replacing the one of its enrollment id. Once this returns `Ok` the
    /// record survives a crash.
    ///
    /// # Errors
    /// [`StoreError::Io`]: it could not be written.
    fn save(&self, record: &DeviceRecord) -> Result<(), StoreError>;
}

/// A file per device in one directory.
pub struct FileDeviceStore {
    fs: Arc<dyn Fs>,
    dir: PathBuf,
}

impl std::fmt::Debug for FileDeviceStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileDeviceStore")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, prost::Message)]
struct FilePb {
    #[prost(uint32, tag = "1")]
    schema: u32,
    #[prost(bytes = "vec", tag = "2")]
    record: Vec<u8>,
    #[prost(bytes = "vec", tag = "3")]
    sha256: Vec<u8>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct SentPb {
    #[prost(string, tag = "1")]
    uuid: String,
    #[prost(string, tag = "2")]
    request_type: String,
    #[prost(uint64, tag = "3")]
    sent_unix: u64,
    #[prost(bool, tag = "4")]
    queued: bool,
}

#[derive(Clone, PartialEq, prost::Message)]
struct RecordPb {
    #[prost(string, tag = "1")]
    enrollment_id: String,
    #[prost(string, tag = "2")]
    udid: String,
    #[prost(string, optional, tag = "3")]
    authenticate_serial: Option<String>,
    #[prost(string, optional, tag = "4")]
    serial: Option<String>,
    /// 1 authenticated, 2 enrolled, 3 checked out.
    #[prost(uint32, tag = "5")]
    phase: u32,
    #[prost(uint32, tag = "6")]
    enrollment: u32,
    #[prost(bool, optional, tag = "7")]
    supervised: Option<bool>,
    #[prost(bool, optional, tag = "8")]
    enrolled_via_dep: Option<bool>,
    #[prost(bool, optional, tag = "9")]
    user_approved: Option<bool>,
    #[prost(uint64, optional, tag = "10")]
    enrolled_since_unix: Option<u64>,
    #[prost(uint64, tag = "11")]
    last_checkin_unix: u64,
    #[prost(uint64, optional, tag = "12")]
    checked_out_unix: Option<u64>,
    #[prost(uint64, optional, tag = "13")]
    device_information_unix: Option<u64>,
    #[prost(uint64, optional, tag = "14")]
    security_info_unix: Option<u64>,
    #[prost(message, repeated, tag = "15")]
    sent: Vec<SentPb>,
    #[prost(string, repeated, tag = "16")]
    refused: Vec<String>,
    #[prost(string, optional, tag = "17")]
    last_error: Option<String>,
}

fn phase_number(phase: Phase) -> u32 {
    match phase {
        Phase::Authenticated => 1,
        Phase::Enrolled => 2,
        Phase::CheckedOut => 3,
    }
}

fn phase_of(number: u32) -> Option<Phase> {
    match number {
        1 => Some(Phase::Authenticated),
        2 => Some(Phase::Enrolled),
        3 => Some(Phase::CheckedOut),
        _ => None,
    }
}

/// The bytes of the file of `record`.
fn encode(record: &DeviceRecord) -> Vec<u8> {
    let pb = RecordPb {
        enrollment_id: record.enrollment_id.as_str().to_owned(),
        udid: record.udid.clone(),
        authenticate_serial: record.authenticate_serial.clone(),
        serial: record.serial.clone(),
        phase: phase_number(record.phase),
        enrollment: record.enrollment,
        supervised: record.supervised,
        enrolled_via_dep: record.enrolled_via_dep,
        user_approved: record.user_approved,
        enrolled_since_unix: record.enrolled_since_unix,
        last_checkin_unix: record.last_checkin_unix,
        checked_out_unix: record.checked_out_unix,
        device_information_unix: record.device_information_unix,
        security_info_unix: record.security_info_unix,
        sent: record
            .sent
            .iter()
            .map(|sent| SentPb {
                uuid: sent.uuid.as_str().to_owned(),
                request_type: sent.request_type.clone(),
                sent_unix: sent.sent_unix,
                queued: sent.queued,
            })
            .collect(),
        refused: record.refused.clone(),
        last_error: record.last_error.clone(),
    }
    .encode_to_vec();
    FilePb {
        schema: SCHEMA,
        sha256: Sha256::digest(&pb).to_vec(),
        record: pb,
    }
    .encode_to_vec()
}

/// The record in the file `path`, whose bytes are `bytes`.
fn decode(path: &Path, bytes: &[u8]) -> Result<DeviceRecord, StoreError> {
    let corrupt = |reason: &str| StoreError::Corrupt {
        path: path.to_path_buf(),
        reason: reason.to_owned(),
    };
    let file = FilePb::decode(bytes).map_err(|_| corrupt("the file is not a record"))?;
    if file.schema == 0 {
        return Err(corrupt("the file has no schema"));
    }
    if file.schema > SCHEMA {
        return Err(StoreError::NewerSchema {
            path: path.to_path_buf(),
            schema: file.schema,
        });
    }
    if Sha256::digest(&file.record).as_slice() != file.sha256.as_slice() {
        return Err(corrupt("the record does not match its digest"));
    }
    let pb = RecordPb::decode(file.record.as_slice())
        .map_err(|_| corrupt("the record does not decode"))?;
    let enrollment_id = EnrollmentId::parse(&pb.enrollment_id)
        .filter(|id| path.file_name() == Some(format!("{id}.pb").as_ref()))
        .ok_or_else(|| corrupt("the record's enrollment id is not its file's name"))?;
    let phase = phase_of(pb.phase).ok_or_else(|| corrupt("the record's phase is unknown"))?;
    let mut sent = Vec::with_capacity(pb.sent.len());
    for one in pb.sent {
        let uuid = CommandUuid::parse(&one.uuid)
            .ok_or_else(|| corrupt("a sent command's UUID is malformed"))?;
        if !COMMAND_TYPES.contains(&one.request_type.as_str()) {
            return Err(corrupt("a sent command's type is not one the gate sends"));
        }
        sent.push(Sent {
            uuid,
            request_type: one.request_type,
            sent_unix: one.sent_unix,
            queued: one.queued,
        });
    }
    Ok(DeviceRecord {
        enrollment_id,
        udid: pb.udid,
        authenticate_serial: pb.authenticate_serial,
        serial: pb.serial,
        phase,
        enrollment: pb.enrollment,
        supervised: pb.supervised,
        enrolled_via_dep: pb.enrolled_via_dep,
        user_approved: pb.user_approved,
        enrolled_since_unix: pb.enrolled_since_unix,
        last_checkin_unix: pb.last_checkin_unix,
        checked_out_unix: pb.checked_out_unix,
        device_information_unix: pb.device_information_unix,
        security_info_unix: pb.security_info_unix,
        sent,
        refused: pb.refused,
        last_error: pb.last_error,
    })
}

impl FileDeviceStore {
    /// The store in `dir`, which is made (mode 0700) when missing.
    ///
    /// # Errors
    /// [`StoreError::Io`]: the directory cannot be made.
    pub fn open(fs: Arc<dyn Fs>, dir: &Path) -> Result<Self, StoreError> {
        fs.create_dir(dir).map_err(|source| StoreError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        Ok(Self {
            fs,
            dir: dir.to_path_buf(),
        })
    }
}

impl DeviceStore for FileDeviceStore {
    fn load(&self) -> Result<Vec<DeviceRecord>, StoreError> {
        let io = |path: &Path| {
            let path = path.to_path_buf();
            move |source| StoreError::Io { path, source }
        };
        let mut records = Vec::new();
        let names = self.fs.list(&self.dir).map_err(io(&self.dir))?;
        // Only `<id>.pb` is a record: a temporary file a crash left (`<id>.pb.tmp`) is not, and
        // the next write of that device replaces it.
        for name in names.iter().filter(|name| name.ends_with(".pb")) {
            let path = self.dir.join(name);
            // A record listed a moment ago and gone now was removed by hand: it is not kept.
            if let Some(bytes) = self.fs.read(&path).map_err(io(&path))? {
                records.push(decode(&path, &bytes)?);
            }
        }
        Ok(records)
    }

    fn save(&self, record: &DeviceRecord) -> Result<(), StoreError> {
        let name = format!("{}.pb", record.enrollment_id);
        write_atomic(&*self.fs, &self.dir, &name, &encode(record), FILE_MODE).map_err(|source| {
            StoreError::Io {
                path: self.dir.join(&name),
                source,
            }
        })
    }
}
