//! The host's BMC as its daemon reads it from the BMC itself, in-band (`HeartbeatRequest.bmc` of
//! `hostkeeper.agent.v1`): the BMC's IPv4 address and its MAC, by which the coordinator reaches the
//! BMC for the host's console. The daemon reads it (`hostkeeper_daemon::bmc`); this crate only
//! carries it in every gRPC heartbeat it is given for. It is a few tens of bytes, counted toward
//! a heartbeat's bound and never dropped from it.

use serde_json::{Value, json};

/// The BMC's LAN settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BmcReport {
    /// The BMC's IPv4 address, in dotted decimal (`192.0.2.12`), as the daemon read it. The crate
    /// carries it as text: it opens no connection of its own but the link's.
    pub ip: String,
    /// The BMC's MAC, six pairs of lower-case hex digits split by `:`.
    pub mac: String,
}

impl BmcReport {
    /// The report as JSON with the wire's names: what a heartbeat's bound counts of it.
    pub fn to_json(&self) -> Value {
        json!({ "ip": self.ip, "mac": self.mac })
    }
}
