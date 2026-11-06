//! The store itself: its directory, its lock, reading and writing its files, the two steps a
//! request takes before issuance: [`EnrollmentStore::post`] and [`EnrollmentStore::approve`]
//! (or [`EnrollmentStore::approve_recorded`]), and the one it takes when no operator approves
//! it in time: [`EnrollmentStore::expire`].

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use prost::Message;

use super::files::{
    DenyState, HostRow, RequestRow, SCHEMA, claims_of_pb, claims_to_pb, pb, read_deny, read_issued,
    read_serial,
};
use super::mac_nonce::{MAC_NONCES, MacNonceHash};
use super::{
    Approval, ApprovalRecord, ApproveError, Claims, Decision, NewRequest, PostError, PostOutcome,
    RecordedApproval, RequestState, RequestView, StoreError,
};
use crate::ca::check_request;
use crate::fs::{FileLock, Fs, write_atomic};
use crate::names::{HostId, KeyHash, MacAddress};

/// Two daemons reporting one host id with different machine ids within this many seconds of
/// each other make the host a duplicate.
pub const LIVE_WINDOW_SECS: u64 = 600;

/// The serial of the first certificate the store hands out; 1 is the CA certificate's.
pub const FIRST_SERIAL: u64 = 2;

/// How many requests may wait for an operator, unless [`EnrollmentStore::with_max_pending`]
/// says otherwise (the coordinator's `--max-pending-enrollments`).
pub const DEFAULT_MAX_PENDING: usize = 256;

const REQUESTS: &str = "requests";
const ISSUED: &str = "issued";
const HOSTS: &str = "hosts";
const DENY: &str = "deny";
const SERIAL_FILE: &str = "serial.pb";
const LOCK_FILE: &str = "lock";

/// Every file is written owner-only.
const FILE_MODE: u32 = 0o600;

/// The enrollment store over one state directory (the coordinator's `--enrollment-dir`).
pub struct EnrollmentStore {
    fs: Arc<dyn Fs>,
    dir: PathBuf,
    mutex: Mutex<()>,
    /// Per host id, the machine ids that reported it live and when, within the window.
    pub(super) live: Mutex<BTreeMap<HostId, BTreeMap<String, u64>>>,
    /// The most requests that may wait for an operator.
    max_pending: usize,
    /// The Mac nonces an approval consumed: read from the approvals at open and added to by
    /// each approval, so a nonce whose own file was not yet marked is still used.
    pub(super) consumed: Mutex<BTreeSet<MacNonceHash>>,
}

impl std::fmt::Debug for EnrollmentStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EnrollmentStore({})", self.dir.display())
    }
}

/// The store's lock, held: the file lock is released before the mutex.
pub(super) struct Held<'a> {
    _file: FileLock,
    _guard: MutexGuard<'a, ()>,
}

pub(super) fn io_at(path: &Path) -> impl FnOnce(io::Error) -> StoreError + '_ {
    move |source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    }
}

impl EnrollmentStore {
    /// The store in `dir`, which is made (mode 0700) with its subdirectories when missing. A
    /// Mac nonce an approval consumed whose own file was not yet marked is marked now.
    ///
    /// # Errors
    /// [`StoreError::Io`] when a directory cannot be made; [`StoreError`] when a request or
    /// nonce file cannot be read or written.
    pub fn open(fs: Arc<dyn Fs>, dir: &Path) -> Result<EnrollmentStore, StoreError> {
        for sub in [REQUESTS, ISSUED, HOSTS, DENY, MAC_NONCES] {
            let path = dir.join(sub);
            fs.create_dir(&path).map_err(io_at(&path))?;
        }
        let store = EnrollmentStore {
            fs,
            dir: dir.to_path_buf(),
            mutex: Mutex::new(()),
            live: Mutex::new(BTreeMap::new()),
            max_pending: DEFAULT_MAX_PENDING,
            consumed: Mutex::new(BTreeSet::new()),
        };
        store.reconcile_mac_nonces()?;
        Ok(store)
    }

