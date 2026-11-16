//! `Ssh.HostKeys`, `Ssh.PinHostKeys` and `Ssh.Tunnel` over [`Coordinator`] (docs/design/ssh-
//! through-coordinator.md, sections 7 and 8), and what a heartbeat tells the book.
//!
//! The three calls check who may reach which host as `Ssh.SignUserCert` does ([`crate::ssh_sign`]),
//! in the same order, and each refusal after the caller is known is an audit line:
//!
//! 1. the caller is an operator, and the coordinator holds an SSH user CA and tunnels (else
//!    `not_configured`);
//! 2. for a tunnel, the instance is not draining and the first frame is `open`;
//! 3. the host id is one, the operators file lists the caller, the host is enrolled and the file
//!    permits the caller the host;
//! 4. a tunnel then takes a slot of the instance (`max_tunnels`), the book checks the certificate,
//!    the host's address and liveness and the operator's tunnels and records the tunnel with its
//!    audit line in one write, and the instance dials the host: when it cannot, the tunnel's
//!    row is closed (`error`) and the call refused `dial_failed`.
//!
//! The book blocks; every call of it from here runs off the runtime's threads.

use std::net::IpAddr;
use std::sync::Arc;

use futures::{Stream, StreamExt};
use hostkeeper_enrollment::enroll::EnrolledHost;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::SshRefusal as Refusal;
use hostkeeper_proto::hostkeeper::operator::v1::tunnel_frame::Frame;
use tonic::Status;

use crate::operator::Caller;
use crate::service::Coordinator;
use crate::ssh_book::{HostKeys, OpenRefused, Opening, PinRefused};
use crate::ssh_operators::Target;
use crate::ssh_sign::{Signing, class_of, deny};
use crate::ssh_tunnel::{Dialed, Failed, Incoming, NoSlot, Outgoing, Tunnels};
use crate::wait::ConnectionHold;

/// The most fingerprints a pin holds.
const MAX_PINNED: usize = 8;

/// The host a call reaches, and the checks it passed.
struct Reached {
    host_id: String,
    host: EnrolledHost,
}

/// A refusal of the checks, and the host to write its line for (none: no line is written).
struct Refused {
    why: Refusal,
    audit_for: Option<String>,
}

impl Refused {
    fn plain(why: Refusal) -> Self {
        Self {
            why,
            audit_for: None,
        }
    }

    fn audited(why: Refusal, host_id: &str) -> Self {
        Self {
            why,
            audit_for: Some(host_id.to_owned()),
        }
    }

    /// Writes the line, if the refusal has one, and gives the refusal.
    fn write(self, ssh: &Signing, by: &str) -> Refusal {
        if let Some(host_id) = &self.audit_for {
            ssh.refused(by, host_id, self.why);
        }
        self.why
    }

    /// [`Self::write`] off the runtime's threads.
    async fn write_off_runtime(self, ssh: &Signing, by: &str) -> Refusal {
        if let Some(host_id) = self.audit_for.clone() {
            let (ledger, by, why) = (Arc::clone(&ssh.ledger), by.to_owned(), self.why);
            // A line that could not be written is logged by `deny`: the refusal stands.
            let _ = tokio::task::spawn_blocking(move || deny(&*ledger, &by, &host_id, why)).await;
        }
        self.why
    }
}

/// Whether `fingerprint` is `SHA256:` and the 43 base64 characters OpenSSH prints.
fn fingerprint_ok(fingerprint: &str) -> bool {
    fingerprint.strip_prefix("SHA256:").is_some_and(|digest| {
        digest.len() == 43
            && digest
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'+' || c == b'/')
    })
}

/// `HostKeysResponse` for `keys`: the pinned set and its state.
fn keys_response(keys: &HostKeys) -> proto::HostKeysResponse {
    proto::HostKeysResponse {
        fingerprints: keys.pinned.clone().unwrap_or_default(),
        state: keys.state().to_owned(),
    }
}

impl Coordinator {
    /// The SSH signing and the tunnels, when the coordinator holds both.
    pub(crate) fn ssh_tunnels(&self) -> Option<(&Signing, &Arc<Tunnels>)> {
        self.ssh.as_ref().zip(self.tunnels.as_ref())
    }

