//! `Enroll`, `Poll` and `Renew` of `hostkeeper.agent.v1.Coordinator`, as handlers over the
//! protobuf messages, and [`call`], which answers one encoded call from a peer.
//!
//! - `Enroll` and `Poll` need no client certificate: the host has none yet. The caller is
//!   authenticated by the key it proves: the CSR's self-signature, then the signed nonce.
//! - `Renew` needs the host's client certificate, one this CA issued: the host is the one it
//!   names, and the new certificate is for the key it certifies.
//!
//! A transport serves them on the heartbeat listener, which lets a connection without a client
//! certificate through the handshake ([`crate::listen`]); [`call`] refuses a `Renew` without one
//! (`forbidden`) before it reads the body, as the heartbeat route does a heartbeat.
//!
//! The coordinator's own enrollment changes are audited in its ops store under the name
//! `coordinator` ([`Enrollment::audit_to`]): a new request (`enroll_requested`; a repost of a
//! request is not one, even one that marks it `duplicate_claim`, which the store alerts on),
//! its certificate's first issue (`enroll_issued`, once per request and round, as the store
//! decides under its lock), a renewal (`cert_renewed`) and a request that expired
//! (`request_expired`, `expiry`). The entry names the host when there
//! is one, and its reason holds the request id, the round and the serial: never the CSR, a key,
//! a nonce or a signature.

use std::time::Duration;

use hostkeeper_enrollment::ca::check_request;
use hostkeeper_enrollment::enroll::{Claims, IssueError, NewRequest, RequestState};
use hostkeeper_enrollment::names::{HostId, KeyHash};
use hostkeeper_proto::hostkeeper::agent::v1::{
    CoordinatorRefusal, EnrollRequest, EnrollResponse, EnrollState, PollRequest, PollResponse,
    PollState, RenewRequest, RenewResponse,
};
use hyper::body::Body;
use prost::Message;

use super::refusal::{RpcError, rpc_error};
use super::{Enrollment, MacAllowEntry, MacOutcome};
use crate::http::{BoxError, Peer, read_body};
use crate::ops::AuditEntry;
use crate::reconcile::COORDINATOR;

/// The audit action of a new request.
pub(crate) const ACTION_REQUESTED: &str = "enroll_requested";

/// The audit action of a request's certificate issued for the first time.
pub(crate) const ACTION_ISSUED: &str = "enroll_issued";

/// The audit action of a host's certificate renewed.
pub(crate) const ACTION_RENEWED: &str = "cert_renewed";

/// The coordinator's audit entry of `action` about `host`, with `reason`.
pub(super) fn audit_entry(action: &str, host: Option<&HostId>, reason: String) -> AuditEntry {
    let mut entry = AuditEntry::new(COORDINATOR, action);
    entry.node_id = host.map(HostId::to_string);
    entry.reason = Some(reason);
    entry
}

/// The most bytes of one request message read: a CSR is at most 4096 bytes, and the rest of an
/// `Enroll` (the claims, the image, a host id) fits in as many again.
pub const MAX_MESSAGE_BYTES: usize = 8192;

/// A method of `hostkeeper.agent.v1.Coordinator` that [`call`] answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// Ask for a certificate.
    Enroll,
    /// Ask whether a request was approved, and take its certificate.
    Poll,
    /// A new certificate for the key the host holds.
    Renew,
}

impl Method {
    /// Every method, in the proto's order.
    pub const ALL: [Method; 3] = [Method::Enroll, Method::Poll, Method::Renew];

    /// The method's gRPC path.
    #[must_use]
    pub fn path(self) -> &'static str {
        match self {
            Method::Enroll => "/hostkeeper.agent.v1.Coordinator/Enroll",
            Method::Poll => "/hostkeeper.agent.v1.Coordinator/Poll",
            Method::Renew => "/hostkeeper.agent.v1.Coordinator/Renew",
        }
    }

    /// Whether a caller must present a client certificate: `Renew` only.
    #[must_use]
    pub fn needs_certificate(self) -> bool {
        self == Method::Renew
    }
}

/// Answers one call of `method` by `peer`, whose request is `body`: the encoded response, or why
/// not. A `Renew` without a client certificate is refused `forbidden` before the body is read;
/// a body larger than [`MAX_MESSAGE_BYTES`], not all there after `patience`, or not the
/// method's message is [`RpcError::Malformed`].
///
/// # Errors
/// [`RpcError`]: the refusal, a malformed request, or a failure of the coordinator.
pub async fn call<B>(
    enrollment: &Enrollment,
    method: Method,
    peer: &Peer,
    body: B,
    patience: Duration,
) -> Result<Vec<u8>, RpcError>
where
    B: Body,
    B::Error: Into<BoxError>,
{
    if method.needs_certificate() && peer.is_none() {
        return Err(RpcError::Refused(CoordinatorRefusal::Forbidden));
    }
    let bytes = read_body(body, MAX_MESSAGE_BYTES, patience)
        .await
        .ok_or(RpcError::Malformed)?;
    match method {
        Method::Enroll => enrollment
            .enroll(decode(&bytes)?)
            .map(|answer| answer.encode_to_vec()),
        Method::Poll => enrollment
            .poll(decode(&bytes)?)
            .map(|answer| answer.encode_to_vec()),
        Method::Renew => enrollment
            .renew(peer.as_deref(), decode(&bytes)?)
            .map(|answer| answer.encode_to_vec()),
    }
}

