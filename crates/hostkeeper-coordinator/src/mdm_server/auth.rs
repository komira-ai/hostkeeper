//! Who is calling `/checkin` or `/mdm` (`docs/design/coordinator-mdm.md`, sections 4.1 and 4.2).
//!
//! A request proves possession of a key in one of two ways, and either is enough: the TLS client
//! certificate of the connection (the handshake verified its chain to the MDM CA and the client's
//! signature), or the `Mdm-Signature` header ([`signature::signer`]) over the body. Either way the
//! certificate's SHA-256 is then looked up in the identity table: the coordinator must have issued
//! exactly that certificate, and its identity must not be retired. The enrollment the request is
//! about is then the live enrollment of the identity's serial, entered with that same certificate;
//! a request on a certificate whose enrollment a newer one replaced is refused.

use hyper::HeaderMap;
use sha2::{Digest, Sha256};

use super::signature;
use crate::http::Peer;
use crate::store::mdm::{Enrollment, Identity, IdentityState, MdmError, MdmStore};

/// The header a signing client puts its signature in.
pub(crate) const SIGNATURE_HEADER: &str = "mdm-signature";

/// Why a request is not accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    /// No certificate and no signature that verifies.
    Unauthenticated,
    /// The certificate is not one the coordinator issued, or its identity ended.
    UnknownIdentity,
    /// The certificate is the identity of an enrollment that is no longer the live one.
    NotLive,
    /// The identity is for another device than the one the message names.
    OtherDevice,
    /// The store failed.
    Store(MdmError),
}

impl From<MdmError> for Refused {
    fn from(why: MdmError) -> Self {
        Self::Store(why)
    }
}

/// The SHA-256 of `der` in hex, as the tables spell a certificate.
pub(crate) fn fingerprint(der: &[u8]) -> String {
    hex::encode(Sha256::digest(der))
}

/// The certificate hash a request proves possession of: the connection's client certificate, or
/// the signer of `body` named by the `Mdm-Signature` header.
///
/// # Errors
/// [`Refused::Unauthenticated`]: neither.
pub(crate) fn certificate(
    peer: &Peer,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<String, Refused> {
    if let Some(leaf) = peer {
        return Ok(fingerprint(leaf));
    }
    let header = headers
        .get(SIGNATURE_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or(Refused::Unauthenticated)?;
    signature::signer(body, header)
        .map(|der| fingerprint(&der))
        .map_err(|why| {
            tracing::warn!(%why, "refused a request: its Mdm-Signature does not verify");
            Refused::Unauthenticated
        })
}

/// The issued identity of the certificate `cert_sha256`.
///
/// # Errors
/// [`Refused::UnknownIdentity`]: no identity row has it, or it is not `Issued` (retired by a
/// replacement or a check-out, or dead); [`Refused::Store`].
pub(crate) fn identity(store: &dyn MdmStore, cert_sha256: &str) -> Result<Identity, Refused> {
    match store.identity(cert_sha256)? {
        Some(identity) if identity.state == IdentityState::Issued => Ok(identity),
        _ => Err(Refused::UnknownIdentity),
    }
}

/// The live enrollment that `identity` and the certificate `cert_sha256` stand for.
///
/// # Errors
/// [`Refused::NotLive`]: the serial has no live enrollment, or it was entered with another
/// certificate; [`Refused::Store`].
pub(crate) fn enrollment(
    store: &dyn MdmStore,
    identity: &Identity,
    cert_sha256: &str,
) -> Result<Enrollment, Refused> {
    match store.live_enrollment(&identity.serial)? {
        Some(live) if live.cert_sha256.as_deref() == Some(cert_sha256) => Ok(live),
        _ => Err(Refused::NotLive),
    }
}

#[cfg(test)]
#[path = "auth_tests.rs"]
mod tests;