    /// The same store, letting at most `max` requests wait for an operator.
    #[must_use]
    pub fn with_max_pending(mut self, max: usize) -> EnrollmentStore {
        self.max_pending = max;
        self
    }

    /// The most requests that may wait for an operator.
    pub fn max_pending(&self) -> usize {
        self.max_pending
    }

    /// Takes the store's mutex, then the `flock` on its lock file.
    pub(super) fn lock(&self) -> Result<Held<'_>, StoreError> {
        let guard = self.mutex.lock().unwrap_or_else(PoisonError::into_inner);
        let path = self.dir.join(LOCK_FILE);
        let file = self.fs.lock(&path).map_err(io_at(&path))?;
        Ok(Held {
            _file: file,
            _guard: guard,
        })
    }

    pub(super) fn read(&self, path: &Path) -> Result<Option<Vec<u8>>, StoreError> {
        self.fs.read(path).map_err(io_at(path))
    }

    /// The path of `name` in the store's subdirectory `sub`.
    pub(super) fn path_in(&self, sub: &str, name: &str) -> PathBuf {
        self.dir.join(sub).join(name)
    }

    pub(super) fn write(&self, sub: &str, name: &str, bytes: &[u8]) -> Result<(), StoreError> {
        let dir = self.dir.join(sub);
        let path = dir.join(name);
        write_atomic(&*self.fs, &dir, name, bytes, FILE_MODE).map_err(io_at(&path))
    }

    fn request_path(&self, id: &KeyHash) -> PathBuf {
        self.dir.join(REQUESTS).join(format!("{id}.pb"))
    }

    pub(super) fn load_request(&self, id: &KeyHash) -> Result<Option<RequestRow>, StoreError> {
        let path = self.request_path(id);
        self.read(&path)?
            .map(|bytes| RequestRow::read(&path, &bytes, id))
            .transpose()
    }

    pub(super) fn write_request(&self, row: &RequestRow) -> Result<(), StoreError> {
        let name = format!("{}.pb", row.id);
        self.write(REQUESTS, &name, &row.pb.encode_to_vec())
    }

    /// Every request, in the order of their ids.
    pub(super) fn all_requests(&self) -> Result<Vec<RequestRow>, StoreError> {
        let dir = self.dir.join(REQUESTS);
        let names = self.fs.list(&dir).map_err(io_at(&dir))?;
        names
            .iter()
            .filter_map(|name| name.strip_suffix(".pb").and_then(KeyHash::from_hex))
            .map(|id| self.load_request(&id))
            .filter_map(Result::transpose)
            .collect()
    }

    pub(super) fn host_path(&self, host: &HostId) -> PathBuf {
        self.dir.join(HOSTS).join(format!("{host}.pb"))
    }

    pub(super) fn load_host(&self, host: &HostId) -> Result<Option<HostRow>, StoreError> {
        let path = self.host_path(host);
        self.read(&path)?
            .map(|bytes| HostRow::read(&path, &bytes))
            .transpose()
    }

    /// The host ids of every host file, sorted; a file whose name is no host id is left out.
    pub(super) fn host_ids(&self) -> Result<Vec<HostId>, StoreError> {
        let dir = self.dir.join(HOSTS);
        let names = self.fs.list(&dir).map_err(io_at(&dir))?;
        let mut ids: Vec<HostId> = names
            .iter()
            .filter_map(|name| name.strip_suffix(".pb"))
            .filter_map(|name| HostId::parse(name).ok())
            .collect();
        ids.sort();
        Ok(ids)
    }

    pub(super) fn write_host(&self, host: &pb::Host) -> Result<(), StoreError> {
        let name = format!("{}.pb", host.host_id);
        self.write(HOSTS, &name, &host.encode_to_vec())
    }

    pub(super) fn load_deny(&self, host: &HostId) -> Result<Option<DenyState>, StoreError> {
        let path = self.dir.join(DENY).join(format!("{host}.pb"));
        self.read(&path)?
            .map(|bytes| read_deny(&path, &bytes))
            .transpose()
    }

    pub(super) fn write_deny(
        &self,
        host: &HostId,
        state: DenyState,
        cleared_by: &[u8],
    ) -> Result<(), StoreError> {
        let message = pb::Deny {
            schema: SCHEMA,
            host_id: host.to_string(),
            state: state as i32,
            cleared_by: cleared_by.to_vec(),
        };
        self.write(DENY, &format!("{host}.pb"), &message.encode_to_vec())
    }

    /// The host id is denied: retiring or retired.
    pub(super) fn denied(&self, host: &HostId) -> Result<bool, StoreError> {
        Ok(self.load_deny(host)?.is_some_and(DenyState::is_active))
    }

    pub(super) fn load_issued(&self, id: &KeyHash) -> Result<Option<pb::Issued>, StoreError> {
        let path = self.dir.join(ISSUED).join(format!("{id}.pb"));
        self.read(&path)?
            .map(|bytes| read_issued(&path, &bytes))
            .transpose()
    }

    pub(super) fn write_issued(&self, issued: &pb::Issued, id: &KeyHash) -> Result<(), StoreError> {
        self.write(ISSUED, &format!("{id}.pb"), &issued.encode_to_vec())
    }

    /// Hands out the next serial and records the one after it. The caller holds the lock.
    pub(super) fn next_serial(&self) -> Result<u64, StoreError> {
        let path = self.dir.join(SERIAL_FILE);
        let next = self
            .read(&path)?
            .map(|bytes| read_serial(&path, &bytes))
            .transpose()?
            .unwrap_or(FIRST_SERIAL);
        let counter = pb::SerialCounter {
            schema: SCHEMA,
            next: next + 1,
        };
        self.write("", SERIAL_FILE, &counter.encode_to_vec())?;
        Ok(next)
    }

    /// A serial number no certificate of this store carries yet, for a certificate the
    /// coordinator has signed outside the store (its own server certificate). The counter is
    /// durable before the number is returned.
    ///
    /// # Errors
    /// [`StoreError`] when the counter cannot be read or written.
    pub fn allocate_serial(&self) -> Result<u64, StoreError> {
        let _held = self.lock()?;
        self.next_serial()
    }

    /// The request `id`, as the store holds it.
    ///
    /// # Errors
    /// [`StoreError`] when its file cannot be read.
    pub fn request(&self, id: &KeyHash) -> Result<Option<RequestView>, StoreError> {
        self.load_request(id)?.map(|row| view(&row)).transpose()
    }

    /// The requests waiting for an operator, duplicate claims included, in the order of their
    /// ids.
    ///
    /// # Errors
    /// [`StoreError`] when a file cannot be read.
    pub fn pending(&self) -> Result<Vec<RequestView>, StoreError> {
        self.all_requests()?
            .iter()
            .filter(|row| row.state.is_pending())
            .map(view)
            .collect()
    }

    /// The requests that expired ([`Self::expire`]), in the order of their ids.
    ///
    /// # Errors
    /// [`StoreError`] when a file cannot be read.
    pub fn expired(&self) -> Result<Vec<RequestView>, StoreError> {
        self.all_requests()?
            .iter()
            .filter(|row| row.state == RequestState::Expired)
            .map(view)
            .collect()
    }

    /// Expires every request that has waited for an operator (pending or a duplicate claim)
    /// more than `ttl_secs` at `now` (unix seconds): one posted at `now - ttl_secs` or later
    /// still waits. Each is marked [`RequestState::Expired`] at `now`, under the store lock, so
    /// an approval and the sweep never both take one request, and a request is expired once: a
    /// later sweep finds it no longer waiting. Returns the requests it expired, in the order of
    /// their ids, as the store now holds them.
    ///
    /// # Errors
    /// [`StoreError`] when a file cannot be read or written; the requests expired before the
    /// failure stay expired.
    pub fn expire(&self, now: u64, ttl_secs: u64) -> Result<Vec<RequestView>, StoreError> {
        let _held = self.lock()?;
        let mut expired = Vec::new();
        for mut row in self.all_requests()? {
            if row.state.is_pending() && now.saturating_sub(row.pb.posted_unix) > ttl_secs {
                row.expire(now, ttl_secs);
                self.write_request(&row)?;
                expired.push(view(&row)?);
            }
        }
        Ok(expired)
    }

    /// Records `request`, posted at `now` (unix seconds).
    ///
    /// The id is the SHA-256 of the CSR's key, computed here. The same key posted again is the
    /// same request: no new row and no new place in the queue. A key that comes back with
    /// another machine id while its request is pending flags that request as a duplicate
    /// claim. A new request whose machine id is another request's, or that claims a MAC bound to
    /// an enrolled host ([`EnrollmentStore::bound_macs`]), is a duplicate claim. A
    /// `renewal_of` must name an approved host that is not denied and holds this key; it then
    /// waits for one approval like any request, and when the key's certificate was already
    /// issued it starts the next round. Every duplicate claim emits a warn event
    /// (`duplicate enrollment claim`). A key whose request expired ([`Self::expire`]) starts
    /// over as a new request of the same round, waiting from `now`; an expired request's
    /// machine id makes no other request a duplicate claim.
    ///
    /// A request that would wait while [`Self::max_pending`] requests wait already is refused
    /// and nothing is written: no waiting request is dropped for it, and a repost of a waiting
    /// request is accepted as before. Approved, issued and denied requests do not count.
    ///
    /// # Errors
    /// [`PostError::Profile`] for a CSR outside the profile, [`PostError::RenewalNotAllowed`],
    /// [`PostError::PendingFull`], [`PostError::Store`].
    pub fn post(&self, request: NewRequest, now: u64) -> Result<PostOutcome, PostError> {
        let verified = check_request(&request.csr_der)?;
        let id = verified.key().key_hash();
        let _held = self.lock()?;
        let host = match &request.renewal_of {
            Some(host_id) => Some(self.renewal_host(host_id, &id)?),
            None => None,
        };
        let existing = self.load_request(&id)?;
        let next_round = host.is_some()
            && existing
                .as_ref()
                .is_some_and(|row| row.state == RequestState::Issued);
        let round = match existing {
            Some(row) if row.state == RequestState::Expired => row.pb.round,
            Some(row) if !next_round => return self.repost(row, &request).map_err(PostError::from),
            Some(row) => row.pb.round + 1,
            None => 0,
        };
        let all = self.all_requests()?;
        if all.iter().filter(|row| row.state.is_pending()).count() >= self.max_pending {
            return Err(PostError::PendingFull);
        }
        let reason = match &host {
            Some(host) => (host.pb.machine_id != request.claims.machine_id)
                .then_some("the renewal's machine id is not the host's"),
            None => match machine_id_taken(&all, &request.claims.machine_id) {
                Some(reason) => Some(reason),
                None => self.mac_bound(&request.claims)?,
            },
        };
        let state = reason.map_or(RequestState::Pending, |_| RequestState::DuplicateClaim);
        let row = RequestRow {
            path: self.request_path(&id),
            pb: pb::Request {
                schema: SCHEMA,
                request_id: id.as_bytes().to_vec(),
                csr_der: request.csr_der,
                claims: Some(claims_to_pb(&request.claims)),
                image: request.image,
                posted_unix: now,
                renewal_of: request
                    .renewal_of
                    .as_ref()
                    .map(HostId::to_string)
                    .unwrap_or_default(),
                state: state as i32,
                approved: None,
                spki_der: verified.key().spki_der().to_vec(),
                round,
                serial: 0,
                mac_nonce_presented: request.mac_nonce_presented,
                expired_unix: 0,
                expired_after_secs: 0,
            },
            id,
            key: verified.key().clone(),
            state,
        };
        self.write_request(&row)?;
        if let Some(reason) = reason {
            alert(&id, &request.claims.machine_id, reason);
        }
        Ok(PostOutcome {
            request_id: id,
            state,
            renewal_of: request.renewal_of,
            round,
            reposted: false,
        })
    }

    /// The host a `renewal_of` names, when it may renew with key `id`.
    fn renewal_host(&self, host: &HostId, id: &KeyHash) -> Result<HostRow, PostError> {
        let denied = self.denied(host)?;
        self.load_host(host)?
            .filter(|row| row.key == *id && !denied)
            .ok_or(PostError::RenewalNotAllowed)
    }

    /// Why a new request is a duplicate claim for the MACs it claims: one of them is bound to an
    /// enrolled host. Only universally administered unicast MACs count
    /// ([`MacAddress::is_universal_unicast`]).
    fn mac_bound(&self, claims: &Claims) -> Result<Option<&'static str>, StoreError> {
        let claimed: Vec<MacAddress> = claims
            .all_macs()
            .into_iter()
            .filter(MacAddress::is_universal_unicast)
            .collect();
        if claimed.is_empty() {
            return Ok(None);
        }
        let bound = self.bound_macs()?;
        let taken = claimed.iter().any(|mac| bound.contains(mac));
        Ok(taken.then_some("a MAC is bound to an enrolled host"))
    }

    /// The MACs bound to enrolled hosts: the universally administered unicast MACs (data and
    /// BMC) claimed by the request each approved host's key came from, for every host that is
    /// not denied.
    ///
    /// # Errors
    /// [`StoreError`] when a file cannot be read, or a host's request is missing.
    pub fn bound_macs(&self) -> Result<BTreeSet<MacAddress>, StoreError> {
        let dir = self.dir.join(HOSTS);
        let names = self.fs.list(&dir).map_err(io_at(&dir))?;
        let mut rows = Vec::new();
        for host in names
            .iter()
            .filter_map(|name| name.strip_suffix(".pb"))
            .filter_map(|name| HostId::parse(name).ok())
        {
            if !self.denied(&host)? {
                rows.extend(self.load_host(&host)?.map(|row| (host, row)));
            }
        }
        let mut bound = BTreeSet::new();
        for (host, row) in rows {
            let request = self.load_request(&row.key)?.ok_or_else(|| {
                super::files::corrupt(&self.host_path(&host), "the host's request is missing")
            })?;
            let claims = claims_of_pb(request.pb.claims.as_ref());
            bound.extend(
                claims
                    .all_macs()
                    .into_iter()
                    .filter(MacAddress::is_universal_unicast),
            );
        }
        Ok(bound)
    }

    /// The same key posted again: the same request, flagged when its machine id changed, and
    /// marked as having presented a Mac nonce when it waits and does now.
    fn repost(&self, mut row: RequestRow, request: &NewRequest) -> Result<PostOutcome, StoreError> {
        let changed = row.machine_id() != request.claims.machine_id;
        let flag = changed && row.state == RequestState::Pending;
        let presented =
            request.mac_nonce_presented && !row.pb.mac_nonce_presented && row.state.is_pending();
        if flag {
            row.set_state(RequestState::DuplicateClaim);
        }
        if presented {
            row.pb.mac_nonce_presented = true;
        }
        if flag || presented {
            self.write_request(&row)?;
        }
        if changed {
            alert(
                &row.id,
                &request.claims.machine_id,
                "the same key came back with another machine id",
            );
        }
        Ok(PostOutcome {
            request_id: row.id,
            state: row.state,
            renewal_of: row.renewal_of()?,
            round: row.pb.round,
            reposted: true,
        })
    }

    /// Approves the pending request `request_id` as `decision` says.
    ///
    /// `key_hash` is the SHA-256 of the key the operator saw; it must be the request's. Under
    /// the store lock, in this order: the host id must not be denied and must not be held by
    /// another key, unless `decision.replace` (not for a renewal, which must name the host it
    /// renews); then a deny row it clears is marked cleared by this request, the host row is
    /// written (host id, key hash, machine id, role, services), and the request is marked
    /// approved. A crash between those writes leaves a host row of this key, which a retry
    /// accepts as its own. It does not issue. The key hash is compared in constant time.
    ///
    /// # Errors
    /// [`ApproveError`]; nothing is written when one is returned before the writes.
    pub fn approve(
        &self,
        request_id: &KeyHash,
        key_hash: &KeyHash,
        decision: &Decision,
    ) -> Result<Approval, ApproveError> {
        let _held = self.lock()?;
        self.approve_held(request_id, key_hash, decision, None)
    }

    /// [`Self::approve`] with `record` written into the approval: who approved, when and why.
    /// Under the same lock, before anything is written, it also refuses a request flagged
    /// `duplicate_claim` unless `decision.replace` ([`ApproveError::DuplicateClaim`]), and
    /// checks the Mac nonce of `record`, if any, at `record.approved_unix`: issued, unexpired,
    /// unused, and for the serial the request claims ([`ApproveError::MacNonce`]). The request
    /// file that marks the request approved is the one write that commits the approval and
    /// consumes the nonce; the nonce's own file is marked after it (and at the next
    /// [`Self::open`], should that write not land).
    ///
    /// # Errors
    /// [`ApproveError`]; nothing is written when one is returned before the writes.
    pub fn approve_recorded(
        &self,
        request_id: &KeyHash,
        key_hash: &KeyHash,
        decision: &Decision,
        record: &ApprovalRecord,
    ) -> Result<Approval, ApproveError> {
        let _held = self.lock()?;
        self.approve_held(request_id, key_hash, decision, Some(record))
    }

    /// The approvals recorded with an approver ([`Self::approve_recorded`]), in the order of
    /// their request ids: what the coordinator audits, and audits again at a start when its
    /// entry is missing.
    ///
    /// # Errors
    /// [`StoreError`] when a request file cannot be read.
    pub fn recorded_approvals(&self) -> Result<Vec<RecordedApproval>, StoreError> {
        Ok(self
            .all_requests()?
            .iter()
            .filter_map(|row| {
                let approved = row.pb.approved.as_ref()?;
                (!approved.approver.is_empty()).then(|| RecordedApproval {
                    request_id: row.id,
                    host_id: approved.host_id.clone(),
                    role: approved.role.clone(),
                    services: approved.services.clone(),
                    replace: approved.replace,
                    approver: approved.approver.clone(),
                    approved_unix: approved.approved_unix,
                    reason: approved.reason.clone(),
                    claims: claims_of_pb(row.pb.claims.as_ref()),
                    mac_nonce: !approved.mac_nonce_sha256.is_empty(),
                })
            })
            .collect())
    }

    /// The approval of [`Self::approve`] and [`Self::approve_recorded`]; the caller holds the
    /// lock.
    fn approve_held(
        &self,
        request_id: &KeyHash,
        key_hash: &KeyHash,
        decision: &Decision,
        record: Option<&ApprovalRecord>,
    ) -> Result<Approval, ApproveError> {
        let mut row = self
            .load_request(request_id)?
            .ok_or(ApproveError::UnknownRequest)?;
        if !key_hash.equals_in_constant_time(request_id) {
            return Err(ApproveError::KeyMismatch);
        }
        if !row.state.is_pending() {
            return Err(ApproveError::NotPending(row.state));
        }
        if record.is_some() && row.state == RequestState::DuplicateClaim && !decision.replace {
            return Err(ApproveError::DuplicateClaim);
        }
        let nonce = match record.and_then(|r| r.mac_nonce.map(|hash| (hash, r.approved_unix))) {
            Some((hash, at)) => Some((hash, self.mac_nonce_verdict(&hash, row.mac_serial(), at)??)),
            None => None,
        };
        let renewal = row.renewal_of()?;
        let host = &decision.host_id;
        if renewal.as_ref().is_some_and(|of| of != host) {
            return Err(ApproveError::RenewalOfOtherHost);
        }
        let replace = decision.replace && renewal.is_none();
        let denied = self.denied(host)?;
        let current = self.load_host(host)?;
        if denied && !replace {
            return Err(ApproveError::Denied);
        }
        if current.as_ref().is_some_and(|h| h.key != *request_id) && !replace {
            return Err(ApproveError::HostIdTaken);
        }
        if denied {
            self.write_deny(host, DenyState::Cleared, request_id.as_bytes())?;
        }
        let services: Vec<String> = decision
            .services
            .iter()
            .map(|s| s.as_str().to_owned())
            .collect();
        self.write_host(&pb::Host {
            schema: SCHEMA,
            host_id: host.to_string(),
            key_hash: request_id.as_bytes().to_vec(),
            machine_id: row.machine_id().to_owned(),
            role: decision.role.as_str().to_owned(),
            services: services.clone(),
            // Only a new key clears a duplicate mark; a renewal keeps the host's.
            duplicate: renewal.is_some() && current.is_some_and(|h| h.pb.duplicate),
        })?;
        self.live
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(host);
        row.set_state(RequestState::Approved);
        row.pb.approved = Some(pb::Approval {
            host_id: host.to_string(),
            role: decision.role.as_str().to_owned(),
            services,
            replace: decision.replace,
            approver: record.map(|r| r.approver.clone()).unwrap_or_default(),
            approved_unix: record.map_or(0, |r| r.approved_unix),
            reason: record.map(|r| r.reason.clone()).unwrap_or_default(),
            mac_nonce_sha256: nonce
                .as_ref()
                .map(|(hash, _)| hash.as_bytes().to_vec())
                .unwrap_or_default(),
        });
        self.write_request(&row)?;
        if let Some((hash, file)) = nonce {
            self.consume_mac_nonce(&hash, file, request_id);
        }
        Ok(Approval {
            request_id: *request_id,
            host_id: host.clone(),
            role: decision.role.clone(),
            services: decision.services.clone(),
            renewal: renewal.is_some(),
            cleared_deny: denied,
        })
    }
}

/// Why a new request with `machine_id` is a duplicate claim, if another of `all` that has not
/// expired has it (a key whose request expired, posted again, is not another).
fn machine_id_taken(all: &[RequestRow], machine_id: &str) -> Option<&'static str> {
    let taken = !machine_id.is_empty()
        && all
            .iter()
            .any(|row| row.state != RequestState::Expired && row.machine_id() == machine_id);
    taken.then_some("another request has this machine id")
}

/// The warn event of a duplicate claim. The machine id is not a secret; no key or claim but
/// the machine id is logged.
fn alert(id: &KeyHash, machine_id: &str, reason: &str) {
    tracing::warn!(request_id = %id, machine_id, reason, "duplicate enrollment claim");
}

/// The public view of a request row.
fn view(row: &RequestRow) -> Result<RequestView, StoreError> {
    Ok(RequestView {
        request_id: row.id,
        state: row.state,
        claims: claims_of_pb(row.pb.claims.as_ref()),
        image: row.pb.image.clone(),
        posted_unix: row.pb.posted_unix,
        renewal_of: row.renewal_of()?,
        round: row.pb.round,
        host_id: row.host_id()?,
        mac_nonce_presented: row.pb.mac_nonce_presented,
        expiry: row.expiry(),
    })
}
