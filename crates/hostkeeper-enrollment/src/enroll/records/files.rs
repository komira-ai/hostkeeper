//! [`FileRecords`]: the records as files in one directory, written through the [`Fs`] port.
//!
//! Every record is one binary protobuf message of `proto/hostkeeper/enrollment/store/v1/
//! store.proto`, mode 0600, written whole by [`write_atomic`]. Every reader checks the schema
//! (refusing a newer one and a missing one) and ignores fields it does not know (protobuf does).
//!
//! A commit writes its records one after the other, in the order it is given them, so the store
//! orders them as its crash rules need (a deny row before the host, the host before the request
//! that commits an approval, the certificate before the request marked issued). A failure stops
//! the commit there: the records before it stay written.
//!
//! A Mac nonce marked consumed in the same commit as the approval that consumes it is the one
//! exception: the request's approval is the commit point of the consumption, so the nonce's
//! file is written after it, a failure of that write is logged and not returned, and the
//! consumption is remembered in memory until [`FileRecords::open`] marks the file at the next
//! start.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use prost::Message;

use super::{Record, RecordKey, Records, RecordsLock, pb};
use crate::enroll::mac_nonce::MacNonceHash;
use crate::enroll::rows::{HostRow, RequestRow, SCHEMA, corrupt, deny_state};
use crate::enroll::{FIRST_SERIAL, StoreError};
use crate::fs::{Fs, write_atomic};
use crate::names::{HostId, KeyHash};

const REQUESTS: &str = "requests";
const ISSUED: &str = "issued";
const HOSTS: &str = "hosts";
const DENY: &str = "deny";
const MAC_NONCES: &str = "mac-nonces";
const SERIAL_FILE: &str = "serial.pb";
const LOCK_FILE: &str = "lock";

/// Every file is written owner-only.
const FILE_MODE: u32 = 0o600;

/// The records as files in one state directory (the coordinator's `--enrollment-dir`).
pub struct FileRecords {
    fs: Arc<dyn Fs>,
    dir: PathBuf,
    /// The Mac nonces an approval consumed, with the request that did: read from the approvals
    /// at open and added to by each approval, so a nonce whose own file was not yet marked
    /// still loads as consumed.
    consumed: Mutex<BTreeMap<MacNonceHash, KeyHash>>,
}

impl std::fmt::Debug for FileRecords {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FileRecords({})", self.dir.display())
    }
}

fn io_at(path: &Path) -> impl FnOnce(std::io::Error) -> StoreError + '_ {
    move |source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Decodes `bytes`, read from `path`, and checks the schema `schema_of` reads from it.
fn decode<M: Message + Default>(
    path: &Path,
    bytes: &[u8],
    schema_of: fn(&M) -> u32,
) -> Result<M, StoreError> {
    let message = M::decode(bytes).map_err(|e| corrupt(path, &e.to_string()))?;
    let schema = schema_of(&message);
    if schema == 0 {
        return Err(corrupt(path, "the file has no schema"));
    }
    if schema > SCHEMA {
        return Err(StoreError::NewerSchema {
            path: path.to_path_buf(),
            schema,
        });
    }
    Ok(message)
}

/// The directory under `dir` and the name of the file of `key`.
fn file_of(dir: &Path, key: &RecordKey) -> (PathBuf, String) {
    let (sub, name) = match key {
        RecordKey::Request(id) => (REQUESTS, format!("{id}.pb")),
        RecordKey::Issued(id) => (ISSUED, format!("{id}.pb")),
        RecordKey::Host(host) => (HOSTS, format!("{host}.pb")),
        RecordKey::Deny(host) => (DENY, format!("{host}.pb")),
        RecordKey::MacNonce(hash) => (MAC_NONCES, format!("{}.pb", hex::encode(hash.as_bytes()))),
        RecordKey::Serial => return (dir.to_path_buf(), SERIAL_FILE.to_owned()),
    };
    (dir.join(sub), name)
}

impl FileRecords {
    /// The records in `dir`, which is made (mode 0700) with its subdirectories when missing. A
    /// Mac nonce an approval consumed whose own file was not yet marked is marked now.
    ///
    /// # Errors
    /// [`StoreError::Io`] when a directory cannot be made; [`StoreError`] when a request or
    /// nonce file cannot be read or written.
    pub fn open(fs: Arc<dyn Fs>, dir: &Path) -> Result<FileRecords, StoreError> {
        for sub in [REQUESTS, ISSUED, HOSTS, DENY, MAC_NONCES] {
            let path = dir.join(sub);
            fs.create_dir(&path).map_err(io_at(&path))?;
        }
        let records = FileRecords {
            fs,
            dir: dir.to_path_buf(),
            consumed: Mutex::new(BTreeMap::new()),
        };
        records.reconcile_mac_nonces()?;
        Ok(records)
    }

