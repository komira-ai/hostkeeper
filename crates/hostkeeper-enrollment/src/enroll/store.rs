//! The store itself: its records, its lock, reading and checking each record, the two steps a
//! request takes before issuance: [`EnrollmentStore::post`] and [`EnrollmentStore::approve`]
//! (or [`EnrollmentStore::approve_recorded`]), and the one it takes when no operator approves
//! it in time: [`EnrollmentStore::expire`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::records::{FileRecords, Record, RecordKey, Records, RecordsLock, pb};
use super::rows::{
    DenyState, HostRow, RequestRow, SCHEMA, claims_of_pb, claims_to_pb, corrupt, deny_state,
};
use super::{
    Approval, ApprovalRecord, ApproveError, Claims, Decision, NewRequest, PostError, PostOutcome,
    RecordedApproval, RequestState, RequestView, StoreError,
};
use crate::ca::check_request;
use crate::fs::Fs;
use crate::names::{HostId, KeyHash, MacAddress};

/// Two daemons reporting one host id with different machine ids within this many seconds of
/// each other make the host a duplicate.
pub const LIVE_WINDOW_SECS: u64 = 600;

/// The serial of the first certificate the store hands out; 1 is the CA certificate's.
pub const FIRST_SERIAL: u64 = 2;

/// How many requests may wait for an operator, unless [`EnrollmentStore::with_max_pending`]
/// says otherwise (the coordinator's `--max-pending-enrollments`).
pub const DEFAULT_MAX_PENDING: usize = 256;

/// The enrollment store over its [`Records`]: the files of one state directory (the
/// coordinator's `--enrollment-dir`, [`EnrollmentStore::open`]), or any other
/// ([`EnrollmentStore::new`]).
pub struct EnrollmentStore {
    records: Arc<dyn Records>,
    mutex: Mutex<()>,
    /// Per host id, the machine ids that reported it live and when, within the window.
    pub(super) live: Mutex<BTreeMap<HostId, BTreeMap<String, u64>>>,
    /// The most requests that may wait for an operator.
    max_pending: usize,
    /// The hosts whose host record is known to hold a first heartbeat since its approval, so a
    /// later heartbeat takes neither the lock nor a read ([`EnrollmentStore::note_heartbeat`]).
    /// An approval of the host takes it out.
    pub(super) heard: Mutex<BTreeSet<HostId>>,
    /// The hosts whose host record is known to hold the first heartbeat that reported a
    /// managed service installed and healthy ([`EnrollmentStore::note_serving`]); an approval
    /// of the host takes it out.
    pub(super) served: Mutex<BTreeSet<HostId>>,
}

impl std::fmt::Debug for EnrollmentStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EnrollmentStore({:?})", self.records)
    }
}

/// The store's lock, held: the records' lock is released before the mutex.
pub(super) struct Held<'a> {
    _records: RecordsLock,
    _guard: MutexGuard<'a, ()>,
}

impl EnrollmentStore {
    /// The store over the files in `dir` ([`FileRecords::open`]), which is made (mode 0700)
    /// with its subdirectories when missing. A Mac nonce an approval consumed whose own file was
    /// not yet marked is marked now.
    ///
    /// # Errors
    /// [`StoreError::Io`] when a directory cannot be made; [`StoreError`] when a request or
    /// nonce file cannot be read or written.
    pub fn open(fs: Arc<dyn Fs>, dir: &Path) -> Result<EnrollmentStore, StoreError> {
        Ok(EnrollmentStore::new(Arc::new(FileRecords::open(fs, dir)?)))
    }

    /// The store over `records`.
    pub fn new(records: Arc<dyn Records>) -> EnrollmentStore {
        EnrollmentStore {
            records,
            mutex: Mutex::new(()),
            live: Mutex::new(BTreeMap::new()),
            max_pending: DEFAULT_MAX_PENDING,
            heard: Mutex::new(BTreeSet::new()),
            served: Mutex::new(BTreeSet::new()),
        }
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

    /// Takes the store's mutex, then the records' lock.
    pub(super) fn lock(&self) -> Result<Held<'_>, StoreError> {
        let guard = self.mutex.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(Held {
            _records: self.records.lock()?,
            _guard: guard,
        })
    }

    pub(super) fn records(&self) -> &dyn Records {
        &*self.records
    }

    /// Writes `records` in one commit.
    pub(super) fn commit(&self, records: &[Record]) -> Result<(), StoreError> {
        self.records.commit(records)
    }

