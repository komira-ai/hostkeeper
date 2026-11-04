//! What the daemon sends the coordinator during enrollment and renewal, and what it reads back,
//! as plain types; and [`CoordinatorEnroll`], the port that carries them.
//!
//! The contract is the coordinator's enrollment API (design section 5.7):
//!
//! - `Enroll` takes a PKCS#10 request for the host's key, the claims, the boot image and, for a
//!   host that was offline past expiry, `renewal_of`; it answers the request id and a state.
//! - The request id is the lower-case hex SHA-256 of the key's DER SubjectPublicKeyInfo.
//! - A first `Poll` with an empty signature answers a coordinator nonce, 16 to 64 bytes, good for
//!   one use. The second `Poll` carries that nonce and an ECDSA P-256/SHA-256 signature (DER) by
//!   the request key over [`poll_message`]. Before approval it answers pending, not an error.
//! - `Renew` carries a new request for the same key, sent by a host whose certificate is still
//!   valid; its self-signature is the proof that the host holds the key.
//!
//! This crate has no transport: the daemon's gRPC client implements the port in its own change,
//! and the tests use an in-memory coordinator.

use std::fmt;

use sha2::{Digest, Sha256};

/// The first part of the message a poll signs.
pub const POLL_CONTEXT: &[u8] = b"hostkeeper-enroll-poll-v1";
/// The shortest nonce a coordinator may give.
pub const NONCE_MIN: usize = 16;
/// The longest nonce a coordinator may give.
pub const NONCE_MAX: usize = 64;

/// The id of an enrollment request: the lower-case hex SHA-256 of the DER SubjectPublicKeyInfo
/// of the request's key. One key has one id, so a reposted request is the same request.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestId(String);

impl RequestId {
    /// The id of the key whose DER SubjectPublicKeyInfo is `spki_der`.
    pub fn of_spki(spki_der: &[u8]) -> RequestId {
        RequestId(hex::encode(Sha256::digest(spki_der)))
    }

    /// The 64 hex digits.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The id the coordinator gave a host when an operator approved it; its certificate's subject is
/// `host:<id>`. Never empty.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HostId(String);

impl HostId {
    /// `id`, or `None` when it is empty.
    pub fn new(id: &str) -> Option<HostId> {
        (!id.is_empty()).then(|| HostId(id.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The subject common name a certificate of this host carries: `host:<id>`.
    pub fn subject(&self) -> String {
        format!("host:{}", self.0)
    }
}

impl fmt::Display for HostId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why the coordinator denied a request or refused a renewal: the token of the closed reason of
/// the coordinator's API, as the transport names it. The daemon reports it and does not act on
/// its value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reason(String);

impl Reason {
    pub fn new(token: &str) -> Reason {
        Reason(token.to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a host says about itself so that an operator can recognise it. Claims are not proof:
/// they authorise nothing (design section 5.7, step 2). A claim the host cannot read is absent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Claims {
    /// `/etc/machine-id`.
    pub machine_id: Option<String>,
    /// The DMI product serial.
    pub product_serial: Option<String>,
    /// The DMI product uuid.
    pub product_uuid: Option<String>,
    /// The link-layer addresses of the host's network interfaces, sorted, without duplicates.
    pub network_addresses: Vec<String>,
}

/// An `Enroll` request. It is a function of the key, the claims, the image and `renewal_of`
/// only, so a repost of the same key is the same request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnrollRequest {
    /// The PKCS#10 request (DER) for the host's key, signed by it.
    pub csr_der: Vec<u8>,
    pub claims: Claims,
    /// The boot image or installer the daemon came from.
    pub image: String,
    /// The host this request returns, for a host whose certificate expired while it was offline.
    pub renewal_of: Option<HostId>,
}

/// Where a request stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnrollState {
    Pending,
    Approved,
    Denied(Reason),
}

/// The coordinator's answer to `Enroll`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnrollAnswer {
    pub request_id: RequestId,
    pub state: EnrollState,
}

/// A `Poll`: unsigned (empty `nonce` and `signature`) to get a nonce, then signed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Poll {
    pub request_id: RequestId,
    pub nonce: Vec<u8>,
    /// ECDSA P-256/SHA-256, DER, over [`poll_message`].
    pub signature: Vec<u8>,
}

/// A certificate the coordinator issued, as it sent it. Nothing here is trusted until it is
/// checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issued {
    pub host_id: String,
    /// The host certificate, PEM.
    pub cert_pem: String,
    /// The CA bundle, PEM, one or more certificates.
    pub ca_bundle_pem: String,
}

/// The coordinator's answer to a `Poll`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PollAnswer {
    /// The answer to an unsigned poll: the nonce to sign.
    Challenge(Vec<u8>),
    Pending,
    Denied(Reason),
    Issued(Issued),
}

/// A `Renew` request: a new PKCS#10 request for the key the host holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenewRequest {
    pub host_id: HostId,
    pub csr_der: Vec<u8>,
}

/// The coordinator's answer to `Renew`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenewAnswer {
    Issued(Issued),
    Refused(Reason),
}

/// The coordinator could not be reached, or answered with a transport error.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct TransportError(pub String);

/// The port to the coordinator's enrollment API.
///
/// [`CoordinatorEnroll::presented_ca_der`] connects and returns the CA certificate the
/// coordinator's chain ends in, before anything is sent; the enroller compares its SHA-256 with
/// the pinned `ca_sha256` and sends nothing when they differ. An implementation sends the calls
/// that follow over that connection only, or refuses on any new connection whose CA is not the
/// one it returned, so a call never goes to a coordinator whose CA was not checked.
pub trait CoordinatorEnroll: Send {
    /// The DER of the CA certificate the coordinator's certificate chain ends in.
    fn presented_ca_der(&mut self) -> Result<Vec<u8>, TransportError>;
    fn enroll(&mut self, request: &EnrollRequest) -> Result<EnrollAnswer, TransportError>;
    fn poll(&mut self, poll: &Poll) -> Result<PollAnswer, TransportError>;
    fn renew(&mut self, request: &RenewRequest) -> Result<RenewAnswer, TransportError>;
}

/// The bytes a signed poll signs: [`POLL_CONTEXT`], `0x00`, the 64 hex characters of the
/// request id, `0x00`, the nonce.
pub fn poll_message(request_id: &RequestId, nonce: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(POLL_CONTEXT.len() + 66 + nonce.len());
    message.extend_from_slice(POLL_CONTEXT);
    message.push(0);
    message.extend_from_slice(request_id.as_str().as_bytes());
    message.push(0);
    message.extend_from_slice(nonce);
    message
}
