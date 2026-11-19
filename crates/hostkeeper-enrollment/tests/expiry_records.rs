//! The store's expiry over records that expire in one write of their own
//! ([`Records::expire_waiting`] returning `Some`), as the coordinator's database records do: the
//! store takes what they return, checks it, and answers in the order of the ids. Times are the
//! `now` each call is given; nothing here reads a clock.

mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use hostkeeper_enrollment::enroll::records::{
    MemoryRecords, Record, RecordKey, Records, RecordsLock, pb,
};
use hostkeeper_enrollment::enroll::{
    EnrollmentStore, Expiry, MacNonceHash, RequestState, StoreError,
};
use hostkeeper_enrollment::names::{HostId, KeyHash};
use support::{HostKey, NOW, request};

const TTL: u64 = 72 * 3600;

/// What the wrapper's own expiry does with the requests it takes.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Expires the overdue waiting requests and returns them, the last id first.
    Honest,
    /// Fails.
    Failing,
    /// Returns the request it expired under another request's id.
    Misfiled,
}

/// The in-memory records with an expiry of their own, as one write.
#[derive(Debug)]
struct Expiring {
    inner: MemoryRecords,
    failing: AtomicBool,
    misfiling: AtomicBool,
}

impl Expiring {
    fn new(mode: Mode) -> Arc<Expiring> {
        Arc::new(Expiring {
            inner: MemoryRecords::new(),
            failing: AtomicBool::new(mode == Mode::Failing),
            misfiling: AtomicBool::new(mode == Mode::Misfiled),
        })
    }
}

impl Records for Expiring {
    fn lock(&self) -> Result<RecordsLock, StoreError> {
        self.inner.lock()
    }
    fn place(&self, key: &RecordKey) -> PathBuf {
        self.inner.place(key)
    }
    fn request(&self, id: &KeyHash) -> Result<Option<pb::Request>, StoreError> {
        self.inner.request(id)
    }
    fn requests(&self) -> Result<Vec<(KeyHash, pb::Request)>, StoreError> {
        self.inner.requests()
    }
    fn issued(&self, id: &KeyHash) -> Result<Option<pb::Issued>, StoreError> {
        self.inner.issued(id)
    }
    fn host(&self, host: &HostId) -> Result<Option<pb::Host>, StoreError> {
        self.inner.host(host)
    }
    fn host_ids(&self) -> Result<Vec<HostId>, StoreError> {
        self.inner.host_ids()
    }
    fn deny(&self, host: &HostId) -> Result<Option<pb::Deny>, StoreError> {
        self.inner.deny(host)
    }
    fn mac_nonce(&self, hash: &MacNonceHash) -> Result<Option<pb::MacNonce>, StoreError> {
        self.inner.mac_nonce(hash)
    }
    fn commit(&self, records: &[Record]) -> Result<(), StoreError> {
        self.inner.commit(records)
    }
    fn take_serial(&self, reserving: Option<&pb::Request>) -> Result<u64, StoreError> {
        self.inner.take_serial(reserving)
    }
    fn set_first_heartbeat(&self, host: &pb::Host, at_unix: u64) -> Result<(), StoreError> {
        self.inner.set_first_heartbeat(host, at_unix)
    }
    fn set_first_serving(&self, host: &pb::Host, at_unix: u64) -> Result<(), StoreError> {
        self.inner.set_first_serving(host, at_unix)
    }
    fn set_running(&self, host: &pb::Host, running: pb::Running) -> Result<(), StoreError> {
        self.inner.set_running(host, running)
    }
    fn set_duplicate(&self, host: &pb::Host) -> Result<(), StoreError> {
        self.inner.set_duplicate(host)
    }

    fn expire_waiting(
        &self,
        now: u64,
        ttl_secs: u64,
    ) -> Result<Option<Vec<(KeyHash, pb::Request)>>, StoreError> {
        if self.failing.load(Ordering::SeqCst) {
            return Err(StoreError::Unkeyed("request"));
        }
        let mut taken = Vec::new();
        for (id, mut request) in self.inner.requests()? {
            let waiting = matches!(request.state, 1 | 2) && request.expired_unix == 0;
            if waiting && now.saturating_sub(request.posted_unix) > ttl_secs {
                request.expired_unix = now;
                request.expired_after_secs = ttl_secs;
                self.inner.commit(&[Record::Request(request.clone())])?;
                taken.push((id, request));
            }
        }
        taken.reverse();
        if self.misfiling.load(Ordering::SeqCst) {
            for entry in &mut taken {
                entry.0 = KeyHash::from_bytes(&[9; 32]).expect("a key hash");
            }
        }
        Ok(Some(taken))
    }
}

fn post(store: &EnrollmentStore, machine_id: &str, at: u64) -> KeyHash {
    store
        .post(request(&HostKey::new(), machine_id), at)
        .expect("post")
        .request_id
}

/// Proves: over records that expire in one write of their own, the store returns the requests
/// that write expired in the order of their ids, each in the state Expired with its expiry
/// marks, leaves a request that is not overdue waiting, and lists them as expired afterwards.
///
/// Catches: a store that trusts the order the records answer in, or that returns the stored
/// waiting state instead of Expired.
#[test]
fn the_store_returns_what_the_records_expired_sorted_by_id() {
    let records = Expiring::new(Mode::Honest);
    let store = EnrollmentStore::new(records);
    let mut overdue: Vec<KeyHash> = (0..4)
        .map(|n| post(&store, &format!("m-{n}"), NOW))
        .collect();
    overdue.sort();
    let young = post(&store, "m-young", NOW + 100);

    let at = NOW + TTL + 1;
    let swept = store.expire(at, TTL).expect("a sweep");
    assert_eq!(
        swept.iter().map(|view| view.request_id).collect::<Vec<_>>(),
        overdue
    );
    for view in &swept {
        assert_eq!(view.state, RequestState::Expired);
        assert_eq!(
            view.expiry,
            Some(Expiry {
                expired_unix: at,
                ttl_secs: TTL
            })
        );
        assert_eq!(view.posted_unix, NOW);
    }
    assert_eq!(store.expired().expect("expired"), swept);
    assert_eq!(
        store
            .pending()
            .expect("pending")
            .iter()
            .map(|view| view.request_id)
            .collect::<Vec<_>>(),
        [young]
    );
    assert_eq!(store.expire(at, TTL).expect("a sweep"), [], "once");
}

/// Proves: an error of the records' own expiry is the store's error, and nothing is expired.
///
/// Catches: a failed expiry swallowed and answered as an empty sweep.
#[test]
fn a_failed_expiry_of_the_records_is_the_stores_error() {
    let store = EnrollmentStore::new(Expiring::new(Mode::Failing));
    post(&store, "m-1", NOW);
    let error = store.expire(NOW + TTL + 1, TTL).expect_err("a failure");
    assert_eq!(
        error.to_string(),
        "a request record whose key does not parse is not kept"
    );
    assert_eq!(store.expired().expect("expired"), []);
}

/// Proves: a request the records return under an id that is not its key's is refused as a
/// corrupt record naming where it is kept.
///
/// Catches: a returned request trusted without the check every record read gets.
#[test]
fn a_request_returned_under_another_id_is_refused() {
    let store = EnrollmentStore::new(Expiring::new(Mode::Misfiled));
    post(&store, "m-1", NOW);
    let error = store.expire(NOW + TTL + 1, TTL).expect_err("refused");
    assert!(
        error
            .to_string()
            .contains("the request's key is not the one its id names"),
        "{error}"
    );
}
