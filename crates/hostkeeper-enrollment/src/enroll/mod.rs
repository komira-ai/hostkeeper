//! The enrollment store: the coordinator's durable record of who asked to join, who was
//! approved, which certificate each request got, and which hosts are denied (design 5.7, 6.1,
//! 8.8 and 10).
//!
//! - [`EnrollmentStore::post`] records a request. Its id is the SHA-256 of the CSR's
//!   SubjectPublicKeyInfo, computed here; no caller supplies one. Posting the same key again is
//!   the same request. A request claiming a machine id another request has, or a MAC bound to
//!   an enrolled host, is a duplicate claim.
//! - [`EnrollmentStore::approve`] is the operator's decision, as a plain function: the
//!   operator names the key hash, and the host id is checked unique under the store lock that
//!   records the approval. [`EnrollmentStore::approve_recorded`] is the same decision with who
//!   made it, when and why written into the approval, a `duplicate_claim` request refused
//!   unless the decision replaces, and a Mac nonce checked and consumed in the same write.
//! - [`EnrollmentStore::post`] refuses a new request while [`EnrollmentStore::max_pending`]
//!   requests wait (`pending_full`); a repost of a waiting request is still the same request.
//! - [`EnrollmentStore::issue_mac_nonce`] records a Mac nonce by its SHA-256, for one serial,
//!   good for [`MAC_NONCE_VALID_SECS`] and one approval.
//! - [`EnrollmentStore::issue`] makes the certificate of an approved request, once per request
//!   id and round, whatever fails in between.
//! - [`EnrollmentStore::expire`] moves a request that waited longer than the request TTL to
//!   [`RequestState::Expired`]: its host is unenrolled, it is not approved, and it no longer
//!   counts toward [`EnrollmentStore::max_pending`]. The same key posted again starts it over.
//! - [`EnrollmentStore::verify_poll`] checks a poll's signed nonce against the request's key.
//! - [`EnrollmentStore::deny`], [`EnrollmentStore::retire`], [`EnrollmentStore::report_live`]
//!   and the questions the heartbeat path asks (`is_denied`, `is_retiring`, `is_duplicate`,
//!   `renew_allowed`).
//!
//! Every mutation takes one mutex and an exclusive `flock` on the store's lock file. The files
//! and their messages are listed in `proto/hostkeeper/enrollment/store/v1/store.proto`.

mod files;
mod hosts;
mod issue;
mod mac_nonce;
mod poll;
mod store;

use std::io;
use std::path::PathBuf;

pub use issue::IssuedCertificate;
pub use mac_nonce::{MAC_NONCE_VALID_SECS, MacNonceHash, MacNonceRefusal};
pub use poll::{MAX_NONCE_BYTES, MIN_NONCE_BYTES, POLL_DOMAIN, poll_message};
pub use store::{DEFAULT_MAX_PENDING, EnrollmentStore, FIRST_SERIAL, LIVE_WINDOW_SECS};

use crate::ca::{ProfileError, SignError};
use crate::names::{HostId, KeyHash, MacAddress, Role, ServiceName};

/// What a host says about itself when it asks to join: what an operator recognises a machine
/// by. Claims authorise nothing; the store compares machine ids and MACs to flag duplicates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Claims {
    pub machine_id: String,
    pub dmi_serial: String,
    pub dmi_product_uuid: String,
    pub addresses: Vec<String>,
    /// A Mac's serial number.
    pub mac_serial: String,
    /// A Mac's platform UUID.
    pub mac_platform_uuid: String,
    /// The MAC address of the host's BMC; empty when the host claimed none.
    pub bmc_mac: String,
}

impl Claims {
    /// The claimed addresses that read as MAC addresses ([`MacAddress::parse`]), in their one
    /// spelling, sorted, without duplicates. An address of another shape (an InfiniBand
    /// address) is left out.
    pub fn data_macs(&self) -> Vec<MacAddress> {
        let mut macs: Vec<MacAddress> = self
            .addresses
            .iter()
            .filter_map(|text| MacAddress::parse(text).ok())
            .collect();
        macs.sort();
        macs.dedup();
        macs
    }

    /// The claimed BMC MAC, when there is one that reads as a MAC address.
    pub fn bmc(&self) -> Option<MacAddress> {
        MacAddress::parse(&self.bmc_mac).ok()
    }

    /// Every MAC the host claims: its data MACs and its BMC MAC.
    pub fn all_macs(&self) -> Vec<MacAddress> {
        let mut macs = self.data_macs();
        macs.extend(self.bmc());
        macs.sort();
        macs.dedup();
        macs
    }
}

/// A request as the coordinator received it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewRequest {
    /// The PKCS#10 request, DER.
    pub csr_der: Vec<u8>,
    pub claims: Claims,
    /// The boot image or package the host says it was installed from.
    pub image: String,
    /// The host this request renews, for a host whose certificate expired.
    pub renewal_of: Option<HostId>,
    /// The host presented a Mac nonce with the request (the nonce is not given to the store
    /// here; it is checked when the request is approved).
    pub mac_nonce_presented: bool,
}

