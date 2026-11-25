//! Why the desired state was not set ([`SetError`]) and why a CI token was not accepted
//! ([`CiRefused`]): the refusals of the service's calls that take CI's token. They are
//! re-exported by [`crate::service`], where they were made.

use crate::desired::{DesiredError, StoreError};
use crate::ops::OpsError;

/// Why the desired state was not set. Every refusal is decided before anything changes: a
/// refused set leaves the state file, the ops file and memory as they were.
#[derive(Debug, thiserror::Error)]
pub enum SetError {
    /// The caller has no valid CI token: no bearer, or the verifier refused it (the name is
    /// the verifier's short reason; the operator token is refused like any bad token).
    #[error("unauthorized: {0}")]
    Unauthorized(&'static str),
    /// The token's issuer keys could not be fetched.
    #[error("the issuer's keys are unavailable")]
    Unavailable,
    /// The body is no desired state.
    #[error(transparent)]
    Invalid(#[from] DesiredError),
    /// The state file could not be written.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The ops file could not be written: for a set of a service, the desired state is set, and
    /// the halt the updater raised, which the set lifts, stands. Setting the same state again is
    /// safe and tries the lift again. For a set in host groups, whose desired state and audit
    /// entries are one commit, nothing is set; [`crate::ops::OpsError::DesiredChanged`] there
    /// means the desired state changed in the store each time the set was decided.
    #[error(transparent)]
    Ops(#[from] OpsError),
    /// The allow-list entry that admitted the token does not name this service, or names
    /// `host_groups` that exclude where the set would deploy.
    #[error("the token may not set this service")]
    Forbidden,
    /// The version is below the service's `min_version`.
    #[error("below_min_version: the version is below the service's min_version")]
    BelowMinVersion,
    /// The service's `version_scheme` cannot order the version.
    #[error("unparseable_version: the service's version scheme cannot read the version")]
    UnparseableVersion,
    /// The version is marked bad: only the operator's unmark or a force lifts the mark.
    #[error("bad_version: the version is marked bad")]
    BadVersion,
    /// CI set the version before with another package digest: a changed artifact for a version.
    #[error("digest_conflict: the version was set before with another package digest")]
    DigestConflict,
    /// The set's targets break a rule of the release ([`hostkeeper_release::target`]).
    #[error("target_invalid: {0}")]
    TargetInvalid(hostkeeper_release::TargetError),
    /// The version was set before as another release ([`crate::releases`]).
    #[error("release_exists: the version was set before as another release")]
    ReleaseExists,
    /// A repository entry names a repository the registry does not hold ([`crate::repositories`]).
    #[error("repository_unknown: a repository entry names a repository the registry lacks")]
    RepositoryUnknown,
    /// The set names a host group that does not exist or does not list the package, or names none
    /// and no group lists it.
    #[error("not_in_host_group: no such group lists the package")]
    NotInHostGroup,
    /// The set names `if_generation` and no group, and more than one group lists the package.
    #[error("ambiguous_host_group: more than one group lists the package")]
    AmbiguousHostGroup,
    /// The set carries `max_updating` or `min_up`, which are a group's policy.
    #[error("limits_in_host_group: the limits are the group's policy")]
    LimitsInHostGroup,
}

impl SetError {
    /// The closed reason token of a refusal of the version itself (`below_min_version`,
    /// `unparseable_version`, `bad_version`, `digest_conflict`, `release_exists`,
    /// `repository_unknown`), of its release index (`index_invalid`, `index_digest_mismatch`), of
    /// its targets (`target_invalid`) or of its host group (`not_in_host_group`,
    /// `ambiguous_host_group`, `limits_in_host_group`), which the answer says; `None` for every
    /// other error.
    #[must_use]
    pub const fn reason(&self) -> Option<&'static str> {
        match self {
            Self::Invalid(why) => why.reason(),
            Self::BelowMinVersion => Some("below_min_version"),
            Self::UnparseableVersion => Some("unparseable_version"),
            Self::BadVersion => Some("bad_version"),
            Self::DigestConflict => Some("digest_conflict"),
            Self::TargetInvalid(_) => Some("target_invalid"),
            Self::ReleaseExists => Some("release_exists"),
            Self::RepositoryUnknown => Some("repository_unknown"),
            Self::NotInHostGroup => Some("not_in_host_group"),
            Self::AmbiguousHostGroup => Some("ambiguous_host_group"),
            Self::LimitsInHostGroup => Some("limits_in_host_group"),
            Self::Unauthorized(_)
            | Self::Unavailable
            | Self::Store(_)
            | Self::Ops(_)
            | Self::Forbidden => None,
        }
    }
}

/// Why a CI token was not accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CiRefused {
    /// No bearer, or the verifier refused it (the name is the verifier's short reason; the
    /// operator token is refused like any bad token).
    #[error("unauthorized: {0}")]
    Unauthorized(&'static str),
    /// The token's issuer keys could not be fetched.
    #[error("the issuer's keys are unavailable")]
    Unavailable,
}

impl From<CiRefused> for SetError {
    fn from(refused: CiRefused) -> Self {
        match refused {
            CiRefused::Unauthorized(why) => Self::Unauthorized(why),
            CiRefused::Unavailable => Self::Unavailable,
        }
    }
}
