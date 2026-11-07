//! Mac nonces (design 5.9): the one-time value the policy gate gives a Mac, which the
//! coordinator approves the Mac's request by.
//!
//! A nonce is kept by its SHA-256 only ([`MacNonceHash`]), one file each under `mac-nonces/`,
//! with the serial it was issued for, the pending request it was issued for (its key hash), and
//! when. It is good for [`MAC_NONCE_VALID_SECS`] from its issue and for one approval, of that
//! request only. A nonce file written before the request field holds none, and is judged without
//! it. The approval consumes it in the request file it writes
//! (`Approval.mac_nonce_sha256`), which is the commit point; the nonce's own file is marked
//! consumed after that write, and at the next [`EnrollmentStore::open`] if that mark did not
//! land, and until then the store remembers it in memory. A check that does not lead to an
//! approval consumes nothing.

use std::fmt;
use std::sync::PoisonError;

use prost::Message;

use super::StoreError;
use super::files::{SCHEMA, pb, read_mac_nonce};
use super::store::EnrollmentStore;
use crate::names::KeyHash;

/// How long a Mac nonce is good for after its issue, in seconds: one hour.
pub const MAC_NONCE_VALID_SECS: u64 = 3600;

/// The subdirectory of the nonce files.
pub(super) const MAC_NONCES: &str = "mac-nonces";

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
    fn from_bytes(bytes: &[u8]) -> Option<MacNonceHash> {
        <[u8; 32]>::try_from(bytes).ok().map(MacNonceHash)
    }

    /// The 32 bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The name of its file.
    fn file_name(&self) -> String {
        format!("{}.pb", hex::encode(self.0))
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
        if self.load_mac_nonce(hash)?.is_some() {
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
        self.write(MAC_NONCES, &hash.file_name(), &nonce.encode_to_vec())?;
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
        let Some(nonce) = self.load_mac_nonce(hash)? else {
            return Ok(Err(MacNonceRefusal::Unknown));
        };
        if now.saturating_sub(nonce.issued_unix) > MAC_NONCE_VALID_SECS {
            return Ok(Err(MacNonceRefusal::Expired));
        }
        let remembered = self
            .consumed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(hash);
        if remembered || !nonce.consumed_by.is_empty() {
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

    /// Consumes the nonce of `hash`, whose file is `nonce`, for `request`, whose approval was
    /// just written: remembered at once, then marked in its file. A mark that does not land is
    /// logged; the approval holds the consumption, and the next open marks the file.
    pub(super) fn consume_mac_nonce(
        &self,
        hash: &MacNonceHash,
        nonce: pb::MacNonce,
        request: &KeyHash,
    ) {
        self.consumed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(*hash);
        if let Err(why) = self.mark_consumed(hash, nonce, request) {
            tracing::error!(
                %why,
                request_id = %request,
                "a consumed Mac nonce's file was not marked: the approval holds the consumption"
            );
        }
    }

    /// Writes the file of `hash`, which held `nonce`, consumed by `request`.
    fn mark_consumed(
        &self,
        hash: &MacNonceHash,
        mut nonce: pb::MacNonce,
        request: &KeyHash,
    ) -> Result<(), StoreError> {
        nonce.consumed_by = request.as_bytes().to_vec();
        self.write(MAC_NONCES, &hash.file_name(), &nonce.encode_to_vec())
    }

    /// At open: remembers every nonce an approval consumed, and marks the file of each one
    /// whose mark did not land, under the lock. A consumed nonce with no file (removed by
    /// hand) is left so: it is unknown, which approves nothing either.
    pub(super) fn reconcile_mac_nonces(&self) -> Result<(), StoreError> {
        let consumed: Vec<(MacNonceHash, KeyHash)> = self
            .all_requests()?
            .iter()
            .filter_map(|row| {
                let approved = row.pb.approved.as_ref()?;
                MacNonceHash::from_bytes(&approved.mac_nonce_sha256).map(|hash| (hash, row.id))
            })
            .collect();
        for (hash, request) in consumed {
            self.consumed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(hash);
            let unmarked = self
                .load_mac_nonce(&hash)?
                .filter(|nonce| nonce.consumed_by.is_empty());
            if let Some(nonce) = unmarked {
                let _held = self.lock()?;
                self.mark_consumed(&hash, nonce, &request)?;
            }
        }
        Ok(())
    }

    fn load_mac_nonce(&self, hash: &MacNonceHash) -> Result<Option<pb::MacNonce>, StoreError> {
        let path = self.path_in(MAC_NONCES, &hash.file_name());
        self.read(&path)?
            .map(|bytes| read_mac_nonce(&path, &bytes))
            .transpose()
    }
}