    /// The checks every call that reaches a host makes after its host id is read: the id is one,
    /// the operators file lists `operator`, the host is enrolled, and the file permits the
    /// operator the host.
    fn ssh_reach(&self, ssh: &Signing, operator: &str, host_id: &str) -> Result<Reached, Refused> {
        let host_id = hostkeeper_enrollment::names::HostId::parse(host_id)
            .map_err(|_| Refused::plain(Refusal::InvalidRequest))?;
        let host_id = host_id.as_str();
        let operators = ssh.operators();
        if !operators.lists(operator) {
            return Err(Refused::audited(Refusal::NotPermitted, host_id));
        }
        let host = match self.enrolled_host(host_id) {
            Ok(Some(host)) => host,
            Ok(None) => return Err(Refused::audited(Refusal::UnknownHost, host_id)),
            Err(why) => {
                tracing::error!(%why, "the enrolled host could not be read");
                return Err(Refused::plain(Refusal::StoreFailed));
            }
        };
        let target = Target {
            id: host_id,
            group: &host.host_group,
            class: class_of(ssh, host_id),
        };
        if !operators.permits(operator, &target) {
            return Err(Refused::audited(Refusal::NotPermitted, host_id));
        }
        Ok(Reached {
            host_id: host_id.to_owned(),
            host,
        })
    }

    /// `Ssh.HostKeys`: the host keys pinned for a host, and whether the host reports the same.
    /// Changes nothing.
    ///
    /// # Errors
    /// [`Refusal`]: the call is refused.
    pub(crate) fn ssh_host_keys(
        &self,
        caller: &Caller,
        request: &proto::HostKeysRequest,
    ) -> Result<proto::HostKeysResponse, Refusal> {
        let operator = caller.operator().map_err(Refusal::from)?;
        let (ssh, tunnels) = self.ssh_tunnels().ok_or(Refusal::NotConfigured)?;
        let reached = self
            .ssh_reach(ssh, operator, &request.host_id)
            .map_err(|refused| refused.write(ssh, operator))?;
        keys_of(tunnels, &reached.host_id)
    }

    /// `Ssh.PinHostKeys`: pins the host keys the host's last heartbeat reported, when the request
    /// names exactly those, and answers as `HostKeys` does after.
    ///
    /// # Errors
    /// [`Refusal`]: the call is refused and nothing is pinned.
    pub(crate) fn ssh_pin_host_keys(
        &self,
        caller: &Caller,
        request: &proto::PinHostKeysRequest,
    ) -> Result<proto::HostKeysResponse, Refusal> {
        let operator = caller.operator().map_err(Refusal::from)?;
        let (ssh, tunnels) = self.ssh_tunnels().ok_or(Refusal::NotConfigured)?;
        if request.fingerprints.len() > MAX_PINNED
            || !request.fingerprints.iter().all(|f| fingerprint_ok(f))
        {
            return Err(Refusal::InvalidRequest);
        }
        let reached = self
            .ssh_reach(ssh, operator, &request.host_id)
            .map_err(|refused| refused.write(ssh, operator))?;
        match tunnels
            .book
            .pin(operator, &reached.host_id, &request.fingerprints)
        {
            Ok(()) => {}
            Err(PinRefused::NotReported) => {
                ssh.refused(operator, &reached.host_id, Refusal::NotReported);
                return Err(Refusal::NotReported);
            }
            Err(why @ PinRefused::Unwritten(_)) => {
                tracing::error!(%why, "host keys were not pinned: the book did not write");
                return Err(Refusal::AuditUnavailable);
            }
        }
        keys_of(tunnels, &reached.host_id)
    }

