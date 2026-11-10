//! What the store knows of an approved host: deny rows (design 6.1), renewal, and duplicate
//! live daemons on one host id (design 5.7, step 4).
//!
//! A deny row is written by [`EnrollmentStore::deny`] (the operator's `Hosts.Retire`) as
//! retiring: the host is refused at once
//! (`is_denied`), and an update may still be in flight on it (`is_retiring`). The coordinator
//! frees the host's update slot when the host reports that update's record final, and then
//! calls [`EnrollmentStore::retire`]; the store never touches the slot. A deny row is never
//! removed: only an approval with `replace` marks it cleared, and records which request did.

use std::sync::PoisonError;

use super::records::Record;
use super::rows::{DenyState, claims_of_pb, corrupt};
use super::store::{EnrollmentStore, LIVE_WINDOW_SECS};
use super::{EnrolledHost, HostHeld, HostRunning, RequestState, StoreError};
use crate::names::{HostId, KeyHash, Role};

impl EnrollmentStore {
    /// Denies `host`: a retiring deny row (a row already retiring or retired is kept), and the
    /// host's request marked denied, so a repost or poll of its key is refused. Returns whether
    /// the row is new: `false` when the host was denied already, decided under the store's lock,
    /// so of two calls at once exactly one sees `true`.
    ///
    /// # Errors
    /// [`StoreError`] when a record cannot be read or written.
    pub fn deny(&self, host: &HostId) -> Result<bool, StoreError> {
        let _held = self.lock()?;
        let new = !self.denied(host)?;
        let row = new.then(|| Self::deny_record(host, DenyState::Retiring, &[]));
        self.commit_denied(host, row.into_iter().collect())?;
        Ok(new)
    }

    /// Records that `host` retired: its deny row becomes final (written denied when it was not
    /// yet), so `is_retiring` is false and `is_denied` stays true.
    ///
    /// # Errors
    /// [`StoreError`] when a record cannot be read or written.
    pub fn retire(&self, host: &HostId) -> Result<(), StoreError> {
        let _held = self.lock()?;
        self.commit_denied(host, vec![Self::deny_record(host, DenyState::Retired, &[])])
    }

    /// Commits `rows` (the deny row, if any) with `host`'s request marked denied. When the
    /// request cannot be read, the deny row is committed alone and the error returned: the host
    /// is refused whatever else fails.
    fn commit_denied(&self, host: &HostId, mut rows: Vec<Record>) -> Result<(), StoreError> {
        match self.request_denied(host) {
            Ok(request) => {
                rows.extend(request);
                self.commit(&rows)
            }
            Err(why) => {
                self.commit(&rows)?;
                Err(why)
            }
        }
    }

    /// The request of `host`'s key marked denied; `None` for a host id never approved.
    fn request_denied(&self, host: &HostId) -> Result<Option<Record>, StoreError> {
        let Some(row) = self.load_host(host)? else {
            return Ok(None);
        };
        let missing = corrupt(&self.host_path(host), "the host's request is missing");
        let mut request = self.load_request(&row.key)?.ok_or(missing)?;
        request.set_state(RequestState::Denied);
        Ok(Some(Record::Request(request.pb)))
    }

    /// Whether `host` is denied (retiring or retired): what the heartbeat handshake and
    /// `Renew` check.
    ///
    /// # Errors
    /// [`StoreError`] when its deny row cannot be read.
    pub fn is_denied(&self, host: &HostId) -> Result<bool, StoreError> {
        self.denied(host)
    }

    /// The role `host` was approved for; `None` for a host id never approved. The heartbeat
    /// path reads it as the role of a caller whose certificate names the host.
    ///
    /// # Errors
    /// [`StoreError`] when the host record cannot be read, or holds no role.
    pub fn host_role(&self, host: &HostId) -> Result<Option<Role>, StoreError> {
        let Some(row) = self.load_host(host)? else {
            return Ok(None);
        };
        Role::parse(&row.pb.role)
            .map(Some)
            .map_err(|e| corrupt(&self.host_path(host), &e.to_string()))
    }

