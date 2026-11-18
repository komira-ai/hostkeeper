//! The operator keys the coordinator sends and the report the daemon sends of them
//! (`HeartbeatResponse.operator_keys`, `HeartbeatRequest.operator_keys` of `hostkeeper.agent.v1`),
//! as plain data for the loop. The loop judges none of it: [`crate::OperatorKeysSink`], the
//! daemon's, checks the set (`hostkeeper_wire::operator_keys`) and writes it.

/// The longest reason of a refusal, and the longest failure, in bytes, that the coordinator
/// reads.
pub const MAX_KEYS_REASON: usize = 256;

/// The operator keys of the host's group, whole, as the coordinator sent them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PushedKeys {
    /// The SHA-256 of the keys (`hostkeeper_wire::operator_keys::digest`), 64 lower-case hex
    /// digits, as the coordinator says it.
    pub digest: String,
    /// The keys, in order; none: take the managed block away.
    pub keys: Vec<String>,
}

/// A set of keys the daemon refused, and why.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeysRefusal {
    /// The refused set's digest.
    pub digest: String,
    /// Why, at most [`MAX_KEYS_REASON`] bytes.
    pub reason: String,
}

/// What the managed block holds, the last set refused, and why the last try failed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeysReport {
    /// The digest of the set in force as the file holds it now; empty while none the
    /// coordinator sent is.
    pub digest: String,
    /// The last set refused, while it is the last one sent.
    pub refused: Option<KeysRefusal>,
    /// Why the last try to put a set in force failed, at most [`MAX_KEYS_REASON`] bytes; empty
    /// when it did not.
    pub failed: String,
}
