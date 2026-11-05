//! The signed nonce of a poll.
//!
//! A daemon polls for its certificate by signing, with the key of its request, the bytes
//! [`poll_message`] builds: the domain `hostkeeper-enroll-poll-v1`, a 0x00 byte, the request
//! id as 64 lower-case hex characters, a 0x00 byte, and the nonce the coordinator gave it (16
//! to 64 bytes). The signature is ECDSA P-256 with SHA-256, ASN.1 DER. The coordinator chooses
//! and remembers the nonces; this module only builds the message and checks the signature.

use super::PollError;
use super::store::EnrollmentStore;
use crate::names::KeyHash;

/// The domain that starts every poll message.
pub const POLL_DOMAIN: &[u8] = b"hostkeeper-enroll-poll-v1";

/// The shortest nonce, in bytes.
pub const MIN_NONCE_BYTES: usize = 16;

/// The longest nonce, in bytes.
pub const MAX_NONCE_BYTES: usize = 64;

/// The bytes a poll for `request_id` with `nonce` signs.
pub fn poll_message(request_id: &KeyHash, nonce: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(POLL_DOMAIN.len() + 66 + nonce.len());
    message.extend_from_slice(POLL_DOMAIN);
    message.push(0);
    message.extend_from_slice(request_id.to_hex().as_bytes());
    message.push(0);
    message.extend_from_slice(nonce);
    message
}

impl EnrollmentStore {
    /// Checks that `signature` is the poll signature of request `request_id` over `nonce`,
    /// made by the key the request was posted with.
    ///
    /// # Errors
    /// [`PollError::BadNonce`] for a nonce outside 16 to 64 bytes (checked first),
    /// [`PollError::UnknownRequest`], [`PollError::BadSignature`], [`PollError::Store`].
    pub fn verify_poll(
        &self,
        request_id: &KeyHash,
        nonce: &[u8],
        signature: &[u8],
    ) -> Result<(), PollError> {
        if !(MIN_NONCE_BYTES..=MAX_NONCE_BYTES).contains(&nonce.len()) {
            return Err(PollError::BadNonce(nonce.len()));
        }
        let row = self
            .load_request(request_id)?
            .ok_or(PollError::UnknownRequest)?;
        if row.key.verify(&poll_message(request_id, nonce), signature) {
            return Ok(());
        }
        Err(PollError::BadSignature)
    }
}
