//! Mac nonces (design 5.9): the one-time value the policy gate gives a Mac, which the
//! coordinator approves the Mac's request by.
//!
//! A nonce is kept by its SHA-256 only ([`MacNonceHash`]), one record each, with the serial it
//! was issued for, the pending request it was issued for (its key hash), and when. It is good
//! for [`MAC_NONCE_VALID_SECS`] from its issue and for one approval, of that request only. A
//! nonce written before the request field holds none, and is judged without it. The approval
//! consumes it in the commit that marks its request approved (`Approval.mac_nonce_sha256`), and
//! the same commit marks the nonce consumed. The files make the request the commit point and
//! write the nonce after it ([`FileRecords`](super::records::FileRecords)); records that commit
//! all or nothing write both or neither. A check that does not lead to an approval consumes
//! nothing.

use std::fmt;

use super::StoreError;
use super::records::{Record, pb};
use super::rows::SCHEMA;
use super::store::EnrollmentStore;
use crate::names::KeyHash;

/// How long a Mac nonce is good for after its issue, in seconds: one hour.
pub const MAC_NONCE_VALID_SECS: u64 = 3600;

/// The SHA-256 of a Mac nonce: all the store keeps of one. Its `Debug` shows no byte of it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MacNonceHash([u8; 32]);

impl MacNonceHash {
    /// The SHA-256 of `nonce`.
    pub fn of(nonce: &[u8]) -> MacNonceHash {
        let digest = ring::digest::digest(&ring::digest::SHA256, nonce);
        let mut out = [0u8; 32];
        out.copy_from_slice(digest.as_ref());
        MacNonceHash(out)
    }

    /// The hash whose bytes are `bytes`, which must be exactly 32.
    pub(crate) fn from_bytes(bytes: &[u8]) -> Option<MacNonceHash> {
        <[u8; 32]>::try_from(bytes).ok().map(MacNonceHash)
    }

    /// The 32 bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for MacNonceHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MacNonceHash(..)")
    }
}

/// Why a Mac nonce does not approve a request. Each names its token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MacNonceRefusal {
    /// No nonce of this hash was issued.
    #[error("nonce_unknown: no nonce of this hash was issued")]
    Unknown,
    /// More than [`MAC_NONCE_VALID_SECS`] have passed since its issue.
    #[error("nonce_expired: the nonce is older than one hour")]
    Expired,
    /// An approval consumed it already.
    #[error("nonce_used: an approval consumed the nonce already")]
    Used,
    /// It was issued for another serial than the one the request claims.
    #[error("nonce_wrong_serial: the nonce was issued for another serial")]
    WrongSerial,
    /// It was issued for another request (another key) than the one it is presented with.
    #[error("nonce_wrong_request: the nonce was issued for another request")]
    WrongRequest,
}

impl MacNonceRefusal {
    /// The token of the refusal.
    pub fn token(self) -> &'static str {
        match self {
            MacNonceRefusal::Unknown => "nonce_unknown",
            MacNonceRefusal::Expired => "nonce_expired",
            MacNonceRefusal::Used => "nonce_used",
            MacNonceRefusal::WrongSerial => "nonce_wrong_serial",
            MacNonceRefusal::WrongRequest => "nonce_wrong_request",
        }
    }
}

impl EnrollmentStore {
    /// Records the nonce of SHA-256 `hash`, issued at `now` (unix seconds) for the Mac of
    /// `serial` and its pending request `request` (the request's key hash): it approves that
    /// request only. Returns `false`, and changes nothing, when a nonce of that hash exists
    /// already (consumed or not): it is never issued again.
    ///
    /// # Errors
    /// [`StoreError`] when its file cannot be read or written.
    pub fn issue_mac_nonce(
        &self,
        serial: &str,
        request: &KeyHash,
        hash: &MacNonceHash,
        now: u64,
    ) -> Result<bool, StoreError> {
        let _held = self.lock()?;
        if self.records().mac_nonce(hash)?.is_some() {
            return Ok(false);
        }
        let nonce = pb::MacNonce {
            schema: SCHEMA,
            nonce_sha256: hash.as_bytes().to_vec(),
            serial: serial.to_owned(),
            issued_unix: now,
            consumed_by: Vec::new(),
            request_id: request.as_bytes().to_vec(),
        };
        self.commit(&[Record::MacNonce(nonce)])?;
        Ok(true)
    }

    /// Whether the nonce of SHA-256 `hash` approves the request `request`, claiming `serial`,
    /// at `now`: its file, or the refusal, checked in this order: unknown, expired, used, wrong
    /// serial, wrong request. A nonce that names no request (written before the field) is not
    /// checked for it; one whose request field is not a key hash approves no request. The
    /// caller holds the store's lock and consumes it in the approval it writes.
    pub(super) fn mac_nonce_verdict(
        &self,
        hash: &MacNonceHash,
        request: &KeyHash,
        serial: &str,
        now: u64,
    ) -> Result<Result<pb::MacNonce, MacNonceRefusal>, StoreError> {
        let Some(nonce) = self.records().mac_nonce(hash)? else {
            return Ok(Err(MacNonceRefusal::Unknown));
        };
        if now.saturating_sub(nonce.issued_unix) > MAC_NONCE_VALID_SECS {
            return Ok(Err(MacNonceRefusal::Expired));
        }
        if !nonce.consumed_by.is_empty() {
            return Ok(Err(MacNonceRefusal::Used));
        }
        if nonce.serial != serial {
            return Ok(Err(MacNonceRefusal::WrongSerial));
        }
        let bound = nonce.request_id.is_empty()
            || KeyHash::from_bytes(&nonce.request_id)
                .is_some_and(|bound| bound.equals_in_constant_time(request));
        if !bound {
            return Ok(Err(MacNonceRefusal::WrongRequest));
        }
        Ok(Ok(nonce))
    }
}
