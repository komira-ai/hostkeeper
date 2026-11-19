//! `Console/Open`, `Console/List` and `Console/Close` (`docs/design/console-broker.md`, decisions
//! D2, D4, D11 and D12): the one session per host, which no call here connects to the device.
//!
//! `Open` checks the caller, the request and the host's device as the module above says, takes
//! the host's slot (`console_busy` when it has a session, `too_many_sessions` past
//! `--console-max-sessions`, attached sessions included), draws the session's 256-bit id, and
//! writes `console_open` with the session's id and kind after the operator's reason; with no line
//! there is no session. The session waits `--console-attach-s` to be attached by the WebSocket
//! route ([`super::route`]); one that is not ends when its window has passed, the sweep or the
//! first `Console` call after that writing its `console_close` by `coordinator`, with the time it
//! ended. `Close` ends a host's session, whoever holds it: an unattached one at once, with
//! `console_close` by the closer; an attached one by stopping its stream (close code 4007), whose
//! end writes that line. Close lines are best effort.

use std::time::Duration;

use hostkeeper_console::close::Close;
use hostkeeper_console::operators::Need;
use hostkeeper_console::reason::audit_reason;
use hostkeeper_console::session::{Kind, Refused, SESSION_ID_BYTES, Session, SessionId, Sessions};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::ConsoleRefusal as Refusal;

use super::{host_id, required_reason};
use crate::audit_actions::AuditOnly;
use crate::operator::Caller;
use crate::reconcile::COORDINATOR;
use crate::service::Coordinator;

impl Coordinator {
    /// `Console/Open`: takes the host's session slot and writes the audit line, as the module
    /// says, without contacting the device.
    ///
    /// # Errors
    /// The caller's refusal, `no_console_access`, `invalid_request`, `reason_required`,
    /// `reason_too_long`, the host's ([`Self::console_device`]), `console_busy`,
    /// `too_many_sessions`, `store_failed` (no id could be drawn) or `audit_unavailable`.
    pub(crate) fn console_open(
        &self,
        caller: &Caller,
        request: &proto::OpenConsoleRequest,
    ) -> Result<proto::OpenConsoleResponse, Refusal> {
        let (by, grant) = self.console_grant(caller, Need::Console)?;
        let host = host_id(&request.host_id)?;
        let reason = required_reason(&request.reason)?;
        let refuse = |cause| self.console_denied(by, &host, "open", &reason, cause);
        let device = self
            .console_device(&host, &grant.targets, false)
            .map_err(refuse)?;
        let now = (self.clock)();
        let mut table = self.console.lock_table();
        let sessions = &mut table.sessions;
        self.end_unattached(sessions, now);
        sessions.admit(host.as_str()).map_err(|why| {
            refuse(match why {
                Refused::Busy => Refusal::ConsoleBusy,
                Refused::Full => Refusal::TooManySessions,
            })
        })?;
        let mut bytes = [0u8; SESSION_ID_BYTES];
        if !(self.console.random)(&mut bytes) {
            tracing::error!("no session id could be drawn: the random source failed");
            return Err(Refusal::StoreFailed);
        }
        let id = SessionId::from_bytes(bytes);
        let session = id.to_string();
        let details = [
            ("session", session.as_str()),
            ("kind", device.kind.as_str()),
        ];
        self.console_audit(
            AuditOnly::ConsoleOpen,
            by,
            Some(host.as_str()),
            &audit_reason(&reason, &details),
        )?;
        sessions.insert(Session {
            id,
            host_id: host.to_string(),
            kind: device.kind,
            opened_by: by.to_owned(),
            opened_unix: self.ops.unix_now(),
            opened_at: now,
            reason,
            attachment: None,
        });
        Ok(proto::OpenConsoleResponse {
            kind: proto_kind(device.kind).into(),
            session,
            expires_in_secs: sessions.window().as_secs(),
        })
    }

    /// `Console/List`: every open session, by host id, without its id. Not audited.
    ///
    /// # Errors
    /// The caller's refusal, or `no_console_access`.
    pub(crate) fn console_list(
        &self,
        caller: &Caller,
        _request: &proto::ListConsoleSessionsRequest,
    ) -> Result<proto::ListConsoleSessionsResponse, Refusal> {
        self.console_operator(caller, Need::Console)?;
        let mut table = self.console.lock_table();
        let sessions = &mut table.sessions;
        self.end_unattached(sessions, (self.clock)());
        Ok(proto::ListConsoleSessionsResponse {
            sessions: sessions
                .iter()
                .map(|session| proto::ConsoleSession {
                    host_id: session.host_id.clone(),
                    kind: proto_kind(session.kind).into(),
                    opened_by: session.opened_by.clone(),
                    opened_unix: session.opened_unix,
                    attached: session.attachment.is_some(),
                    reason: session.reason.clone(),
                })
                .collect(),
        })
    }

    /// `Console/Close`: ends the session of a host, whoever holds it, and writes the closer's
    /// line.
    ///
    /// # Errors
    /// The caller's refusal, `no_console_access`, `invalid_request` or `no_session`.
    pub(crate) fn console_close(
        &self,
        caller: &Caller,
        request: &proto::CloseConsoleRequest,
    ) -> Result<proto::CloseConsoleResponse, Refusal> {
        let by = self.console_operator(caller, Need::Console)?;
        let host = host_id(&request.host_id)?;
        let mut table = self.console.lock_table();
        self.end_unattached(&mut table.sessions, (self.clock)());
        if let Some(live) = table.live.get(host.as_str()) {
            // The stream's end removes the session and writes its line, by this closer.
            live.stop(Close::ClosedByOperator, by);
            return Ok(proto::CloseConsoleResponse {});
        }
        let Some(session) = table.sessions.close(host.as_str()) else {
            return Err(self.console_denied(by, &host, "close", "", Refusal::NoSession));
        };
        self.closed(by, &session, "closed", self.ops.unix_now(), &[]);
        self.console.ended.notify_waiters();
        Ok(proto::CloseConsoleResponse {})
    }

    /// Ends every session of `sessions` not attached by `now` and writes each one's close line.
    pub(super) fn end_unattached(&self, sessions: &mut Sessions, now: Duration) {
        let window = sessions.window();
        for session in sessions.expire(now) {
            let ended = session.opened_unix.saturating_add(window.as_secs());
            self.closed(COORDINATOR, &session, "not_attached", ended, &[]);
            self.console.ended.notify_waiters();
        }
    }

    /// Writes the `console_close` line of `session` by `by` for `cause`, ended at `ended_unix`,
    /// with the stream's `more` details after the others; best effort.
    pub(super) fn closed(
        &self,
        by: &str,
        session: &Session,
        cause: &str,
        ended_unix: u64,
        more: &[(&str, &str)],
    ) {
        let id = session.id.to_string();
        let ended = ended_unix.to_string();
        let mut details = vec![
            ("session", id.as_str()),
            ("kind", session.kind.as_str()),
            ("opened_by", session.opened_by.as_str()),
            ("cause", cause),
            ("ended_unix", ended.as_str()),
        ];
        details.extend_from_slice(more);
        let reason = audit_reason("", &details);
        let host = Some(session.host_id.as_str());
        let _ = self.console_audit(AuditOnly::ConsoleClose, by, host, &reason);
    }
}

/// The proto's value of `kind`.
fn proto_kind(kind: Kind) -> proto::ConsoleKind {
    match kind {
        Kind::Serial => proto::ConsoleKind::Serial,
        Kind::Screen => proto::ConsoleKind::Screen,
    }
}
