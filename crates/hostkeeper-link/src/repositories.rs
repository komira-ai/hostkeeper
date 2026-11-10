//! The repository set the coordinator pushes and the report the daemon sends of it
//! (`HeartbeatResponse.repositories`, `HeartbeatRequest.repositories` of `hostkeeper.agent.v1`),
//! as plain data for the loop. The loop judges none of it: [`crate::RepositorySink`], the
//! daemon's, checks and writes the set.

use hostkeeper_proto::hostkeeper::release::v1::Repository;

/// The longest reason of a refusal, in bytes, that the coordinator reads.
pub const MAX_REFUSAL_REASON: usize = 256;

/// The repositories of a host, whole, as the coordinator sent them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PushedSet {
    /// The registry's generation when the coordinator sent the set.
    pub generation: u64,
    /// The SHA-256 of the files the set is written as, 64 lower-case hex digits.
    pub digest: String,
    /// The repositories, by name.
    pub entries: Vec<Repository>,
}

/// A set the daemon refused, and why.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepositoryRefusal {
    /// The refused set's generation.
    pub generation: u64,
    /// The refused set's digest.
    pub digest: String,
    /// Why, at most [`MAX_REFUSAL_REASON`] bytes.
    pub reason: String,
}

/// What the daemon's repository configuration holds, and the last set it refused.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepositoriesReport {
    /// The generation of the set in force; 0 while none the coordinator sent is.
    pub generation: u64,
    /// The digest of the set in force; empty with generation 0.
    pub digest: String,
    /// The last set refused, while it is the last one sent.
    pub refused: Option<RepositoryRefusal>,
}
