//! The store's checked rows: every record the store reads through
//! [`Records`](super::records::Records) is checked here, whatever keeps it, so a record read
//! from a database is held to what a file is held to.
//!
//! A request's state must be one this release knows and its key the one its id names; a host's
//! key hash must be 32 bytes; a deny row's state must be one this release knows; host ids must
//! parse. Reading a file (protobuf, schema) is [`FileRecords`](super::records::FileRecords)'.

use std::path::{Path, PathBuf};

use super::records::pb;
use super::{Claims, Expiry, RequestState, StoreError};
use crate::ca::P256Key;
use crate::names::{HostId, KeyHash};

/// The schema this release writes and the newest it reads.
pub(crate) const SCHEMA: u32 = 1;

pub(crate) fn corrupt(path: &Path, reason: &str) -> StoreError {
    StoreError::Corrupt {
        path: path.to_path_buf(),
        reason: reason.to_owned(),
    }
}

/// `text` as an optional host id: empty is none.
pub(crate) fn optional_host(path: &Path, text: &str) -> Result<Option<HostId>, StoreError> {
    if text.is_empty() {
        return Ok(None);
    }
    HostId::parse(text)
        .map(Some)
        .map_err(|e| corrupt(path, &e.to_string()))
}

/// A request record, checked.
#[derive(Clone, Debug)]
pub(crate) struct RequestRow {
    pub path: PathBuf,
    pub pb: pb::Request,
    pub id: KeyHash,
    pub key: P256Key,
    pub state: RequestState,
}

impl RequestRow {
    /// Checks the request `pb`, kept at `path` under the id `id`. A waiting request with
    /// `expired_unix` set is [`RequestState::Expired`].
    pub fn check(path: PathBuf, pb: pb::Request, id: &KeyHash) -> Result<RequestRow, StoreError> {
        let stored = RequestState::from_number(pb.state).ok_or(StoreError::UnknownState {
            path: path.clone(),
            value: pb.state,
        })?;
        let state = if stored.is_pending() && pb.expired_unix != 0 {
            RequestState::Expired
        } else {
            stored
        };
        let key = P256Key::from_spki(&pb.spki_der)
            .ok()
            .filter(|key| key.key_hash() == *id && pb.request_id == id.as_bytes())
            .ok_or(corrupt(
                &path,
                "the request's key is not the one its id names",
            ))?;
        Ok(RequestRow {
            path,
            pb,
            id: *id,
            key,
            state,
        })
    }

    /// The machine id the request claims.
    pub fn machine_id(&self) -> &str {
        self.pb
            .claims
            .as_ref()
            .map_or("", |claims| &claims.machine_id)
    }

    /// The Mac serial number the request claims; empty when it claims none.
    pub fn mac_serial(&self) -> &str {
        self.pb
            .claims
            .as_ref()
            .map_or("", |claims| &claims.mac_serial)
    }

    /// The host this request renews.
    pub fn renewal_of(&self) -> Result<Option<HostId>, StoreError> {
        optional_host(&self.path, &self.pb.renewal_of)
    }

    /// The host id it was approved as.
    pub fn host_id(&self) -> Result<Option<HostId>, StoreError> {
        let text = self
            .pb
            .approved
            .as_ref()
            .map_or("", |approved| &approved.host_id);
        optional_host(&self.path, text)
    }

    /// Sets a state the file holds, in the row and in its message; never
    /// [`RequestState::Expired`], which [`RequestRow::expire`] sets.
    pub fn set_state(&mut self, state: RequestState) {
        self.state = state;
        self.pb.state = state as i32;
    }

    /// Marks the waiting request expired at `now` under `ttl_secs`: the message keeps its
    /// waiting state, so a release that does not know `expired_unix` reads it as waiting.
    pub fn expire(&mut self, now: u64, ttl_secs: u64) {
        self.state = RequestState::Expired;
        // A clock at 0 is not a time a request expires at: 0 means "not expired".
        self.pb.expired_unix = now.max(1);
        self.pb.expired_after_secs = ttl_secs;
    }

