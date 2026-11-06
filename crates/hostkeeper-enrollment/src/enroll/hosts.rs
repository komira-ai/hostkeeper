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

use super::files::{DenyState, claims_of_pb, corrupt};
use super::store::{EnrollmentStore, LIVE_WINDOW_SECS};
use super::{EnrolledHost, RequestState, StoreError};
use crate::names::{HostId, KeyHash, Role};

impl EnrollmentStore {
    /// Denies `host`: a retiring deny row (a row already retiring or retired is kept), and the
    /// host's request marked denied, so a repost or poll of its key is refused. Returns whether
    /// the row is new: `false` when the host was denied already, decided under the store's lock,
    /// so of two calls at once exactly one sees `true`.
    ///
    /// # Errors
    /// [`StoreError`] when a file cannot be read or written.
    pub fn deny(&self, host: &HostId) -> Result<bool, StoreError> {
        let _held = self.lock()?;
        let new = !self.denied(host)?;
        if new {
            self.write_deny(host, DenyState::Retiring, &[])?;
        }
        self.mark_request_denied(host)?;
        Ok(new)
    }

    /// Records that `host` retired: its deny row becomes final (written denied when it was not
    /// yet), so `is_retiring` is false and `is_denied` stays true.
    ///
    /// # Errors
    /// [`StoreError`] when a file cannot be read or written.
    pub fn retire(&self, host: &HostId) -> Result<(), StoreError> {
        let _held = self.lock()?;
        self.write_deny(host, DenyState::Retired, &[])?;
        self.mark_request_denied(host)
    }

    fn mark_request_denied(&self, host: &HostId) -> Result<(), StoreError> {
        let Some(row) = self.load_host(host)? else {
            return Ok(());
        };
        let missing = corrupt(&self.host_path(host), "the host's request is missing");
        let mut request = self.load_request(&row.key)?.ok_or(missing)?;
        request.set_state(RequestState::Denied);
        self.write_request(&request)
    }

    /// Whether `host` is denied (retiring or retired): what the heartbeat handshake and
    /// `Renew` check.
    ///
    /// # Errors
    /// [`StoreError`] when its deny file cannot be read.
    pub fn is_denied(&self, host: &HostId) -> Result<bool, StoreError> {
        self.denied(host)
    }

    /// The role `host` was approved for; `None` for a host id never approved. The heartbeat
    /// path reads it as the role of a caller whose certificate names the host.
    ///
    /// # Errors
    /// [`StoreError`] when the host file cannot be read, or holds no role.
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
    /// [`StoreError`] when the host file cannot be read, or holds no role.
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

    /// `host` as enrolled: its host file names the key it was approved for, and the request of
    /// that key holds what it claimed and when it was approved. `None` for a host id never
    /// approved, or denied (retiring or retired).
    ///
    /// # Errors
    /// [`StoreError`] when a file cannot be read, or the host's request is missing.
    pub fn enrolled_host(&self, host: &HostId) -> Result<Option<EnrolledHost>, StoreError> {
        let Some(row) = self.load_host(host)? else {
            return Ok(None);
        };
        if self.denied(host)? {
            return Ok(None);
        }
        let missing = corrupt(&self.host_path(host), "the host's request is missing");
        let request = self.load_request(&row.key)?.ok_or(missing)?;
        Ok(Some(EnrolledHost {
            host_id: host.clone(),
            claims: claims_of_pb(request.pb.claims.as_ref()),
            approved_unix: request.pb.approved.as_ref().map_or(0, |a| a.approved_unix),
        }))
    }

    /// Every enrolled host ([`Self::enrolled_host`]), by host id.
    ///
    /// # Errors
    /// [`StoreError`] when a file cannot be read, or a host's request is missing.
    pub fn enrolled_hosts(&self) -> Result<Vec<EnrolledHost>, StoreError> {
        let mut enrolled = Vec::new();
        for host in self.host_ids()? {
            enrolled.extend(self.enrolled_host(&host)?);
        }
        Ok(enrolled)
    }

    /// Whether `host` is denied and may still have an update in flight.
    ///
    /// # Errors
    /// [`StoreError`] when its deny file cannot be read.
    pub fn is_retiring(&self, host: &HostId) -> Result<bool, StoreError> {
        Ok(self.load_deny(host)? == Some(DenyState::Retiring))
    }

    /// Whether `key_hash` may renew the certificate of `host`: the host was approved for that
    /// key and is not denied.
    ///
    /// # Errors
    /// [`StoreError`] when a file cannot be read.
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
    /// [`StoreError`] when the host file cannot be read.
    pub fn is_duplicate(&self, host: &HostId) -> Result<bool, StoreError> {
        Ok(self.load_host(host)?.is_some_and(|row| row.pb.duplicate))
    }

    /// The heartbeat path's report that a daemon on machine `machine_id` speaks for `host` at
    /// `now`. When another machine id reported the same host within [`LIVE_WINDOW_SECS`]
    /// before, the host is marked duplicate (durably) and a warn event is emitted
    /// (`two live daemons report one host id`). Returns whether the host is duplicate.
    ///
    /// # Errors
    /// [`StoreError::UnknownHost`] for a host id never approved; [`StoreError`] for a file.
    pub fn report_live(
        &self,
        host: &HostId,
        machine_id: &str,
        now: u64,
    ) -> Result<bool, StoreError> {
        let _held = self.lock()?;
        let mut row = self
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
            row.pb.duplicate = true;
            self.write_host(&row.pb)?;
            tracing::warn!(host_id = %host, machines, "two live daemons report one host id");
        }
        Ok(row.pb.duplicate)
    }
}
