//! Where the gate's device records are kept: [`DeviceStore`], the one interface the gate reads
//! and writes them through, and [`FileDeviceStore`], a file per device.
//!
//! The coordinator keeps them where it keeps its other state: in files with the file store, and
//! in the table `mdm_devices` of its database with `--database-dsn-file`, another implementation
//! of [`DeviceStore`] in the coordinator. The gate does not change. Every implementation keeps a
//! record as the bytes [`encode_record`] makes and reads it back with [`decode_record`], so a
//! record moves between them byte for byte, and [`crate::conformance`] holds each to the same
//! cases.
//!
//! [`FileDeviceStore`] keeps `<dir>/<enrollment id>.pb`, mode 0600, written whole by
//! [`write_atomic`] (a temporary file, fsync, rename, fsync of the directory). The file is a
//! protobuf message: the schema, the record's own message, and the SHA-256 of that message. A
//! file that does not decode, has no schema or a newer one, or whose digest is not its record's
//! is refused when the store is opened, so a torn write never reads as an enrolled device: the
//! coordinator does not start until the file is fixed or removed. Every cut of a file is
//! refused: one that ends before the digest has none, and one inside it has a short one. A
//! field the reader does not know is skipped (protobuf), so a record a later release writes
//! with more fields is read by this one; an unknown phase is refused. The enrollment side's
//! fields (the config profile's command, the last nonce) were added to schema 1 as fields 18
//! and 19, so a release before them reads a record that has them, and this one reads a record
//! without them as one with no profile sent and no nonce issued. The pkg sends of an enrollment
//! (task M14) are field 20, added to schema 1 the same way: a record without it is one with no
//! pkg sent. The enrollment baseline's sends (design 6.4) are field 21 the same way; its
//! unanswered `EnableRemoteDesktop`, `ProfileList` and `Settings` commands are written to field 22, not to
//! the list of field 15, because a release before them refuses a sent command of a type it does
//! not send, and so would refuse the whole record. The device name's last `Settings` and the
//! baseline installs are fields 6 and 7 of the baseline message, added the same way.
//!
//! Under the coordinator's file store, the directory is `mdm-devices` in the enrollment store
//! (`--enrollment-dir`), so `hostkeeper-coordinator backup` and `restore` copy it with the
//! store's other `.pb` files.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_enrollment::fs::{Fs, write_atomic};
use hostkeeper_enrollment::names::KeyHash;
use prost::Message as _;
use sha2::{Digest as _, Sha256};