    /// The role `host` was approved for, when its approval is for the key `key_hash`; `None`
    /// for a host id never approved, or taken over by another key since (an approval with
    /// `replace`): a certificate of the key before names the host no longer. The heartbeat
    /// path reads it for the key of the certificate a caller presents.
    ///
    /// # Errors
    /// [`StoreError`] when the host record cannot be read, or holds no role.
    pub fn host_role_for(
        &self,
        host: &HostId,
        key_hash: &KeyHash,
    ) -> Result<Option<Role>, StoreError> {
        let holds = self
            .load_host(host)?
            .is_some_and(|row| row.key.equals_in_constant_time(key_hash));
        if !holds {
            return Ok(None);
        }
        self.host_role(host)
    }

    /// `host` as enrolled: its host record names the key it was approved for, and the request of
    /// that key holds what it claimed and when it was approved. `None` for a host id never
    /// approved, or denied (retiring or retired).
    ///
    /// # Errors
    /// [`StoreError`] when a record cannot be read, or the host's request is missing.
    pub fn enrolled_host(&self, host: &HostId) -> Result<Option<EnrolledHost>, StoreError> {
        let Some(row) = self.load_host(host)? else {
            return Ok(None);
        };
        if self.denied(host)? {
            return Ok(None);
        }
        let role = Role::parse(&row.pb.role)
            .map_err(|e| corrupt(&self.host_path(host), &e.to_string()))?;
        let missing = corrupt(&self.host_path(host), "the host's request is missing");
        let request = self.load_request(&row.key)?.ok_or(missing)?;
        Ok(Some(EnrolledHost {
            host_id: host.clone(),
            claims: claims_of_pb(request.pb.claims.as_ref()),
            approved_unix: request.pb.approved.as_ref().map_or(0, |a| a.approved_unix),
            first_heartbeat_unix: row.pb.first_heartbeat_unix,
            first_serving_unix: row.pb.first_serving_unix,
            role,
            host_group: row.pb.host_group.clone(),
            moving_from: row.pb.moving_from.clone(),
            running: row.pb.running.as_ref().map(HostRunning::of_pb),
            running_unix: row.pb.running.as_ref().map(|r| r.at_unix),
        }))
    }

    /// The host ids of every enrolled host ([`Self::enrolled_host`]): those with a host record
    /// that are not denied (retiring or retired), by host id. Reads no request, so a page of
    /// hosts can be chosen without reading them all.
    ///
    /// # Errors
    /// [`StoreError`] when a host record or deny row cannot be read.
    pub fn enrolled_host_ids(&self) -> Result<Vec<HostId>, StoreError> {
        let mut ids = Vec::new();
        for host in self.host_ids()? {
            if !self.denied(&host)? {
                ids.push(host);
            }
        }
        Ok(ids)
    }

    /// Every enrolled host ([`Self::enrolled_host`]), by host id.
    ///
    /// # Errors
    /// [`StoreError`] when a record cannot be read, or a host's request is missing.
    pub fn enrolled_hosts(&self) -> Result<Vec<EnrolledHost>, StoreError> {
        let mut enrolled = Vec::new();
        for host in self.host_ids()? {
            enrolled.extend(self.enrolled_host(&host)?);
        }
        Ok(enrolled)
    }

    /// What the store holds for `host` ([`HostHeld`]): its deny row when it has one that is not
    /// cleared, else an approved host when its host record is there, else nothing. A deny row
    /// cleared by an approval with `replace` is the approved host of that approval. Read
    /// without the store's lock.
    ///
    /// # Errors
    /// [`StoreError`] when its deny row or host record cannot be read or decoded.
    pub fn held(&self, host: &HostId) -> Result<HostHeld, StoreError> {
        match self.load_deny(host)? {
            Some(DenyState::Retiring) => return Ok(HostHeld::Denied),
            Some(DenyState::Retired) => return Ok(HostHeld::Retired),
            Some(DenyState::Cleared) | None => {}
        }
        Ok(match self.load_host(host)? {
            Some(_) => HostHeld::Approved,
            None => HostHeld::None,
        })
    }

    /// Whether `host` is denied and may still have an update in flight.
    ///
    /// # Errors
    /// [`StoreError`] when its deny row cannot be read.
    pub fn is_retiring(&self, host: &HostId) -> Result<bool, StoreError> {
        Ok(self.load_deny(host)? == Some(DenyState::Retiring))
    }

