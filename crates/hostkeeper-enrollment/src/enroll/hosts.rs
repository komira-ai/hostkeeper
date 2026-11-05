//! What the store knows of an approved host: deny rows (design 6.1), renewal, and duplicate
//! live daemons on one host id (design 5.7, step 4).
//!
//! A deny row is written by [`EnrollmentStore::deny`] as retiring: the host is refused at once
//! (`is_denied`), and an update may still be in flight on it (`is_retiring`). The coordinator
//! frees the host's update slot when the host reports that update's record final, and then
//! calls [`EnrollmentStore::retire`]; the store never touches the slot. A deny row is never
//! removed: only an approval with `replace` marks it cleared, and records which request did.

use std::sync::PoisonError;

use super::files::{DenyState, corrupt};
use super::store::{EnrollmentStore, LIVE_WINDOW_SECS};
use super::{RequestState, StoreError};
use crate::names::{HostId, KeyHash};

impl EnrollmentStore {
    /// Denies `host`: a retiring deny row (a row already retiring or retired is kept), and the
    /// host's request marked denied, so a repost or poll of its key is refused.
    ///
    /// # Errors
    /// [`StoreError`] when a file cannot be read or written.
    pub fn deny(&self, host: &HostId) -> Result<(), StoreError> {
        let _held = self.lock()?;
        if !self.denied(host)? {
            self.write_deny(host, DenyState::Retiring, &[])?;
        }
        self.mark_request_denied(host)
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