/// Where a request is. The numbers are those of `RequestState` in the store's proto, except
/// [`RequestState::Expired`], which the proto has no number for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum RequestState {
    /// Waiting for an operator.
    Pending = 1,
    /// Waiting for an operator, and flagged as a duplicate claim (an alert was emitted).
    DuplicateClaim = 2,
    /// Approved; the certificate is made at the next poll.
    Approved = 3,
    /// The certificate of the current round is made.
    Issued = 4,
    /// The host it was approved for is denied.
    Denied = 5,
    /// It waited for an operator longer than the coordinator's request TTL
    /// ([`EnrollmentStore::expire`]): its host is unenrolled, and it is never approved. Not a
    /// state the file's `state` field holds: the file keeps the waiting state it expired in and
    /// sets `expired_unix`, which a release before expiry ignores.
    Expired = 6,
}

impl RequestState {
    const ALL: [RequestState; 5] = [
        RequestState::Pending,
        RequestState::DuplicateClaim,
        RequestState::Approved,
        RequestState::Issued,
        RequestState::Denied,
    ];

    /// The state whose proto number is `value`; never [`RequestState::Expired`].
    fn from_number(value: i32) -> Option<RequestState> {
        RequestState::ALL
            .into_iter()
            .find(|state| *state as i32 == value)
    }

    /// Waiting for an operator.
    pub fn is_pending(self) -> bool {
        matches!(self, RequestState::Pending | RequestState::DuplicateClaim)
    }
}

/// The answer to a post.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostOutcome {
    /// The SHA-256 of the CSR's SubjectPublicKeyInfo.
    pub request_id: KeyHash,
    pub state: RequestState,
    /// The host the request renews.
    pub renewal_of: Option<HostId>,
    /// The issuance round: 0 for a key's first enrollment, one more per accepted renewal.
    pub round: u32,
    /// The request already existed; nothing new was queued.
    pub reposted: bool,
}

/// A request as the store holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestView {
    pub request_id: KeyHash,
    pub state: RequestState,
    pub claims: Claims,
    pub image: String,
    pub posted_unix: u64,
    pub renewal_of: Option<HostId>,
    pub round: u32,
    /// The host id it was approved as.
    pub host_id: Option<HostId>,
    /// The host presented a Mac nonce with the request.
    pub mac_nonce_presented: bool,
    /// When and under which TTL the request expired; `None` unless its state is
    /// [`RequestState::Expired`].
    pub expiry: Option<Expiry>,
}

/// Why a request is unenrolled: it waited for an operator longer than the request TTL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Expiry {
    /// Unix seconds: the time the sweep expired it.
    pub expired_unix: u64,
    /// The request TTL it expired under, in seconds.
    pub ttl_secs: u64,
}

/// The operator's decision on a pending request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    pub host_id: HostId,
    pub role: Role,
    pub services: Vec<ServiceName>,
    /// Take the host id over from the key that holds it, or out of its deny row (re-enrollment
    /// of a host). Ignored for a renewal, which keeps its host's key.
    pub replace: bool,
}

/// Who made an approval, when and why, and the Mac nonce it consumes: what
/// [`EnrollmentStore::approve_recorded`] writes into the approval.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalRecord {
    /// The name of the operator's token, or the name of an automatic approver.
    pub approver: String,
    /// Unix seconds: the time of the approval, and the time a Mac nonce is checked at.
    pub approved_unix: u64,
    /// Why, in the approver's words.
    pub reason: String,
    /// The Mac nonce the host presented, by its SHA-256: it must be known, unexpired, unused
    /// and issued for the serial the request claims, and the approval consumes it.
    pub mac_nonce: Option<MacNonceHash>,
}

/// An approval as the store holds it with its record: what the coordinator audits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedApproval {
    pub request_id: KeyHash,
    pub host_id: String,
    pub role: String,
    pub services: Vec<String>,
    /// The approval was asked to take the host id over (`replace`).
    pub replace: bool,
    pub approver: String,
    pub approved_unix: u64,
    pub reason: String,
    /// What the host claimed when it asked.
    pub claims: Claims,
    /// The approval consumed a Mac nonce.
    pub mac_nonce: bool,
}

/// A recorded approval.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Approval {
    pub request_id: KeyHash,
    pub host_id: HostId,
    pub role: Role,
    pub services: Vec<ServiceName>,
    /// The request renews its host: no new host id was minted.
    pub renewal: bool,
    /// The approval took the host id out of a deny row.
    pub cleared_deny: bool,
}

/// The store could not read or write its files.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("cannot use {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    /// A file this release cannot read: not protobuf, no schema, or inconsistent.
    #[error("{path} is not a store file this release reads: {reason}")]
    Corrupt { path: PathBuf, reason: String },
    /// A file written by a newer release.
    #[error("{path} has schema {schema}; this release reads schema 1")]
    NewerSchema { path: PathBuf, schema: u32 },
    /// A file holding a state this release does not know.
    #[error("{path} holds state {value}, which this release does not know")]
    UnknownState { path: PathBuf, value: i32 },
    /// No host of this id was ever approved.
    #[error("no host {0} was approved")]
    UnknownHost(HostId),
}