    /// `Ssh.Tunnel`: opens a tunnel for the first frame of `frames` (`open`) and answers the
    /// stream whose first frame is `opened` and which the relay then feeds
    /// ([`crate::ssh_tunnel`]). `hold` keeps the caller's connection for as long as the relay
    /// runs.
    ///
    /// # Errors
    /// [`Refusal`]: the call is refused before any frame is answered, and nothing is recorded
    /// (a tunnel whose connection to the host failed is recorded and closed, `error`).
    pub(crate) async fn ssh_tunnel_open(
        &self,
        caller: &Caller,
        frames: impl Stream<Item = Result<proto::TunnelFrame, Status>> + Send + 'static,
        hold: Option<ConnectionHold>,
    ) -> Result<Outgoing, Refusal> {
        let operator = caller.operator().map_err(Refusal::from)?;
        let (ssh, tunnels) = self.ssh_tunnels().ok_or(Refusal::NotConfigured)?;
        if tunnels.draining() {
            return Err(Refusal::Draining);
        }
        let mut frames: Incoming = frames.boxed();
        let Some(Ok(proto::TunnelFrame {
            frame: Some(Frame::Open(open)),
        })) = frames.next().await
        else {
            return Err(Refusal::InvalidRequest);
        };
        let reached = match self.ssh_reach(ssh, operator, &open.host_id) {
            Ok(reached) => reached,
            Err(refused) => return Err(refused.write_off_runtime(ssh, operator).await),
        };
        let slot = tunnels.reserve().map_err(|why| match why {
            NoSlot::Draining => Refusal::Draining,
            NoSlot::Full => Refusal::TooManyTunnels,
        })?;
        let opening = Opening {
            operator: operator.to_owned(),
            host_id: reached.host_id.clone(),
            serial: open.cert_serial,
            instance: tunnels.instance().to_owned(),
            live_secs: tunnels.limits.live_secs,
            per_operator: tunnels.limits.per_operator,
        };
        // A task of its own: a caller that gives up while the book writes or the host is dialled
        // does not leave a tunnel recorded without a relay, and the slot is the task's until it is
        // done, so it is not given back before the row is closed.
        let job = tokio::spawn(Arc::clone(tunnels).open_and_dial(opening, slot));
        let dialed = job
            .await
            .unwrap_or_else(|why| Err(Failed::Open(OpenRefused::Unwritten(why.to_string()))));
        let Dialed {
            recorded,
            host,
            slot,
        } = match dialed {
            Ok(dialed) => dialed,
            Err(failed) => {
                let (why, audit) = match failed {
                    Failed::Open(OpenRefused::BadSerial) => (Refusal::BadSerial, true),
                    Failed::Open(OpenRefused::NoAddress) => (Refusal::NoAddress, true),
                    Failed::Open(OpenRefused::HostNotLive) => (Refusal::HostNotLive, true),
                    Failed::Open(OpenRefused::TooMany) => (Refusal::TooManyTunnels, true),
                    Failed::Open(OpenRefused::Unwritten(why)) => {
                        tracing::error!(%why, "a tunnel was not opened: the book did not write");
                        (Refusal::AuditUnavailable, false)
                    }
                    Failed::Dial => (Refusal::DialFailed, false),
                };
                let refused = if audit {
                    Refused::audited(why, &reached.host_id)
                } else {
                    Refused::plain(why)
                };
                return Err(refused.write_off_runtime(ssh, operator).await);
            }
        };
        let opened = recorded.keep();
        Ok(tunnels.start(
            slot,
            &opened,
            (operator, &reached.host_id),
            ssh.login_for(&reached.host),
            (host, frames),
            hold,
        ))
    }

    /// What a heartbeat of `host_id` from `from` that reported the host keys `reported` tells
    /// the book: where the host was heard from and which keys it reports. A heartbeat from a
    /// connection that has no address says nothing, and a book that cannot take it is logged: the
    /// heartbeat is answered all the same.
    pub(crate) async fn hear_ssh(
        &self,
        host_id: &str,
        from: Option<IpAddr>,
        reported: Vec<String>,
    ) {
        let (Some(tunnels), Some(from)) = (self.tunnels.as_ref(), from) else {
            return;
        };
        let book = Arc::clone(&tunnels.book);
        // A dual-stack listener names an IPv4 peer by its mapped IPv6 address.
        let address = from.to_canonical().to_string();
        let host_id = host_id.to_owned();
        match tokio::task::spawn_blocking(move || book.heard(&host_id, &address, &reported)).await {
            Ok(Ok(())) => {}
            Ok(Err(why)) => {
                tracing::warn!(%why, "a heartbeat's address and host keys were not noted")
            }
            Err(why) => tracing::warn!(%why, "noting a heartbeat's address and host keys failed"),
        }
    }
}

/// The host keys of `host_id` as the book holds them.
fn keys_of(tunnels: &Tunnels, host_id: &str) -> Result<proto::HostKeysResponse, Refusal> {
    tunnels
        .book
        .host_keys(host_id)
        .map(|keys| keys_response(&keys))
        .map_err(|why| {
            tracing::error!(%why, "the host keys could not be read");
            Refusal::StoreFailed
        })
}

#[cfg(test)]
#[path = "ssh_access_tests.rs"]
pub(crate) mod tests;
