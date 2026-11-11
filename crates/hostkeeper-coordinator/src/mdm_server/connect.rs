//! `PUT /mdm` (`docs/design/coordinator-mdm.md`, sections 4.3 and 5.2): the Mac says it is idle or
//! answers a command, and is handed the next one.
//!
//! One request does, in this order:
//!
//! 1. the Mac is known: its certificate names an issued identity whose enrollment is live, and
//!    the `UDID` of the message is the identity's;
//! 2. the contact is recorded, which answers the Mac's outstanding push, so that the next command
//!    queued is pushed at once;
//! 3. the result is taken: the store changes a command only while it is `Sent` to this
//!    enrollment, and any other result is dropped and not told to the gate. A taken result is
//!    told to the gate ([`Event::Result`]); `NotNow` also arms the push floor and puts the command
//!    on this connection's list, so it is not handed back on it;
//! 4. the next command the connection has not tried is claimed and answered, and put on the list.
//!    With none, the answer is empty and the Mac closes the session.
//!
//! Delivery is at least once: the timeout of `--mdm-command-timeout-s` returns a command to the
//! queue even while a slow Mac holds it, so a late answer may find the command `Queued`, `Sent`
//! again or ended, and only a `Sent` one changes.

use bytes::Bytes;
use hostkeeper_mdm_gate::{EnrollmentId, Event};
use hostkeeper_mdm_server::{CommandResult, Status};
use http_body_util::Full;
use hyper::{HeaderMap, Response};

use super::{ConnectionTried, MdmServer, Refusal, auth, checkin::hear, ok};
use crate::http::Peer;
use crate::store::mdm::Answer;

/// The text of a status as the gate spells it.
fn status_text(status: &Status) -> &'static str {
    match status {
        Status::Idle => "Idle",
        Status::Acknowledged => "Acknowledged",
        Status::Error(_) => "Error",
        Status::NotNow => "NotNow",
        Status::CommandFormatError(_) => "CommandFormatError",
    }
}

/// The store's answer for a result's status; none for `Idle`.
fn answer_of(status: &Status) -> Option<Answer> {
    match status {
        Status::Idle => None,
        Status::Acknowledged => Some(Answer::Acknowledged),
        Status::NotNow => Some(Answer::NotNow),
        // A command the Mac could not read is as final as one that failed.
        Status::Error(_) | Status::CommandFormatError(_) => Some(Answer::Error),
    }
}

/// Answers one `/mdm` request: `body` from `peer` with `headers` on the connection whose tried
/// list is `tried`.
pub(crate) async fn answer(
    server: &MdmServer,
    peer: &Peer,
    headers: &HeaderMap,
    body: &[u8],
    tried: &ConnectionTried,
) -> Result<Response<Full<Bytes>>, Refusal> {
    let cert = auth::certificate(peer, headers, body)?;
    let identity = auth::identity(&*server.store, &cert)?;
    let enrollment = auth::enrollment(&*server.store, &identity, &cert)?;
    let result = CommandResult::parse(body).map_err(|why| {
        tracing::warn!(%why, "refused a result that is not one");
        Refusal::BadRequest
    })?;
    if result.device != identity.udid {
        return Err(auth::Refused::OtherDevice.into());
    }
    let id = enrollment.enrollment_id.as_str();
    server.queue.contact(id)?;
    let mut hearing = true;
    if let (Some(answer), Some(uuid)) = (answer_of(&result.status), result.command_uuid.as_deref())
    {
        let taken = server.queue.answer(id, uuid, answer, body)?;
        if taken == crate::store::mdm::Answered::Taken {
            if answer == Answer::NotNow {
                tried.add(id, uuid);
            }
        } else {
            // A result for a command that is not sent to this enrollment changes nothing, and
            // the gate does not hear it either.
            hearing = false;
        }
    }
    if hearing {
        hear(
            server,
            EnrollmentId::parse(id).map(|id| Event::Result {
                id,
                command_uuid: result.command_uuid.clone(),
                status: status_text(&result.status).to_owned(),
                payload: body.to_vec(),
            }),
        )
        .await;
    }
    match server.queue.claim(id, &tried.of(id))? {
        Some(command) => {
            tried.add(id, &command.id);
            Ok(Response::new(Full::new(Bytes::from(command.payload))))
        }
        None => Ok(ok()),
    }
}

#[cfg(test)]
#[path = "connect_tests.rs"]
mod tests;