use crate::command::{COMMAND_TYPES, ENABLE_REMOTE_DESKTOP, PROFILE_LIST, SETTINGS};
use crate::ids::{CommandUuid, EnrollmentId};
use crate::pkg::PkgDigest;
use crate::record::{
    BaselineInstall, BaselineProfile, BaselineRecord, DeviceRecord, InstallRecord, NonceRecord,
    Phase, ProfileAnswer, ProfileCommand, Sent,
};

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
struct ProfilePb {
    #[prost(string, tag = "1")]
    uuid: String,
    #[prost(uint64, tag = "2")]
    sent_unix: u64,
    /// Set once answered: true for `Acknowledged`.
    #[prost(bool, optional, tag = "3")]
    acknowledged: Option<bool>,
    #[prost(uint64, tag = "4")]
    answered_unix: u64,
    /// The config profile's settings digest; absent on the other commands and on a record
    /// written before the field.
    #[prost(string, optional, tag = "5")]
    settings_sha256: Option<String>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct NoncePb {
    #[prost(string, tag = "1")]
    serial: String,
    #[prost(bytes = "vec", tag = "2")]
    request_id: Vec<u8>,
    #[prost(uint64, tag = "3")]
    issued_unix: u64,
    #[prost(uint32, tag = "4")]
    enrollment: u32,
    #[prost(uint32, tag = "5")]
    pushes: u32,
    #[prost(message, optional, tag = "6")]
    push: Option<ProfilePb>,
    #[prost(message, optional, tag = "7")]
    removal: Option<ProfilePb>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct InstallPb {
    #[prost(string, tag = "1")]
    sha256: String,
    #[prost(uint32, tag = "2")]
    sends: u32,
    #[prost(message, optional, tag = "3")]
    last: Option<ProfilePb>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct BaselineProfilePb {
    #[prost(string, tag = "1")]
    identifier: String,
    #[prost(message, optional, tag = "2")]
    command: Option<ProfilePb>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct BaselineInstallPb {
    #[prost(string, tag = "1")]
    name: String,
    #[prost(message, optional, tag = "2")]
    command: Option<ProfilePb>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct BaselinePb {
    #[prost(message, optional, tag = "1")]
    remote_desktop: Option<ProfilePb>,
    #[prost(message, repeated, tag = "2")]
    profiles: Vec<BaselineProfilePb>,
    #[prost(message, optional, tag = "3")]
    list: Option<ProfilePb>,
    #[prost(string, repeated, tag = "4")]
    installed: Vec<String>,
    #[prost(uint64, optional, tag = "5")]
    checked_unix: Option<u64>,
    /// The device name's `Settings` and the baseline installs: added to the baseline message
    /// as fields 6 and 7, so a release before them skips both and reads the rest.
    #[prost(message, optional, tag = "6")]
    name: Option<ProfilePb>,
    #[prost(message, repeated, tag = "7")]
    installs: Vec<BaselineInstallPb>,
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
    #[prost(message, optional, tag = "18")]
    config_profile: Option<ProfilePb>,
    #[prost(message, optional, tag = "19")]
    nonce: Option<NoncePb>,
    #[prost(message, optional, tag = "20")]
    install: Option<InstallPb>,
    #[prost(message, optional, tag = "21")]
    baseline: Option<BaselinePb>,
    /// The unanswered commands of the types a release before the baseline does not know
    /// (`EnableRemoteDesktop`, `ProfileList`), kept apart from field 15 so that release, which
    /// refuses a sent command of a type it does not send, still reads the record and skips
    /// these.
    #[prost(message, repeated, tag = "22")]
    baseline_sent: Vec<SentPb>,
}

fn profile_pb(command: &ProfileCommand) -> ProfilePb {
    ProfilePb {
        uuid: command.uuid.as_str().to_owned(),
        sent_unix: command.sent_unix,
        acknowledged: command.answer.map(|answer| answer.acknowledged),
        answered_unix: command.answer.map_or(0, |answer| answer.at_unix),
        settings_sha256: command.settings_sha256.clone(),
    }
}

fn profile_of(pb: ProfilePb) -> Option<ProfileCommand> {
    Some(ProfileCommand {
        uuid: CommandUuid::parse(&pb.uuid)?,
        sent_unix: pb.sent_unix,
        answer: pb.acknowledged.map(|acknowledged| ProfileAnswer {
            acknowledged,
            at_unix: pb.answered_unix,
        }),
        settings_sha256: pb.settings_sha256,
    })
}

fn nonce_pb(nonce: &NonceRecord) -> NoncePb {
    NoncePb {
        serial: nonce.serial.clone(),
        request_id: nonce.request_id.as_bytes().to_vec(),
        issued_unix: nonce.issued_unix,
        enrollment: nonce.enrollment,
        pushes: nonce.pushes,
        push: nonce.push.as_ref().map(profile_pb),
        removal: nonce.removal.as_ref().map(profile_pb),
    }
}

/// The nonce of `pb`; `None` when its request id is not a key hash or a command's UUID is
/// malformed.
fn nonce_of(pb: NoncePb) -> Option<NonceRecord> {
    let command = |pb: Option<ProfilePb>| pb.map(profile_of).map_or(Some(None), |c| c.map(Some));
    Some(NonceRecord {
        serial: pb.serial,
        request_id: KeyHash::from_bytes(&pb.request_id)?,
        issued_unix: pb.issued_unix,
        enrollment: pb.enrollment,
        pushes: pb.pushes,
        push: command(pb.push)?,
        removal: command(pb.removal)?,
    })
}

fn install_pb(install: &InstallRecord) -> InstallPb {
    InstallPb {
        sha256: install.sha256.as_str().to_owned(),
        sends: install.sends,
        last: Some(profile_pb(&install.last)),
    }
}

/// The install of `pb`; `None` when its digest or its command's UUID is malformed, or it has
/// no command.
fn install_of(pb: InstallPb) -> Option<InstallRecord> {
    Some(InstallRecord {
        sha256: PkgDigest::parse(&pb.sha256)?,
        sends: pb.sends,
        last: profile_of(pb.last?)?,
    })
}

fn baseline_pb(baseline: &BaselineRecord) -> BaselinePb {
    BaselinePb {
        remote_desktop: baseline.remote_desktop.as_ref().map(profile_pb),
        profiles: baseline
            .profiles
            .iter()
            .map(|profile| BaselineProfilePb {
                identifier: profile.identifier.clone(),
                command: Some(profile_pb(&profile.command)),
            })
            .collect(),
        list: baseline.list.as_ref().map(profile_pb),
        installed: baseline.installed.clone(),
        checked_unix: baseline.checked_unix,
        name: baseline.name.as_ref().map(profile_pb),
        installs: baseline
            .installs
            .iter()
            .map(|install| BaselineInstallPb {
                name: install.name.clone(),
                command: Some(profile_pb(&install.command)),
            })
            .collect(),
    }
}

/// The baseline of `pb`; `None` when a command's UUID is malformed or a profile has no command.
fn baseline_of(pb: BaselinePb) -> Option<BaselineRecord> {
    let command = |pb: Option<ProfilePb>| pb.map(profile_of).map_or(Some(None), |c| c.map(Some));
    let profiles = pb
        .profiles
        .into_iter()
        .map(|profile| {
            Some(BaselineProfile {
                identifier: profile.identifier,
                command: profile_of(profile.command?)?,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let installs = pb
        .installs
        .into_iter()
        .map(|install| {
            Some(BaselineInstall {
                name: install.name,
                command: profile_of(install.command?)?,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(BaselineRecord {
        remote_desktop: command(pb.remote_desktop)?,
        profiles,
        list: command(pb.list)?,
        name: command(pb.name)?,
        installs,
        installed: pb.installed,
        checked_unix: pb.checked_unix,
    })
}

/// Whether a command of `request_type` is kept in `baseline_sent` (field 22), not in `sent`.
fn is_baseline_type(request_type: &str) -> bool {
    [ENABLE_REMOTE_DESKTOP, PROFILE_LIST, SETTINGS].contains(&request_type)
}

fn sent_pb(sent: &Sent) -> SentPb {
    SentPb {
        uuid: sent.uuid.as_str().to_owned(),
        request_type: sent.request_type.clone(),
        sent_unix: sent.sent_unix,
        queued: sent.queued,
    }
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

/// The bytes a store keeps `record` as: the message of its file `<enrollment id>.pb`, which
/// holds the schema, the record and the record's SHA-256. [`decode_record`] reads them back.
#[must_use]
pub fn encode_record(record: &DeviceRecord) -> Vec<u8> {
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
            .filter(|sent| !is_baseline_type(&sent.request_type))
            .map(sent_pb)
            .collect(),
        refused: record.refused.clone(),
        last_error: record.last_error.clone(),
        config_profile: record.config_profile.as_ref().map(profile_pb),
        nonce: record.nonce.as_ref().map(nonce_pb),
        install: record.install.as_ref().map(install_pb),
        baseline: record.baseline.as_ref().map(baseline_pb),
        baseline_sent: record
            .sent
            .iter()
            .filter(|sent| is_baseline_type(&sent.request_type))
            .map(sent_pb)
            .collect(),
    }
    .encode_to_vec();
    FilePb {
        schema: SCHEMA,
        sha256: Sha256::digest(&pb).to_vec(),
        record: pb,
    }
    .encode_to_vec()
}

/// The record a store keeps under the enrollment id `key` as `bytes`, the bytes
/// [`encode_record`] made; `place` is where it is kept, as an error names it.
///
/// # Errors
/// [`StoreError::Corrupt`]: the bytes are not a whole record this release reads, or the record
/// is of another enrollment id than `key`. [`StoreError::NewerSchema`]: they are of a newer
/// schema.
pub fn decode_record(place: &Path, key: &str, bytes: &[u8]) -> Result<DeviceRecord, StoreError> {
    decode(
        place,
        bytes,
        |id| id.as_str() == key,
        "the record's enrollment id is not its key",
    )
}

/// The record at `path` whose bytes are `bytes`, kept under the enrollment id `kept_under`
/// accepts; `elsewhere` is the reason it is refused when that refuses its id.
fn decode(
    path: &Path,
    bytes: &[u8],
    kept_under: impl Fn(&EnrollmentId) -> bool,
    elsewhere: &str,
) -> Result<DeviceRecord, StoreError> {
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
        .filter(|id| kept_under(id))
        .ok_or_else(|| corrupt(elsewhere))?;
    let phase = phase_of(pb.phase).ok_or_else(|| corrupt("the record's phase is unknown"))?;
    let mut sent = Vec::with_capacity(pb.sent.len() + pb.baseline_sent.len());
    for one in pb.sent.into_iter().chain(pb.baseline_sent) {
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
    let config_profile = pb
        .config_profile
        .map(|pb| profile_of(pb).ok_or_else(|| corrupt("the config profile's UUID is malformed")))
        .transpose()?;
    let nonce = pb
        .nonce
        .map(|pb| {
            nonce_of(pb).ok_or_else(|| corrupt("the nonce's request id or a UUID is malformed"))
        })
        .transpose()?;
    let install = pb
        .install
        .map(|pb| {
            install_of(pb).ok_or_else(|| corrupt("the pkg install's digest or UUID is malformed"))
        })
        .transpose()?;
    let baseline = pb
        .baseline
        .map(|pb| baseline_of(pb).ok_or_else(|| corrupt("a baseline command's UUID is malformed")))
        .transpose()?;
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
        config_profile,
        nonce,
        install,
        baseline,
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
                let named =
                    |id: &EnrollmentId| path.file_name() == Some(format!("{id}.pb").as_ref());
                let elsewhere = "the record's enrollment id is not its file's name";
                records.push(decode(&path, &bytes, named, elsewhere)?);
            }
        }
        Ok(records)
    }

    fn save(&self, record: &DeviceRecord) -> Result<(), StoreError> {
        let name = format!("{}.pb", record.enrollment_id);
        write_atomic(
            &*self.fs,
            &self.dir,
            &name,
            &encode_record(record),
            FILE_MODE,
        )
        .map_err(|source| StoreError::Io {
            path: self.dir.join(&name),
            source,
        })
    }
}