/// `bytes` as the message `M`.
fn decode<M: Message + Default>(bytes: &[u8]) -> Result<M, RpcError> {
    M::decode(bytes).map_err(|_| RpcError::Malformed)
}

/// What an `Enroll` did ([`Enrollment::enrolled`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Enrolled {
    /// The answer to the host.
    pub response: EnrollResponse,
    /// The allowlist entry the request was approved as by its Mac nonce, if it was: the
    /// coordinator assigns its services ([`crate::service::Coordinator::enroll`]).
    pub by_nonce: Option<MacAllowEntry>,
}

/// The refusal `refusal`, as an error.
fn refused(refusal: CoordinatorRefusal) -> RpcError {
    RpcError::Refused(refusal)
}

impl Enrollment {
    /// `Enroll` ([`Enrollment::enrolled`]), answered: the services of a Mac approved by its
    /// nonce are not assigned here, but by the coordinator ([`crate::service::Coordinator::enroll`]),
    /// which is what the gRPC service calls.
    ///
    /// # Errors
    /// As [`Enrollment::enrolled`].
    pub fn enroll(&self, request: EnrollRequest) -> Result<EnrollResponse, RpcError> {
        self.enrolled(request).map(|enrolled| enrolled.response)
    }

    /// `Enroll`: records the request of the CSR's key (its id is the key's SHA-256, computed
    /// here), pending until an operator approves it. The same key again is the same request.
    /// A new request while the pending queue is full is refused `pending_full`. A pending
    /// request that carries a Mac nonce is approved by it when every condition of the Mac path
    /// holds ([`Enrollment::mac_auto_approve`]), and the allowlist entry it was approved as is
    /// returned with the answer; otherwise it stays pending. A new request (not a repost) is
    /// told to the MDM gate ([`Enrollment::with_posted`]) once it is recorded.
    ///
    /// # Errors
    /// `bad_csr`, `key_not_p256`, `renewal_not_allowed` (also for a `renewal_of` that is no
    /// host id), `pending_full`, or [`RpcError::Internal`].
    pub fn enrolled(&self, request: EnrollRequest) -> Result<Enrolled, RpcError> {
        let renewal_of = match request.renewal_of.as_str() {
            "" => None,
            text => Some(
                HostId::parse(text).map_err(|_| refused(CoordinatorRefusal::RenewalNotAllowed))?,
            ),
        };
        let claims = request.claims.unwrap_or_default();
        let posted = self
            .store
            .post(
                NewRequest {
                    csr_der: request.csr_der,
                    claims: Claims {
                        machine_id: claims.machine_id,
                        dmi_serial: claims.dmi_serial,
                        dmi_product_uuid: claims.product_uuid,
                        addresses: claims.addresses,
                        mac_serial: claims.mac_serial,
                        mac_platform_uuid: claims.platform_uuid,
                        bmc_mac: claims.bmc_mac,
                    },
                    image: request.image,
                    renewal_of,
                    mac_nonce_presented: !request.mac_nonce.is_empty(),
                },
                self.clock.now_unix(),
            )
            .map_err(rpc_error)?;
        if !posted.reposted {
            let state = if posted.state == RequestState::DuplicateClaim {
                "duplicate_claim"
            } else {
                "pending"
            };
            self.audit(audit_entry(
                ACTION_REQUESTED,
                posted.renewal_of.as_ref(),
                format!(
                    "request {}; round {}; {state}",
                    posted.request_id, posted.round
                ),
            ));
            self.posted.iter().for_each(|told| told());
        }
        let by_nonce = if !request.mac_nonce.is_empty() && posted.state.is_pending() {
            match self.mac_auto_approve(&posted.request_id, &request.mac_nonce) {
                MacOutcome::Approved(entry) => Some(entry),
                MacOutcome::Declined(_) | MacOutcome::Refused(_) => None,
            }
        } else {
            None
        };
        // A request already approved, issued or denied is polled for where it stands. A post never
        // answers an expired request: the store starts it over.
        let state = match posted.state {
            _ if by_nonce.is_some() => EnrollState::Unspecified,
            RequestState::Pending => EnrollState::Pending,
            RequestState::DuplicateClaim => EnrollState::DuplicateClaim,
            RequestState::Approved
            | RequestState::Issued
            | RequestState::Denied
            | RequestState::Expired => EnrollState::Unspecified,
        };
        Ok(Enrolled {
            response: EnrollResponse {
                request_id: posted.request_id.to_hex(),
                state: state as i32,
            },
            by_nonce,
        })
    }