    /// Whether `key_hash` may renew the certificate of `host`: the host was approved for that
    /// key and is not denied.
    ///
    /// # Errors
    /// [`StoreError`] when a record cannot be read.
    pub fn renew_allowed(&self, host: &HostId, key_hash: &KeyHash) -> Result<bool, StoreError> {
        let denied = self.denied(host)?;
        let holds = self
            .load_host(host)?
            .is_some_and(|row| row.key == *key_hash);
        Ok(holds && !denied)
    }

    /// Whether `host` is marked duplicate: two live daemons with different machine ids
    /// reported it within one window. The mark is durable, and only an approval of a new key
    /// for the host id (`replace`) clears it.
    ///
    /// # Errors
    /// [`StoreError`] when the host record cannot be read.
    pub fn is_duplicate(&self, host: &HostId) -> Result<bool, StoreError> {
        Ok(self.load_host(host)?.is_some_and(|row| row.pb.duplicate))
    }

    /// The heartbeat path's report that it accepted a heartbeat of `host` at `now`: the first
    /// since the host's approval is written into its host record, and every later one leaves it
    /// as it is. An approval writes the host record without it, so a host approved again (a new
    /// key, or a renewal) records its next heartbeat. A host id never approved has no record and
    /// nothing is recorded.
    ///
    /// Decided under the store's lock, so of two heartbeats at once exactly one is written. A
    /// host whose record is known to hold the time already (this store wrote or read it) is
    /// answered from memory, without the lock or a read; an approval of the host forgets it.
    ///
    /// # Errors
    /// [`StoreError`] when the host record cannot be read or written.
    pub fn note_heartbeat(&self, host: &HostId, now: u64) -> Result<(), StoreError> {
        let heard = || self.heard.lock().unwrap_or_else(PoisonError::into_inner);
        if heard().contains(host) {
            return Ok(());
        }
        let _held = self.lock()?;
        let Some(row) = self.load_host(host)? else {
            return Ok(());
        };
        if row.pb.first_heartbeat_unix.is_none() {
            self.records().set_first_heartbeat(&row.pb, now)?;
        }
        heard().insert(host.clone());
        Ok(())
    }

    /// The heartbeat path's report that a heartbeat of `host` it accepted at `now` said a
    /// service the coordinator manages for the host is installed and healthy: the first since
    /// the host's approval is written into its host record, and every later one leaves it as it
    /// is. An approval writes the host record without it. A host id never approved has no
    /// record and nothing is recorded.
    ///
    /// Decided under the store's lock, as [`Self::note_heartbeat`] is; a host whose record is
    /// known to hold the time already is answered from memory, and an approval forgets it.
    ///
    /// # Errors
    /// [`StoreError`] when the host record cannot be read or written.
    pub fn note_serving(&self, host: &HostId, now: u64) -> Result<(), StoreError> {
        let served = || self.served.lock().unwrap_or_else(PoisonError::into_inner);
        if served().contains(host) {
            return Ok(());
        }
        let _held = self.lock()?;
        let Some(row) = self.load_host(host)? else {
            return Ok(());
        };
        if row.pb.first_serving_unix.is_none() {
            self.records().set_first_serving(&row.pb, now)?;
        }
        served().insert(host.clone());
        Ok(())
    }

    /// The heartbeat path's report that a daemon on machine `machine_id` speaks for `host` at
    /// `now`. When another machine id reported the same host within [`LIVE_WINDOW_SECS`]
    /// before, the host is marked duplicate (durably) and a warn event is emitted
    /// (`two live daemons report one host id`). Returns whether the host is duplicate.
    ///
    /// # Errors
    /// [`StoreError::UnknownHost`] for a host id never approved; [`StoreError`] for a record.
    pub fn report_live(
        &self,
        host: &HostId,
        machine_id: &str,
        now: u64,
    ) -> Result<bool, StoreError> {
        let _held = self.lock()?;
        let row = self
            .load_host(host)?
            .ok_or(StoreError::UnknownHost(host.clone()))?;
        let machines = {
            let mut live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
            let seen = live.entry(host.clone()).or_default();
            seen.retain(|_, at| now.saturating_sub(*at) <= LIVE_WINDOW_SECS);
            seen.insert(machine_id.to_owned(), now);
            seen.len()
        };
        if machines > 1 && !row.pb.duplicate {
            self.records().set_duplicate(&row.pb)?;
            tracing::warn!(host_id = %host, machines, "two live daemons report one host id");
            return Ok(true);
        }
        Ok(row.pb.duplicate)
    }
}