    fn read(&self, path: &Path) -> Result<Option<Vec<u8>>, StoreError> {
        self.fs.read(path).map_err(io_at(path))
    }

    /// Reads the file of `key` and decodes it with `schema_of`.
    fn load<M: Message + Default>(
        &self,
        key: &RecordKey,
        schema_of: fn(&M) -> u32,
    ) -> Result<Option<M>, StoreError> {
        let path = self.place(key);
        self.read(&path)?
            .map(|bytes| decode(&path, &bytes, schema_of))
            .transpose()
    }

    /// The directory and the name of the file of `key`.
    fn file(&self, key: &RecordKey) -> (PathBuf, String) {
        file_of(&self.dir, key)
    }

    /// Writes `bytes` as the file of `key`.
    fn write(&self, key: &RecordKey, bytes: &[u8]) -> Result<(), StoreError> {
        let (dir, name) = self.file(key);
        write_atomic(&*self.fs, &dir, &name, bytes, FILE_MODE).map_err(io_at(&dir.join(&name)))
    }

    /// The names in the subdirectory `sub`, sorted.
    fn list(&self, sub: &str) -> Result<Vec<String>, StoreError> {
        let dir = self.dir.join(sub);
        self.fs.list(&dir).map_err(io_at(&dir))
    }

    /// [`Self::list`], a subdirectory that is not there holding nothing, as [`FileRecords::open`]
    /// would make it empty: what a read that makes no directory lists.
    fn listed(&self, sub: &str) -> Result<Vec<String>, StoreError> {
        let dir = self.dir.join(sub);
        match self.fs.list(&dir) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            listed => listed.map_err(io_at(&dir)),
        }
    }

    /// At open: remembers every nonce an approval consumed, and marks the file of each one
    /// whose mark did not land, under the lock. A consumed nonce with no file (removed by
    /// hand) is left so: it is unknown, which approves nothing either. Every request is checked
    /// as the store checks it.
    fn reconcile_mac_nonces(&self) -> Result<(), StoreError> {
        let mut consumed = Vec::new();
        for (id, request) in self.requests()? {
            let row = RequestRow::check(self.place(&RecordKey::Request(id)), request, &id)?;
            let hash = row
                .pb
                .approved
                .as_ref()
                .and_then(|approved| MacNonceHash::from_bytes(&approved.mac_nonce_sha256));
            consumed.extend(hash.map(|hash| (hash, id)));
        }
        for (hash, request) in consumed {
            self.consumed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(hash, request);
            let unmarked = self
                .load(&RecordKey::MacNonce(hash), |m: &pb::MacNonce| m.schema)?
                .filter(|nonce: &pb::MacNonce| nonce.consumed_by.is_empty());
            if let Some(mut nonce) = unmarked {
                let _held = self.lock()?;
                nonce.consumed_by = request.as_bytes().to_vec();
                self.write(&RecordKey::MacNonce(hash), &nonce.encode_to_vec())?;
            }
        }
        Ok(())
    }

    /// Writes `record`, kept under `key`.
    fn put(&self, key: &RecordKey, record: &Record) -> Result<(), StoreError> {
        let bytes = match record {
            Record::Request(m) => m.encode_to_vec(),
            Record::Issued(m) => m.encode_to_vec(),
            Record::Host(m) => m.encode_to_vec(),
            Record::Deny(m) => m.encode_to_vec(),
            Record::MacNonce(m) => m.encode_to_vec(),
        };
        self.write(key, &bytes)
    }

    /// The Mac nonce `hash` consumed by the request of an approval earlier in `before`, if
    /// `nonce` is marked so: the request is the consumption's commit point.
    fn consumed_by_approval(
        hash: &MacNonceHash,
        nonce: &pb::MacNonce,
        before: &[Record],
    ) -> Option<KeyHash> {
        let by = KeyHash::from_bytes(&nonce.consumed_by)?;
        before
            .iter()
            .any(|record| match record {
                Record::Request(request) => {
                    request.request_id == by.as_bytes()
                        && request
                            .approved
                            .as_ref()
                            .is_some_and(|a| a.mac_nonce_sha256 == hash.as_bytes())
                }
                _ => false,
            })
            .then_some(by)
    }
}