    /// `Poll`. An unsigned one (empty nonce and signature) is answered with a nonce and nothing
    /// else. A signed one must carry a nonce given for the request and not yet taken (it is
    /// taken now) and its signature by the request's key; it is answered where the request
    /// stands (the certificate is issued at the first poll after approval, the same bytes at
    /// every later one), with a nonce for the next.
    ///
    /// # Errors
    /// `unknown_request`, `bad_signature`, or [`RpcError::Internal`].
    pub fn poll(&self, request: PollRequest) -> Result<PollResponse, RpcError> {
        let id = KeyHash::from_hex(&request.request_id)
            .ok_or(refused(CoordinatorRefusal::UnknownRequest))?;
        if request.nonce.is_empty() && request.signature.is_empty() {
            self.store
                .request(&id)
                .map_err(rpc_error)?
                .ok_or(refused(CoordinatorRefusal::UnknownRequest))?;
            return Ok(PollResponse {
                nonce: self.nonce(&id)?,
                ..PollResponse::default()
            });
        }
        if !self.nonces.take(&id, &request.nonce) {
            return Err(refused(CoordinatorRefusal::BadSignature));
        }
        self.store
            .verify_poll(&id, &request.nonce, &request.signature)
            .map_err(rpc_error)?;
        let nonce = self.nonce(&id)?;
        let only = |state: PollState| PollResponse {
            state: state as i32,
            nonce: nonce.clone(),
            ..PollResponse::default()
        };
        match self.store.issue(&self.ca, &id) {
            Ok(issued) => {
                if issued.first {
                    self.audit(audit_entry(
                        ACTION_ISSUED,
                        Some(&issued.host_id),
                        format!(
                            "request {id}; round {}; serial {}",
                            issued.round, issued.serial
                        ),
                    ));
                }
                Ok(PollResponse {
                    state: PollState::Issued as i32,
                    certificate_der: issued.certificate_der,
                    host_id: issued.host_id.to_string(),
                    ca_bundle_pem: self.ca_pem(),
                    nonce,
                })
            }
            Err(IssueError::NotApproved(_)) => Ok(only(PollState::Pending)),
            Err(IssueError::Denied) => Ok(only(PollState::Denied)),
            Err(other) => Err(rpc_error(other)),
        }
    }

    /// `Renew`, by the holder of the client certificate `peer`: a new certificate for the host
    /// it names and the key it certifies, which the CSR must be for.
    ///
    /// # Errors
    /// `forbidden` (no certificate, or one this CA did not issue to a host), `denied`,
    /// `renewal_not_allowed` (a duplicate host, a CSR for another key, or a key the host no
    /// longer holds), `bad_csr`, `key_not_p256`, or [`RpcError::Internal`].
    pub fn renew(
        &self,
        peer: Option<&[u8]>,
        request: RenewRequest,
    ) -> Result<RenewResponse, RpcError> {
        let current = peer
            .and_then(|der| self.ca_bundle().host_certificate(der))
            .ok_or(refused(CoordinatorRefusal::Forbidden))?;
        let host = current.host_id;
        if self.store.is_denied(&host).map_err(rpc_error)? {
            return Err(refused(CoordinatorRefusal::Denied));
        }
        if self.store.is_duplicate(&host).map_err(rpc_error)? {
            return Err(refused(CoordinatorRefusal::RenewalNotAllowed));
        }
        let key = check_request(&request.csr_der)
            .map_err(rpc_error)?
            .key()
            .clone();
        if key != current.key
            || !self
                .store
                .renew_allowed(&host, &key.key_hash())
                .map_err(rpc_error)?
        {
            return Err(refused(CoordinatorRefusal::RenewalNotAllowed));
        }
        let serial = self.store.allocate_serial().map_err(rpc_error)?;
        let certificate = self.ca.issue_host(&host, &key, serial).map_err(rpc_error)?;
        tracing::info!(host_id = %host, serial, "renewed a host certificate");
        self.audit(audit_entry(
            ACTION_RENEWED,
            Some(&host),
            format!("serial {serial}"),
        ));
        Ok(RenewResponse {
            certificate_der: certificate.der,
            ca_bundle_pem: self.ca_pem(),
        })
    }

    /// A new nonce for `request`.
    fn nonce(&self, request: &KeyHash) -> Result<Vec<u8>, RpcError> {
        self.nonces.give(request).ok_or_else(|| {
            tracing::error!("no random bytes for a poll nonce");
            RpcError::Internal
        })
    }
}