    pub(super) fn load_request(&self, id: &KeyHash) -> Result<Option<RequestRow>, StoreError> {
        self.records
            .request(id)?
            .map(|pb| RequestRow::check(self.records.place(&RecordKey::Request(*id)), pb, id))
            .transpose()
    }

    /// Every request, checked, in the order of their ids.
    pub(super) fn all_requests(&self) -> Result<Vec<RequestRow>, StoreError> {
        self.records
            .requests()?
            .into_iter()
            .map(|(id, pb)| RequestRow::check(self.records.place(&RecordKey::Request(id)), pb, &id))
            .collect()
    }

    pub(super) fn host_path(&self, host: &HostId) -> std::path::PathBuf {
        self.records.place(&RecordKey::Host(host.clone()))
    }

    pub(super) fn load_host(&self, host: &HostId) -> Result<Option<HostRow>, StoreError> {
        self.records
            .host(host)?
            .map(|pb| HostRow::check(&self.host_path(host), pb))
            .transpose()
    }

    /// The host ids of every host record, sorted.
    pub(super) fn host_ids(&self) -> Result<Vec<HostId>, StoreError> {
        self.records.host_ids()
    }

    pub(super) fn load_deny(&self, host: &HostId) -> Result<Option<DenyState>, StoreError> {
        self.records
            .deny(host)?
            .map(|pb| deny_state(&self.records.place(&RecordKey::Deny(host.clone())), &pb))
            .transpose()
    }

    /// The deny row of `host` in `state`, cleared by the request `cleared_by` (none: empty).
    pub(super) fn deny_record(host: &HostId, state: DenyState, cleared_by: &[u8]) -> Record {
        Record::Deny(pb::Deny {
            schema: SCHEMA,
            host_id: host.to_string(),
            state: state as i32,
            cleared_by: cleared_by.to_vec(),
        })
    }

    /// The host id is denied: retiring or retired.
    pub(super) fn denied(&self, host: &HostId) -> Result<bool, StoreError> {
        Ok(self.load_deny(host)?.is_some_and(DenyState::is_active))
    }

    /// A serial number no certificate of this store carries yet, for a certificate the
    /// coordinator has signed outside the store (its own server certificate). The counter is
    /// durable before the number is returned.
    ///
    /// # Errors
    /// [`StoreError`] when the counter cannot be read or written.
    pub fn allocate_serial(&self) -> Result<u64, StoreError> {
        let _held = self.lock()?;
        self.records.take_serial(None)
    }

    /// The request `id`, as the store holds it.
    ///
    /// # Errors
    /// [`StoreError`] when its record cannot be read.
    pub fn request(&self, id: &KeyHash) -> Result<Option<RequestView>, StoreError> {
        self.load_request(id)?.map(|row| view(&row)).transpose()
    }