impl Records for FileRecords {
    fn lock(&self) -> Result<RecordsLock, StoreError> {
        let path = self.dir.join(LOCK_FILE);
        let file = self.fs.lock(&path).map_err(io_at(&path))?;
        Ok(RecordsLock::file(file))
    }

    fn place(&self, key: &RecordKey) -> PathBuf {
        let (dir, name) = self.file(key);
        dir.join(name)
    }

    fn request(&self, id: &KeyHash) -> Result<Option<pb::Request>, StoreError> {
        self.load(&RecordKey::Request(*id), |m: &pb::Request| m.schema)
    }

    fn requests(&self) -> Result<Vec<(KeyHash, pb::Request)>, StoreError> {
        let mut out = Vec::new();
        for id in self
            .list(REQUESTS)?
            .iter()
            .filter_map(|name| name.strip_suffix(".pb").and_then(KeyHash::from_hex))
        {
            out.extend(self.request(&id)?.map(|request| (id, request)));
        }
        Ok(out)
    }

    fn issued(&self, id: &KeyHash) -> Result<Option<pb::Issued>, StoreError> {
        self.load(&RecordKey::Issued(*id), |m: &pb::Issued| m.schema)
    }

    fn host(&self, host: &HostId) -> Result<Option<pb::Host>, StoreError> {
        self.load(&RecordKey::Host(host.clone()), |m: &pb::Host| m.schema)
    }

    fn host_ids(&self) -> Result<Vec<HostId>, StoreError> {
        let mut ids: Vec<HostId> = self
            .list(HOSTS)?
            .iter()
            .filter_map(|name| name.strip_suffix(".pb"))
            .filter_map(|name| HostId::parse(name).ok())
            .collect();
        ids.sort();
        Ok(ids)
    }

    fn deny(&self, host: &HostId) -> Result<Option<pb::Deny>, StoreError> {
        self.load(&RecordKey::Deny(host.clone()), |m: &pb::Deny| m.schema)
    }

    fn mac_nonce(&self, hash: &MacNonceHash) -> Result<Option<pb::MacNonce>, StoreError> {
        let nonce = self.load(&RecordKey::MacNonce(*hash), |m: &pb::MacNonce| m.schema)?;
        let remembered = self
            .consumed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(hash)
            .copied();
        Ok(nonce.map(|mut nonce| {
            if let Some(by) = remembered.filter(|_| nonce.consumed_by.is_empty()) {
                nonce.consumed_by = by.as_bytes().to_vec();
            }
            nonce
        }))
    }

    fn commit(&self, records: &[Record]) -> Result<(), StoreError> {
        let keys = records
            .iter()
            .map(Record::key)
            .collect::<Result<Vec<RecordKey>, StoreError>>()?;
        for (at, (record, key)) in records.iter().zip(&keys).enumerate() {
            let approved = match (record, key) {
                (Record::MacNonce(nonce), RecordKey::MacNonce(hash)) => {
                    Self::consumed_by_approval(hash, nonce, &records[..at])
                        .map(|request| (*hash, request))
                }
                _ => None,
            };
            let Some((hash, request)) = approved else {
                self.put(key, record)?;
                continue;
            };
            self.consumed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(hash, request);
            if let Err(why) = self.put(key, record) {
                tracing::error!(
                    %why,
                    request_id = %request,
                    "a consumed Mac nonce's file was not marked: the approval holds the consumption"
                );
            }
        }
        Ok(())
    }

    fn take_serial(&self, reserving: Option<&pb::Request>) -> Result<u64, StoreError> {
        let reserving = reserving
            .map(|request| {
                Record::Request(request.clone())
                    .key()
                    .map(|key| (key, request))
            })
            .transpose()?;
        let path = self.place(&RecordKey::Serial);
        let next = self
            .read(&path)?
            .map(|bytes| decode(&path, &bytes, |m: &pb::SerialCounter| m.schema))
            .transpose()?
            .map_or(FIRST_SERIAL, |counter| counter.next);
        let counter = pb::SerialCounter {
            schema: SCHEMA,
            next: next + 1,
        };
        self.write(&RecordKey::Serial, &counter.encode_to_vec())?;
        if let Some((key, request)) = reserving {
            let mut request = request.clone();
            request.serial = next;
            self.put(&key, &Record::Request(request))?;
        }
        Ok(next)
    }

