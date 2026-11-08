//! What a host's last heartbeat said of the Sigstore trusted root its daemon checks packages
//! with: the root's age and why it needs attention, kept per host ([`crate::fleet::Node`]),
//! answered by `Hosts.Get` and `Hosts.List`, and alerted on while the reason says it needs
//! attention ([`crate::alerts`], `trusted_root`).
//!
//! Only the gRPC heartbeat carries it (`HeartbeatRequest.trusted_root_age_secs` and
//! `trusted_root_reason`): the JSON route is frozen, so a heartbeat that came by it says nothing
//! of the root. Nor does one from a daemon that predates the fields, or one whose store keeps no
//! root. Each of those is unknown (the reason `UNSPECIFIED`), never a root that needs no
//! attention. A reason the coordinator does not know (a value a later daemon sends) is unknown
//! too. Like the rest of a host's last heartbeat, it is not stored: a restarted coordinator knows
//! it again at the host's next heartbeat.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;

/// What a heartbeat said of the host's trusted root. The default is what a heartbeat that says
/// nothing of it is read as: no age, and the reason unknown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TrustedRootSaid {
    /// The seconds since the root's file was last written on the host; none when the heartbeat
    /// said none (no file, or nothing said of the root).
    pub age_secs: Option<u64>,
    /// Why the root needs attention: `NONE` when it needs none, `UNSPECIFIED` when unknown.
    pub reason: agent::TrustedRootReason,
}

impl TrustedRootSaid {
    /// What `request` says of the root; a reason it names by a value this coordinator does not
    /// know is unknown.
    #[must_use]
    pub fn of(request: &agent::HeartbeatRequest) -> Self {
        Self {
            age_secs: request.trusted_root_age_secs,
            reason: request.trusted_root_reason(),
        }
    }

    /// The reason's word, when it is one that needs attention: `stale`, `missing` or
    /// `unparseable`; none for a root that needs none and for an unknown one.
    #[must_use]
    pub const fn attention(&self) -> Option<&'static str> {
        match self.reason {
            agent::TrustedRootReason::Stale => Some("stale"),
            agent::TrustedRootReason::Missing => Some("missing"),
            agent::TrustedRootReason::Unparseable => Some("unparseable"),
            agent::TrustedRootReason::None | agent::TrustedRootReason::Unspecified => None,
        }
    }
}
