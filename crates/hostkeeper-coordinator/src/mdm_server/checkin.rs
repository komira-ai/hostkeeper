//! `PUT /checkin` (`docs/design/coordinator-mdm.md`, sections 4.2 and 5.1): `Authenticate`,
//! `TokenUpdate` and `CheckOut`.
//!
//! **`Authenticate`** enters the Mac's enrollment. The certificate it presents must be an issued
//! identity (the SCEP step wrote it) for the serial and the UDID the message names. Then, under
//! owner decision B (4.2):
//!
//! - no live enrollment of the serial: the enrollment is entered;
//! - the live enrollment of the serial is this certificate's: the message is a repeat and changes
//!   nothing;
//! - another live enrollment: it is replaced when the serial is in the ABM table (read here, at
//!   `Authenticate`, and listed, not removed) or an operator confirmed the identity; the old
//!   enrollment, its commands and its identity end in one change of the store, so its certificate
//!   is never accepted again; otherwise the request is refused and audited and the live enrollment
//!   stays. Every replacement is an audit row, written before the change.
//!
//! One `Authenticate` runs at a time ([`MdmServer::authenticating`]), so the live enrollment read
//! here is the one replaced. The ABM table is read just before the change and not in its
//! transaction (the table store has no such call): an ABM sync that removes the serial in between
//! lets one replacement through, which the audit row shows. A serial the table lists is still ours
//! at that moment, and the next `Authenticate` reads the table again.
//!
//! The gate hears each message as an [`Event`]; a replaced enrollment is told to it as a
//! `CheckOut`, so its record ends and the serial is not ambiguous.

use hostkeeper_mdm_gate::{EnrollmentId, Event};
use hostkeeper_mdm_server::{Authenticate, CheckIn, TokenUpdate};
use hyper::HeaderMap;

use super::{MdmServer, Refusal, auth, ok};
use crate::audit_actions;
use crate::http::Peer;
use crate::service::Coordinator;
use crate::store::mdm::{Enrollment, Identity};

type Answer = hyper::Response<http_body_util::Full<bytes::Bytes>>;