    fn set_first_heartbeat(&self, host: &pb::Host, at_unix: u64) -> Result<(), StoreError> {
        let mut host = host.clone();
        host.first_heartbeat_unix = Some(at_unix);
        self.commit(&[Record::Host(host)])
    }

    fn set_first_serving(&self, host: &pb::Host, at_unix: u64) -> Result<(), StoreError> {
        let mut host = host.clone();
        host.first_serving_unix = Some(at_unix);
        self.commit(&[Record::Host(host)])
    }

    fn set_running(&self, host: &pb::Host, running: pb::Running) -> Result<(), StoreError> {
        let mut host = host.clone();
        host.running = Some(running);
        self.commit(&[Record::Host(host)])
    }

    fn set_duplicate(&self, host: &pb::Host) -> Result<(), StoreError> {
        let mut host = host.clone();
        host.duplicate = true;
        self.commit(&[Record::Host(host)])
    }
}

/// Every record of a store, read whole: what `hostkeeper-coordinator export` writes from its
/// database (docs/design/coordinator-storage.md, section 11) and its tests read back.
///
/// Each list is in the order of its key, as a file store lists its files.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    /// Every request, each checked as the store checks it.
    pub requests: Vec<pb::Request>,
    /// Every issued certificate.
    pub issued: Vec<pb::Issued>,
    /// Every host.
    pub hosts: Vec<pb::Host>,
    /// Every deny row.
    pub deny: Vec<pb::Deny>,
    /// Every Mac nonce, marked consumed when an approval consumed it.
    pub mac_nonces: Vec<pb::MacNonce>,
    /// The next serial the counter gives; `None` when none was ever taken.
    pub serial: Option<u64>,
}

impl Snapshot {
    /// Whether it holds no record and no serial was taken.
    pub fn is_empty(&self) -> bool {
        *self == Snapshot::default()
    }

    /// The files of a store directory that holds these records, as [`FileRecords`] writes them:
    /// each file's path under the directory and its bytes, the serial counter's file among them
    /// when a serial was taken. Every file is mode 0600 and every directory 0700 in a store; the
    /// subdirectories that hold no file are made by [`FileRecords::open`].
    ///
    /// # Errors
    /// [`StoreError::Unkeyed`]: a record's key does not parse ([`Record::key`]).
    pub fn files(&self) -> Result<Vec<(PathBuf, Vec<u8>)>, StoreError> {
        let records = self
            .requests
            .iter()
            .map(|m| (Record::Request(m.clone()), m.encode_to_vec()))
            .chain(
                self.issued
                    .iter()
                    .map(|m| (Record::Issued(m.clone()), m.encode_to_vec())),
            )
            .chain(
                self.hosts
                    .iter()
                    .map(|m| (Record::Host(m.clone()), m.encode_to_vec())),
            )
            .chain(
                self.deny
                    .iter()
                    .map(|m| (Record::Deny(m.clone()), m.encode_to_vec())),
            )
            .chain(
                self.mac_nonces
                    .iter()
                    .map(|m| (Record::MacNonce(m.clone()), m.encode_to_vec())),
            );
        let mut files = Vec::new();
        for (record, bytes) in records {
            let (dir, name) = file_of(Path::new(""), &record.key()?);
            files.push((dir.join(name), bytes));
        }
        if let Some(next) = self.serial {
            let (dir, name) = file_of(Path::new(""), &RecordKey::Serial);
            let counter = pb::SerialCounter {
                schema: SCHEMA,
                next,
            };
            files.push((dir.join(name), counter.encode_to_vec()));
        }
        Ok(files)
    }
}

/// Refuses `record`, read from `path`, unless it is kept under `key`: the key its own fields name.
fn kept_under(path: &Path, record: Record, key: &RecordKey) -> Result<(), StoreError> {
    if record.key().ok().as_ref() != Some(key) {
        return Err(corrupt(
            path,
            "the record's own key is not the one its file is named by",
        ));
    }
    Ok(())
}

/// The record a listing named, read from `path`: one that is gone since is refused, since the
/// store changed while it was read.
fn there<T>(path: &Path, read: Option<T>) -> Result<T, StoreError> {
    read.ok_or_else(|| {
        corrupt(
            path,
            "the file was listed and is gone: the store changed while it was read",
        )
    })
}

