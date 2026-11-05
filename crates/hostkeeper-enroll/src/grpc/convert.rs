//! The port's plain types and the messages of `hostkeeper.agent.v1`, both ways.
//!
//! An answer the port cannot represent is a [`TransportError`] that names what was wrong: a request
//! id that is not 64 lower-case hex digits, an enum value this daemon does not know (or the
//! UNSPECIFIED of an answer that has to say where a request stands), a reason that is not a token.
//! None is replaced by a default.
//!
//! What the proto says and the port needs:
//!
//! - `Enroll` answers PENDING or DUPLICATE_CLAIM (pending, with an alert for the operator).
//!   Approval and denial reach the daemon in `Poll`; a refusal of `Enroll` is a gRPC status.
//! - `Poll` answers UNSPECIFIED with a nonce (the challenge), PENDING, ISSUED or DENIED. The
//!   message has no reason, so a denied poll carries the reason `denied`, the state's own name.
//! - `Renew` answers the certificate; a refusal is a gRPC status whose message is one reason
//!   token, which is the [`Reason`] of the refusal. Any other status is a transport error. The
//!   certificate comes as DER and is handed on as PEM; the host id is the one the request named.

use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use tonic::Status;
use x509_cert::der::pem::{LineEnding, encode_string};

use crate::wire::{
    EnrollAnswer, EnrollRequest, EnrollState, HostId, Issued, Poll, PollAnswer, Reason,
    RenewAnswer, RenewRequest, RequestId, TransportError,
};

/// The reason of a denied poll: the name of `POLL_STATE_DENIED` without its prefix.
const DENIED: &str = "denied";

/// The type label of a PEM certificate.
const CERTIFICATE: &str = "CERTIFICATE";

/// The most characters of a coordinator's text that an error repeats.
const SHOWN: usize = 64;

/// `text` as a quoted, escaped literal of at most [`SHOWN`] characters, for an error message.
fn quoted(text: &str) -> String {
    format!("{:?}", text.chars().take(SHOWN).collect::<String>())
}

/// The error for an enum value `value` of `field` in an answer to `call` that has no meaning here.
fn unknown(call: &str, field: &str, value: i32) -> TransportError {
    TransportError(format!(
        "{call} answered {field} {value}, which this daemon does not know"
    ))
}

/// The error for a gRPC status `status` that answered `call`.
pub(super) fn status_error(call: &str, status: &Status) -> TransportError {
    TransportError(format!(
        "{call} was answered with the status {:?}: {}",
        status.code(),
        quoted(status.message())
    ))
}

/// `token` as a [`Reason`], or the error that says it is not one.
pub(super) fn reason(call: &str, token: &str) -> Result<Reason, TransportError> {
    Reason::new(token).ok_or_else(|| {
        TransportError(format!(
            "{call} answered the reason {}, which is not a reason token",
            quoted(token)
        ))
    })
}

/// `der` as a PEM document of the type `label` (`CERTIFICATE`, `PRIVATE KEY`).
pub(super) fn pem(label: &str, der: &[u8]) -> Result<String, TransportError> {
    encode_string(label, LineEnding::LF, der)
        .map_err(|e| TransportError(format!("cannot write a {label:?} as PEM: {e}")))
}

pub(super) fn enroll_request(request: &EnrollRequest) -> pb::EnrollRequest {
    let claims = &request.claims;
    pb::EnrollRequest {
        csr_der: request.csr_der.clone(),
        claims: Some(pb::Claims {
            machine_id: claims.machine_id.clone().unwrap_or_default(),
            dmi_serial: claims.product_serial.clone().unwrap_or_default(),
            product_uuid: claims.product_uuid.clone().unwrap_or_default(),
            addresses: claims.network_addresses.clone(),
            mac_serial: claims.mac_serial.clone().unwrap_or_default(),
            platform_uuid: claims.platform_uuid.clone().unwrap_or_default(),
        }),
        image: request.image.clone(),
        renewal_of: request
            .renewal_of
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default(),
        ..pb::EnrollRequest::default()
    }
}

pub(super) fn enroll_answer(reply: pb::EnrollResponse) -> Result<EnrollAnswer, TransportError> {
    let request_id = RequestId::parse(&reply.request_id).ok_or_else(|| {
        TransportError(format!(
            "Enroll answered the request id {}, which is not 64 lower-case hex digits",
            quoted(&reply.request_id)
        ))
    })?;
    let state = match pb::EnrollState::try_from(reply.state) {
        Ok(pb::EnrollState::Pending | pb::EnrollState::DuplicateClaim) => EnrollState::Pending,
        Ok(pb::EnrollState::Unspecified) | Err(_) => {
            return Err(unknown("Enroll", "state", reply.state));
        }
    };
    Ok(EnrollAnswer { request_id, state })
}

pub(super) fn poll_request(poll: &Poll) -> pb::PollRequest {
    pb::PollRequest {
        request_id: poll.request_id.to_string(),
        nonce: poll.nonce.clone(),
        signature: poll.signature.clone(),
    }
}

pub(super) fn poll_answer(reply: pb::PollResponse) -> Result<PollAnswer, TransportError> {
    match pb::PollState::try_from(reply.state) {
        Ok(pb::PollState::Unspecified) => Ok(PollAnswer::Challenge(reply.nonce)),
        Ok(pb::PollState::Pending) => Ok(PollAnswer::Pending),
        Ok(pb::PollState::Denied) => reason("Poll", DENIED).map(PollAnswer::Denied),
        Ok(pb::PollState::Issued) => pem(CERTIFICATE, &reply.certificate_der).map(|cert_pem| {
            PollAnswer::Issued(Issued {
                host_id: reply.host_id,
                cert_pem,
                ca_bundle_pem: reply.ca_bundle_pem,
            })
        }),
        Err(_) => Err(unknown("Poll", "state", reply.state)),
    }
}

pub(super) fn renew_request(request: &RenewRequest) -> pb::RenewRequest {
    pb::RenewRequest {
        csr_der: request.csr_der.clone(),
    }
}

/// The answer to a `Renew` of `host_id`: the certificate, or the refusal a status carries.
pub(super) fn renew_answer(
    reply: Result<tonic::Response<pb::RenewResponse>, Status>,
    host_id: &HostId,
) -> Result<RenewAnswer, TransportError> {
    match reply {
        Ok(response) => {
            let issued = response.into_inner();
            pem(CERTIFICATE, &issued.certificate_der).map(|cert_pem| {
                RenewAnswer::Issued(Issued {
                    host_id: host_id.to_string(),
                    cert_pem,
                    ca_bundle_pem: issued.ca_bundle_pem,
                })
            })
        }
        Err(status) => Reason::new(status.message())
            .map(RenewAnswer::Refused)
            .ok_or_else(|| status_error("Renew", &status)),
    }
}
