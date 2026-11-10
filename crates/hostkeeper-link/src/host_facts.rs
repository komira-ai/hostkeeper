//! The host's own values the coordinator sends for its facts file (`HeartbeatResponse.facts` of
//! `hostkeeper.agent.v1`, docs/design/hostgroups.md section 6.4), as plain data for the loop. The
//! loop judges none of it: [`crate::FactsSink`], the daemon's, writes the file and says which
//! generation it holds.

use std::collections::BTreeMap;

/// The host's facts, whole, as the coordinator sent them. The daemon adds its platform when it
/// writes them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PushedFacts {
    /// Raised by the coordinator at each change of the facts; the daemon reports the one it
    /// wrote (`HeartbeatRequest.facts_generation`), and the coordinator sends the facts until it
    /// does.
    pub generation: u64,
    /// The host id it was enrolled as.
    pub host_id: String,
    /// The host group it is in.
    pub host_group: String,
    /// The labels an operator set on it, by key.
    pub labels: BTreeMap<String, String>,
}
