//! The host prerequisites the coordinator sends and the report the daemon sends of them
//! (`HeartbeatResponse.prerequisites`, `HeartbeatRequest.prerequisites` of `hostkeeper.agent.v1`),
//! as plain data for the loop. The loop judges none of it: [`crate::PrerequisitesSink`], the
//! daemon's, checks the set (`hostkeeper_wire::prerequisites`) and makes sure of it on the host.

/// The longest reason of a refusal, and of a failure, in bytes, that the coordinator reads.
pub const MAX_PREREQUISITE_REASON: usize = 256;

/// The longest version of a present prerequisite, in bytes, that the coordinator reads.
pub const MAX_PREREQUISITE_VERSION: usize = 64;

/// The host prerequisites of the host's group, whole, as the coordinator sent them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PushedPrerequisites {
    /// The SHA-256 of the names (`hostkeeper_wire::prerequisites::digest`), 64 lower-case hex
    /// digits, as the coordinator says it.
    pub digest: String,
    /// The names, in order; none: the group needs nothing.
    pub names: Vec<String>,
}

/// A set of prerequisites the daemon refused, and why.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrerequisitesRefusal {
    /// The refused set's digest.
    pub digest: String,
    /// Why, at most [`MAX_PREREQUISITE_REASON`] bytes.
    pub reason: String,
}

/// Where one prerequisite stands on the host (`PrerequisiteStatus`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PrerequisiteStatus {
    /// Held, and not looked at on the host yet.
    #[default]
    Pending,
    /// The host has it.
    Present,
    /// The host did not have it and the daemon is installing it.
    Installing,
    /// The last try failed; the daemon tries again from [`PrerequisiteState::next_try_unix`].
    Failed,
    /// Of another platform than the host's: nothing is done.
    NotApplicable,
    /// A name the daemon does not know: nothing is done.
    Unknown,
}

impl PrerequisiteStatus {
    /// Its wire name, `PREREQUISITE_STATUS_` and its name in capitals.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            PrerequisiteStatus::Pending => "PREREQUISITE_STATUS_PENDING",
            PrerequisiteStatus::Present => "PREREQUISITE_STATUS_PRESENT",
            PrerequisiteStatus::Installing => "PREREQUISITE_STATUS_INSTALLING",
            PrerequisiteStatus::Failed => "PREREQUISITE_STATUS_FAILED",
            PrerequisiteStatus::NotApplicable => "PREREQUISITE_STATUS_NOT_APPLICABLE",
            PrerequisiteStatus::Unknown => "PREREQUISITE_STATUS_UNKNOWN",
        }
    }
}

/// The state of one prerequisite of the held set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrerequisiteState {
    /// Its name, as the set names it.
    pub name: String,
    /// Where it stands.
    pub status: PrerequisiteStatus,
    /// [`PrerequisiteStatus::Present`]: the version the host has, at most
    /// [`MAX_PREREQUISITE_VERSION`] bytes; empty otherwise.
    pub version: String,
    /// [`PrerequisiteStatus::Failed`]: why, at most [`MAX_PREREQUISITE_REASON`] bytes; empty
    /// otherwise.
    pub reason: String,
    /// [`PrerequisiteStatus::Failed`]: how many tries in a row failed; 0 otherwise.
    pub failures: u32,
    /// [`PrerequisiteStatus::Failed`]: the Unix time from which the daemon tries again; 0
    /// otherwise.
    pub next_try_unix: u64,
    /// The Unix time the daemon last looked at the host for it; 0 before it first did.
    pub seen_unix: u64,
}

/// What the daemon holds of the host prerequisites: the set's digest, the last set refused, and
/// the state of each prerequisite of the held set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrerequisitesReport {
    /// The digest of the set the daemon holds; empty while it holds none since it started.
    pub digest: String,
    /// The last set refused, while it is the last one sent.
    pub refused: Option<PrerequisitesRefusal>,
    /// The state of each prerequisite of the held set, in its order.
    pub states: Vec<PrerequisiteState>,
}
