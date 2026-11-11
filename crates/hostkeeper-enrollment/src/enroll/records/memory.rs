//! [`MemoryRecords`]: the records in memory, for tests that need no files.
//!
//! A commit is all or nothing, as a database transaction is: every record of it is written, or,
//! when it fails, none. Every call of the [`Records`] port but [`Records::place`] is one numbered
//! operation, counted from 1, and the one numbered [`MemoryRecords::fail_at`] fails and changes
//! nothing, so a test can stop the store at any call as a lost database would.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::{Record, RecordKey, Records, RecordsLock, pb};
use crate::enroll::mac_nonce::MacNonceHash;
use crate::enroll::{FIRST_SERIAL, StoreError};
use crate::names::{HostId, KeyHash};

/// What the fake holds: every record, by its key, and the next serial (`None` until one is
/// taken).
#[derive(Debug, Default)]
struct Held {
    records: BTreeMap<RecordKey, Record>,
    next_serial: Option<u64>,
}

impl Held {
    fn get(&self, key: &RecordKey) -> Option<&Record> {
        self.records.get(key)
    }
}

/// The records in memory. Clones of the `Arc` it is shared by see one set of records: a store
/// made again over it is the store after a restart.
#[derive(Debug, Default)]
pub struct MemoryRecords {
    held: Mutex<Held>,
    count: AtomicU64,
    fail_at: AtomicU64,
}

impl MemoryRecords {
    /// No records, failing nothing until told to.
    pub fn new() -> MemoryRecords {
        MemoryRecords::default()
    }

    /// Fails the operation numbered `n` (counted from 1 since this was made); 0 fails nothing.
    pub fn fail_at(&self, n: u64) {
        self.fail_at.store(n, Ordering::SeqCst);
    }

    /// How many operations were asked for so far, the failed one included.
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::SeqCst)
    }

    /// Counts one operation, `what`, and fails it when it is the chosen one; else the records.
    fn step(&self, what: &str) -> Result<MutexGuard<'_, Held>, StoreError> {
        let n = self.count.fetch_add(1, Ordering::SeqCst) + 1;
        if n == self.fail_at.load(Ordering::SeqCst) {
            return Err(StoreError::Io {
                path: PathBuf::from(format!("memory/{what}")),
                source: io::Error::other(format!("injected failure at operation {n}")),
            });
        }
        Ok(self.held.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

impl Records for MemoryRecords {
    fn lock(&self) -> Result<RecordsLock, StoreError> {
        drop(self.step("lock")?);
        Ok(RecordsLock::in_process())
    }

    fn place(&self, key: &RecordKey) -> PathBuf {
        PathBuf::from(match key {
            RecordKey::Request(id) => format!("memory/requests/{id}"),
            RecordKey::Issued(id) => format!("memory/issued/{id}"),
            RecordKey::Host(host) => format!("memory/hosts/{host}"),
            RecordKey::Deny(host) => format!("memory/deny/{host}"),
            RecordKey::MacNonce(hash) => {
                format!("memory/mac-nonces/{}", hex::encode(hash.as_bytes()))
            }
            RecordKey::Serial => "memory/serial".to_owned(),
        })
    }

    fn request(&self, id: &KeyHash) -> Result<Option<pb::Request>, StoreError> {
        Ok(match self.step("request")?.get(&RecordKey::Request(*id)) {
            Some(Record::Request(m)) => Some(m.clone()),
            _ => None,
        })
    }

    fn requests(&self) -> Result<Vec<(KeyHash, pb::Request)>, StoreError> {
        Ok(self
            .step("requests")?
            .records
            .iter()
            .filter_map(|entry| match entry {
                (RecordKey::Request(id), Record::Request(m)) => Some((*id, m.clone())),
                _ => None,
            })
            .collect())
    }

    fn issued(&self, id: &KeyHash) -> Result<Option<pb::Issued>, StoreError> {
        Ok(match self.step("issued")?.get(&RecordKey::Issued(*id)) {
            Some(Record::Issued(m)) => Some(m.clone()),
            _ => None,
        })
    }

    fn host(&self, host: &HostId) -> Result<Option<pb::Host>, StoreError> {
        Ok(
            match self.step("host")?.get(&RecordKey::Host(host.clone())) {
                Some(Record::Host(m)) => Some(m.clone()),
                _ => None,
            },
        )
    }

    fn host_ids(&self) -> Result<Vec<HostId>, StoreError> {
        Ok(self
            .step("hosts")?
            .records
            .keys()
            .filter_map(|key| match key {
                RecordKey::Host(host) => Some(host.clone()),
                _ => None,
            })
            .collect())
    }

    fn deny(&self, host: &HostId) -> Result<Option<pb::Deny>, StoreError> {
        Ok(
            match self.step("deny")?.get(&RecordKey::Deny(host.clone())) {
                Some(Record::Deny(m)) => Some(m.clone()),
                _ => None,
            },
        )
    }

    fn mac_nonce(&self, hash: &MacNonceHash) -> Result<Option<pb::MacNonce>, StoreError> {
        Ok(
            match self.step("mac nonce")?.get(&RecordKey::MacNonce(*hash)) {
                Some(Record::MacNonce(m)) => Some(m.clone()),
                _ => None,
            },
        )
    }

    fn commit(&self, records: &[Record]) -> Result<(), StoreError> {
        let keys = records
            .iter()
            .map(Record::key)
            .collect::<Result<Vec<RecordKey>, StoreError>>()?;
        let mut held = self.step("commit")?;
        held.records
            .extend(keys.into_iter().zip(records.iter().cloned()));
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
        let mut held = self.step("serial")?;
        let next = held.next_serial.unwrap_or(FIRST_SERIAL);
        held.next_serial = Some(next + 1);
        if let Some((key, request)) = reserving {
            let mut request = request.clone();
            request.serial = next;
            held.records.insert(key, Record::Request(request));
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