impl FileRecords {
    /// Every record of the store in `dir`, read without writing anything: no directory is made,
    /// no lock is taken and no Mac nonce's file is marked. A subdirectory that is not there holds
    /// nothing, as [`FileRecords::open`] would make it empty. Every record is checked as the store
    /// checks it when it reads it (the schema, a request's state, key and host ids, a host's key
    /// hash, a deny row's state), and each must be kept under the key its fields name. A Mac
    /// nonce an approval consumed reads as consumed, as [`FileRecords::open`] would mark it.
    ///
    /// # Errors
    /// [`StoreError`]: a directory or file cannot be read, or a record is refused.
    pub fn read_snapshot(fs: Arc<dyn Fs>, dir: &Path) -> Result<Snapshot, StoreError> {
        let records = FileRecords {
            fs,
            dir: dir.to_path_buf(),
            consumed: Mutex::new(BTreeMap::new()),
        };
        let mut snapshot = Snapshot::default();
        let mut consumed = BTreeMap::new();
        let ids = records.listed(REQUESTS)?;
        for id in ids
            .iter()
            .filter_map(|name| name.strip_suffix(".pb").and_then(KeyHash::from_hex))
        {
            let path = records.place(&RecordKey::Request(id));
            let request = there(&path, records.request(&id)?)?;
            let row = RequestRow::check(path, request, &id)?;
            row.renewal_of()?;
            row.host_id()?;
            let hash = row
                .pb
                .approved
                .as_ref()
                .and_then(|approved| MacNonceHash::from_bytes(&approved.mac_nonce_sha256));
            consumed.extend(hash.map(|hash| (hash, id)));
            snapshot.requests.push(row.pb);
        }
        for name in records.listed(ISSUED)? {
            let Some(id) = name.strip_suffix(".pb").and_then(KeyHash::from_hex) else {
                continue;
            };
            let key = RecordKey::Issued(id);
            let path = records.place(&key);
            let issued = there(&path, records.load(&key, |m: &pb::Issued| m.schema)?)?;
            kept_under(&path, Record::Issued(issued.clone()), &key)?;
            snapshot.issued.push(issued);
        }
        let mut hosts: Vec<HostId> = records
            .listed(HOSTS)?
            .iter()
            .filter_map(|name| name.strip_suffix(".pb"))
            .filter_map(|name| HostId::parse(name).ok())
            .collect();
        hosts.sort();
        for host in hosts {
            let key = RecordKey::Host(host.clone());
            let path = records.place(&key);
            let found = HostRow::check(&path, there(&path, records.host(&host)?)?)?.pb;
            kept_under(&path, Record::Host(found.clone()), &key)?;
            snapshot.hosts.push(found);
        }
        // In the order of the host ids, not of the file names: `web-1.pb` lists before `web.pb`.
        let mut denied: Vec<HostId> = records
            .listed(DENY)?
            .iter()
            .filter_map(|name| name.strip_suffix(".pb"))
            .filter_map(|name| HostId::parse(name).ok())
            .collect();
        denied.sort();
        for host in denied {
            let key = RecordKey::Deny(host.clone());
            let path = records.place(&key);
            let deny = there(&path, records.deny(&host)?)?;
            deny_state(&path, &deny)?;
            kept_under(&path, Record::Deny(deny.clone()), &key)?;
            snapshot.deny.push(deny);
        }
        for name in records.listed(MAC_NONCES)? {
            let Some(hash) = name
                .strip_suffix(".pb")
                .and_then(|n| hex::decode(n).ok())
                .and_then(|bytes| MacNonceHash::from_bytes(&bytes))
            else {
                continue;
            };
            let key = RecordKey::MacNonce(hash);
            let path = records.place(&key);
            let mut nonce = there(&path, records.load(&key, |m: &pb::MacNonce| m.schema)?)?;
            if let Some(by) = consumed.get(&hash).filter(|_| nonce.consumed_by.is_empty()) {
                nonce.consumed_by = by.as_bytes().to_vec();
            }
            kept_under(&path, Record::MacNonce(nonce.clone()), &key)?;
            snapshot.mac_nonces.push(nonce);
        }
        let serial = records.place(&RecordKey::Serial);
        snapshot.serial = records
            .read(&serial)?
            .map(|bytes| decode(&serial, &bytes, |m: &pb::SerialCounter| m.schema))
            .transpose()?
            .map(|counter| counter.next);
        Ok(snapshot)
    }
}