/// Answers one check-in `body` from `peer` with `headers`.
pub(crate) async fn answer(
    coordinator: &Coordinator,
    server: &MdmServer,
    peer: &Peer,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<Answer, Refusal> {
    let cert = auth::certificate(peer, headers, body)?;
    let identity = auth::identity(&*server.store, &cert)?;
    let message = CheckIn::parse(body).map_err(|why| {
        tracing::warn!(%why, "refused a check-in that is not one");
        Refusal::BadRequest
    })?;
    match message {
        CheckIn::Authenticate(message) => {
            authenticate(coordinator, server, &cert, &identity, &message).await
        }
        CheckIn::TokenUpdate(message) => token_update(server, &cert, &identity, &message).await,
        CheckIn::CheckOut(message) => {
            let enrollment = live(server, &identity, &cert, message.id.key())?;
            check_out(server, &enrollment).await
        }
        // The Mac gives the server its bootstrap token. This server keeps none; it takes the
        // message so that the Mac moves on.
        CheckIn::SetBootstrapToken(_) => Ok(ok()),
        // This server holds no bootstrap token and no declarations (DDM is CM15).
        CheckIn::GetBootstrapToken(_) | CheckIn::DeclarativeManagement(_) => Ok(
            crate::http::error(hyper::StatusCode::NOT_FOUND, "not_found"),
        ),
    }
}

/// The live enrollment of `identity`'s certificate, for a message about the device `device`.
fn live(
    server: &MdmServer,
    identity: &Identity,
    cert: &str,
    device: &str,
) -> Result<Enrollment, Refusal> {
    if device != identity.udid {
        return Err(auth::Refused::OtherDevice.into());
    }
    Ok(auth::enrollment(&*server.store, identity, cert)?)
}

/// One audit row of the MDM server: `action` about `serial`.
fn audit(coordinator: &Coordinator, action: &str, serial: &str, reason: &str) -> bool {
    coordinator
        .audit_setup("coordinator", action, Some(serial), reason.to_owned())
        .is_ok()
}

async fn authenticate(
    coordinator: &Coordinator,
    server: &MdmServer,
    cert: &str,
    identity: &Identity,
    message: &Authenticate,
) -> Result<Answer, Refusal> {
    if message.id.key() != identity.udid {
        return Err(auth::Refused::OtherDevice.into());
    }
    if message.serial_number.as_deref() != Some(identity.serial.as_str()) {
        audit(
            coordinator,
            audit_actions::MDM_SERIAL_MISMATCH,
            &identity.serial,
            "the Authenticate names another serial than the identity's",
        );
        return Err(Refusal::Forbidden);
    }
    let _one = server.authenticating.lock().await;
    let live = server.store.live_enrollment(&identity.serial)?;
    if live.as_ref().and_then(|l| l.cert_sha256.as_deref()) == Some(cert) {
        // A repeat of the Authenticate that entered this enrollment.
        return Ok(ok());
    }
    if live.is_some() {
        let listed = server.serial_in_abm(&identity.serial)?;
        if !listed && !identity.confirmed {
            audit(
                coordinator,
                audit_actions::MDM_REPLACE_REFUSED,
                &identity.serial,
                "the serial is not in the ABM table and no operator confirmed it: the live enrollment stays",
            );
            return Err(Refusal::Forbidden);
        }
        let reason = if listed {
            "the serial is in the ABM table"
        } else {
            "an operator confirmed the identity"
        };
        if !audit(
            coordinator,
            audit_actions::MDM_ENROLLMENT_REPLACED,
            &identity.serial,
            reason,
        ) {
            return Err(Refusal::Unavailable);
        }
    }
    let id = new_id(server)?;
    let mut enrollment = Enrollment::new(&id, &identity.serial, &identity.udid, server.now());
    enrollment.cert_sha256 = Some(cert.to_owned());
    let replaced = server.store.replace_live(&enrollment)?;
    for old in &replaced {
        server.queue.forget(old);
        hear(
            server,
            EnrollmentId::parse(old).map(|id| Event::CheckOut { id }),
        )
        .await;
    }
    hear(
        server,
        EnrollmentId::parse(&id).map(|id| Event::Authenticate {
            id,
            udid: identity.udid.clone(),
            serial: Some(identity.serial.clone()),
        }),
    )
    .await;
    Ok(ok())
}

/// A new enrollment id: 16 random bytes in hex, which the gate keeps its record under.
fn new_id(server: &MdmServer) -> Result<String, Refusal> {
    let mut bytes = [0_u8; 16];
    if !server.random(&mut bytes) {
        tracing::error!("the system gave no random bytes for an enrollment id");
        return Err(Refusal::Internal);
    }
    Ok(hex::encode(bytes))
}

async fn token_update(
    server: &MdmServer,
    cert: &str,
    identity: &Identity,
    message: &TokenUpdate,
) -> Result<Answer, Refusal> {
    let enrollment = live(server, identity, cert, message.id.key())?;
    // The channel of a user is not the device's: its token is not the Mac's.
    if message.enrollment_user_id.is_some()
        || message.user_id.is_some()
        || message.not_on_console == Some(true)
    {
        return Ok(ok());
    }
    let kept = server.store.set_push(
        &enrollment.enrollment_id,
        &message.token,
        &message.push_magic,
        &message.topic,
    )?;
    if !kept {
        return Err(auth::Refused::NotLive.into());
    }
    hear(
        server,
        EnrollmentId::parse(&enrollment.enrollment_id).map(|id| Event::TokenUpdate {
            id,
            udid: enrollment.udid.clone(),
            tally: None,
        }),
    )
    .await;
    Ok(ok())
}

async fn check_out(server: &MdmServer, enrollment: &Enrollment) -> Result<Answer, Refusal> {
    server
        .store
        .check_out(&enrollment.enrollment_id, server.now())?;
    server.queue.forget(&enrollment.enrollment_id);
    hear(
        server,
        EnrollmentId::parse(&enrollment.enrollment_id).map(|id| Event::CheckOut { id }),
    )
    .await;
    Ok(ok())
}

/// Tells the gate, when there is one, what the Mac said. A gate that cannot write its record is
/// logged and the Mac is answered all the same: the gate learns the device again from its next
/// message.
pub(super) async fn hear(server: &MdmServer, event: Option<Event>) {
    let (Some(gate), Some(event)) = (&server.gate, event) else {
        return;
    };
    if let Err(why) = gate.handle(event).await {
        tracing::error!(%why, "the gate could not write a device record");
    }
}

#[cfg(test)]
#[path = "checkin_tests.rs"]
mod tests;