/// Why a post was refused.
#[derive(Debug, thiserror::Error)]
pub enum PostError {
    /// The CSR does not meet the profile (`bad_csr`, `key_not_p256`).
    #[error(transparent)]
    Profile(#[from] ProfileError),
    /// A `renewal_of` for a host that does not exist, is denied, or holds another key.
    #[error("renewal_not_allowed: the host does not exist, is denied, or holds another key")]
    RenewalNotAllowed,
    /// As many requests as the store holds wait for an operator already.
    #[error("pending_full: the pending queue is full")]
    PendingFull,
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl PostError {
    /// The refusal token the coordinator answers with; `None` for a failure of the store.
    pub fn token(&self) -> Option<&'static str> {
        match self {
            PostError::Profile(profile) => Some(profile.token()),
            PostError::RenewalNotAllowed => Some("renewal_not_allowed"),
            PostError::PendingFull => Some("pending_full"),
            PostError::Store(_) => None,
        }
    }
}

/// Why an approval was refused. The operator RPC maps these to its refusals.
#[derive(Debug, thiserror::Error)]
pub enum ApproveError {
    #[error("unknown_request: no request has this id")]
    UnknownRequest,
    /// The key hash the operator named is not the request's.
    #[error("key_mismatch: the key hash is not the request's")]
    KeyMismatch,
    #[error("not_pending: the request is {0:?}")]
    NotPending(RequestState),
    /// Another key holds the host id and `replace` was not asked for.
    #[error("host_id_taken: another key holds this host id")]
    HostIdTaken,
    /// The host id is denied and `replace` was not asked for (or the request is a renewal).
    #[error("denied: the host id is denied")]
    Denied,
    /// A renewal approved under another host id than the one it renews.
    #[error("renewal_of_other_host: the request renews another host id")]
    RenewalOfOtherHost,
    /// The request is flagged a duplicate claim and the recorded decision does not replace.
    #[error("duplicate_claim: the request is a duplicate claim and replace was not asked for")]
    DuplicateClaim,
    /// The Mac nonce of a recorded approval is refused.
    #[error(transparent)]
    MacNonce(#[from] MacNonceRefusal),
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl ApproveError {
    /// The refusal token; `None` for a failure of the store.
    pub fn token(&self) -> Option<&'static str> {
        match self {
            ApproveError::UnknownRequest => Some("unknown_request"),
            ApproveError::KeyMismatch => Some("key_mismatch"),
            ApproveError::NotPending(_) => Some("not_pending"),
            ApproveError::HostIdTaken => Some("host_id_taken"),
            ApproveError::Denied => Some("denied"),
            ApproveError::RenewalOfOtherHost => Some("renewal_of_other_host"),
            ApproveError::DuplicateClaim => Some("duplicate_claim"),
            ApproveError::MacNonce(refusal) => Some(refusal.token()),
            ApproveError::Store(_) => None,
        }
    }
}

/// Why no certificate was returned.
#[derive(Debug, thiserror::Error)]
pub enum IssueError {
    #[error("unknown_request: no request has this id")]
    UnknownRequest,
    /// The request waits for an operator: the poll's answer is "pending", not a refusal.
    #[error("the request is {0:?}, not approved")]
    NotApproved(RequestState),
    /// The request or its host id is denied.
    #[error("denied: the host is denied")]
    Denied,
    #[error(transparent)]
    Sign(#[from] SignError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl IssueError {
    /// The refusal token; `None` for a pending request and for a failure.
    pub fn token(&self) -> Option<&'static str> {
        match self {
            IssueError::UnknownRequest => Some("unknown_request"),
            IssueError::Denied => Some("denied"),
            IssueError::NotApproved(_) | IssueError::Sign(_) | IssueError::Store(_) => None,
        }
    }
}

/// Why a poll's signature was refused.
#[derive(Debug, thiserror::Error)]
pub enum PollError {
    #[error("unknown_request: no request has this id")]
    UnknownRequest,
    /// The signature is not the request's key's over the poll message.
    #[error("bad_signature: the signature does not verify under the request's key")]
    BadSignature,
    /// The nonce is shorter than 16 or longer than 64 bytes; on the wire, `bad_signature`.
    #[error("bad_signature: the nonce is {0} bytes, not 16 to 64")]
    BadNonce(usize),
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl PollError {
    /// The refusal token; `None` for a failure of the store.
    pub fn token(&self) -> Option<&'static str> {
        match self {
            PollError::UnknownRequest => Some("unknown_request"),
            PollError::BadSignature | PollError::BadNonce(_) => Some("bad_signature"),
            PollError::Store(_) => None,
        }
    }
}