    /// The requests waiting for an operator, duplicate claims included, in the order of their
    /// ids.
    ///
    /// # Errors
    /// [`StoreError`] when a record cannot be read.
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
    /// [`StoreError`] when a record cannot be read.
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
    /// [`StoreError`] when a record cannot be read or written. Every request it expires is
    /// written in one commit: over the files, the ones written before a failure stay expired;
    /// over records that commit all or nothing, none does.
    pub fn expire(&self, now: u64, ttl_secs: u64) -> Result<Vec<RequestView>, StoreError> {
        let _held = self.lock()?;
        let mut expired = Vec::new();
        let mut records = Vec::new();
        for mut row in self.all_requests()? {
            if row.state.is_pending() && now.saturating_sub(row.pb.posted_unix) > ttl_secs {
                row.expire(now, ttl_secs);
                expired.push(view(&row)?);
                records.push(Record::Request(row.pb));
            }
        }
        self.commit(&records)?;
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
            path: self.records.place(&RecordKey::Request(id)),
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
        self.commit(&[Record::Request(row.pb)])?;
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
    /// [`StoreError`] when a record cannot be read, or a host's request is missing.
    pub fn bound_macs(&self) -> Result<BTreeSet<MacAddress>, StoreError> {
        let mut rows = Vec::new();
        for host in self.host_ids()? {
            if !self.denied(&host)? {
                rows.extend(self.load_host(&host)?.map(|row| (host, row)));
            }
        }
        let mut bound = BTreeSet::new();
        for (host, row) in rows {
            let request = self
                .load_request(&row.key)?
                .ok_or_else(|| corrupt(&self.host_path(&host), "the host's request is missing"))?;
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
            self.commit(&[Record::Request(row.pb.clone())])?;
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
    /// unused, for the serial the request claims, and for this request when the nonce names one
    /// ([`ApproveError::MacNonce`]). The commit that marks the request approved consumes the
    /// nonce and marks its record consumed (over the files the request's file is the commit
    /// point, and the nonce's own file is marked
    /// after it, and at the next [`Self::open`] should that write not land).
    ///
    /// An approval with a Mac nonce takes over a host id another key holds, as `replace` would,
    /// when the Mac enrolled in MDM anew since that key was approved: `record` says when its
    /// current enrollment began ([`ApprovalRecord::mac_enrolled_since_unix`]), that key's own
    /// request claimed the same Mac serial, and its approval was recorded before that time. It
    /// never takes a denied host id, and never lifts a `duplicate_claim`. The approval is
    /// recorded as replacing ([`Approval::replace`]).
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
    /// [`StoreError`] when a request record cannot be read.
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
            Some((hash, at)) => Some((
                hash,
                self.mac_nonce_verdict(&hash, request_id, row.mac_serial(), at)??,
            )),
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
        let holder = current.as_ref().filter(|h| h.key != *request_id);
        let rejoined = match (holder, record.and_then(|r| r.mac_enrolled_since_unix)) {
            // A renewal's key is its host's: it has no other holder.
            (Some(holder), Some(since)) if nonce.is_some() => {
                self.mac_rejoined(holder, row.mac_serial(), since)?
            }
            _ => false,
        };
        if holder.is_some() && !replace && !rejoined {
            return Err(ApproveError::HostIdTaken);
        }
        let replaced = decision.replace || rejoined;
        let mut records = Vec::new();
        if denied {
            records.push(Self::deny_record(
                host,
                DenyState::Cleared,
                request_id.as_bytes(),
            ));
        }
        let services: Vec<String> = decision
            .services
            .iter()
            .map(|s| s.as_str().to_owned())
            .collect();
        records.push(Record::Host(pb::Host {
            schema: SCHEMA,
            host_id: host.to_string(),
            key_hash: request_id.as_bytes().to_vec(),
            machine_id: row.machine_id().to_owned(),
            role: decision.role.as_str().to_owned(),
            services: services.clone(),
            // Only a new key clears a duplicate mark; a renewal keeps the host's.
            duplicate: renewal.is_some() && current.is_some_and(|h| h.pb.duplicate),
            // Every approval, a renewal's too, waits for its own first heartbeat, and for its own
            // first heartbeat that reports a managed service serving.
            first_heartbeat_unix: None,
            first_serving_unix: None,
        }));
        row.set_state(RequestState::Approved);
        row.pb.approved = Some(pb::Approval {
            host_id: host.to_string(),
            role: decision.role.as_str().to_owned(),
            services,
            replace: replaced,
            approver: record.map(|r| r.approver.clone()).unwrap_or_default(),
            approved_unix: record.map_or(0, |r| r.approved_unix),
            reason: record.map(|r| r.reason.clone()).unwrap_or_default(),
            mac_nonce_sha256: nonce
                .as_ref()
                .map(|(hash, _)| hash.as_bytes().to_vec())
                .unwrap_or_default(),
        });
        records.push(Record::Request(row.pb));
        if let Some((_, mut file)) = nonce {
            // The approval above commits the consumption; the nonce's own record says so too.
            file.consumed_by = request_id.as_bytes().to_vec();
            records.push(Record::MacNonce(file));
        }
        // Forgotten before the commit: a host written again with no first heartbeat must not be
        // answered from memory, whether or not the rest of the commit lands.
        self.live
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(host);
        self.heard
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(host);
        self.served
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(host);
        self.commit(&records)?;
        Ok(Approval {
            request_id: *request_id,
            host_id: host.clone(),
            role: decision.role.clone(),
            services: decision.services.clone(),
            renewal: renewal.is_some(),
            cleared_deny: denied,
            replace: replaced,
        })
    }

    /// Whether the host id `holder` holds may go to a new key of the Mac of `serial`, whose
    /// current MDM enrollment began at `since`: the holder's own request claimed `serial` (not
    /// empty), and its approval was recorded (with a time) before `since`. A holder whose
    /// request is missing may not. The caller holds the lock.
    fn mac_rejoined(&self, holder: &HostRow, serial: &str, since: u64) -> Result<bool, StoreError> {
        let Some(own) = self.load_request(&holder.key)? else {
            return Ok(false);
        };
        let approved_unix = own.pb.approved.as_ref().map_or(0, |a| a.approved_unix);
        Ok(!serial.is_empty() && own.mac_serial() == serial && (1..since).contains(&approved_unix))
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
pub(super) fn view(row: &RequestRow) -> Result<RequestView, StoreError> {
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