    /// When and under which TTL the request expired, if it did.
    pub fn expiry(&self) -> Option<Expiry> {
        (self.state == RequestState::Expired).then_some(Expiry {
            expired_unix: self.pb.expired_unix,
            ttl_secs: self.pb.expired_after_secs,
        })
    }
}

/// A host record, checked.
#[derive(Clone, Debug)]
pub(crate) struct HostRow {
    pub pb: pb::Host,
    pub key: KeyHash,
}

impl HostRow {
    /// Checks the host `pb`, kept at `path`.
    pub fn check(path: &Path, pb: pb::Host) -> Result<HostRow, StoreError> {
        let key = KeyHash::from_bytes(&pb.key_hash)
            .ok_or(corrupt(path, "the key hash is not 32 bytes"))?;
        Ok(HostRow { pb, key })
    }
}

/// Where a deny row is. The numbers are those of `DenyState` in the store's proto.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub(crate) enum DenyState {
    Retiring = 1,
    Retired = 2,
    Cleared = 3,
}

impl DenyState {
    const ALL: [DenyState; 3] = [DenyState::Retiring, DenyState::Retired, DenyState::Cleared];

    /// Denied: retiring or retired.
    pub fn is_active(self) -> bool {
        self != DenyState::Cleared
    }
}

/// The state of the deny row `pb`, kept at `path`.
pub(crate) fn deny_state(path: &Path, pb: &pb::Deny) -> Result<DenyState, StoreError> {
    DenyState::ALL
        .into_iter()
        .find(|state| *state as i32 == pb.state)
        .ok_or(StoreError::UnknownState {
            path: path.to_path_buf(),
            value: pb.state,
        })
}

pub(crate) fn claims_to_pb(claims: &Claims) -> pb::Claims {
    pb::Claims {
        machine_id: claims.machine_id.clone(),
        dmi_serial: claims.dmi_serial.clone(),
        dmi_product_uuid: claims.dmi_product_uuid.clone(),
        addresses: claims.addresses.clone(),
        mac_serial: claims.mac_serial.clone(),
        mac_platform_uuid: claims.mac_platform_uuid.clone(),
        bmc_mac: claims.bmc_mac.clone(),
    }
}

pub(crate) fn claims_of_pb(claims: Option<&pb::Claims>) -> Claims {
    claims.map_or_else(Claims::default, |claims| Claims {
        machine_id: claims.machine_id.clone(),
        dmi_serial: claims.dmi_serial.clone(),
        dmi_product_uuid: claims.dmi_product_uuid.clone(),
        addresses: claims.addresses.clone(),
        mac_serial: claims.mac_serial.clone(),
        mac_platform_uuid: claims.mac_platform_uuid.clone(),
        bmc_mac: claims.bmc_mac.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Catches the Rust states drifting from the proto's numbers, which would make a file
    /// written by one release read as another state by the next.
    #[test]
    fn the_states_carry_the_proto_numbers() {
        let requests = [
            (RequestState::Pending, pb::RequestState::Pending),
            (
                RequestState::DuplicateClaim,
                pb::RequestState::DuplicateClaim,
            ),
            (RequestState::Approved, pb::RequestState::Approved),
            (RequestState::Issued, pb::RequestState::Issued),
            (RequestState::Denied, pb::RequestState::Denied),
        ];
        for (state, proto) in requests {
            assert_eq!(state as i32, proto as i32, "{state:?}");
            assert_eq!(RequestState::from_number(proto as i32), Some(state));
        }
        assert_eq!(
            RequestState::from_number(pb::RequestState::Unspecified as i32),
            None
        );
        // Expired is held as a waiting state with `expired_unix` set, never as a number of its
        // own: a file in it reads as waiting to a release before expiry.
        assert_eq!(
            pb::RequestState::try_from(RequestState::Expired as i32).ok(),
            None
        );
        assert_eq!(
            RequestState::from_number(RequestState::Expired as i32),
            None
        );
        let denies = [
            (DenyState::Retiring, pb::DenyState::Retiring),
            (DenyState::Retired, pb::DenyState::Retired),
            (DenyState::Cleared, pb::DenyState::Cleared),
        ];
        for (state, proto) in denies {
            assert_eq!(state as i32, proto as i32, "{state:?}");
        }
    }
}
