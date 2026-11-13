//! A refusal of the operator API as a gRPC status: the one place a value of a refusal enum
//! becomes what a caller is told.
//!
//! The status code is the one the proto's comment on the value names, and the message is
//! exactly the value's token: its name without the enum's prefix, in lower case. Nothing else
//! is ever in the message, and no status carries details. A value the enum has but a caller
//! can never be refused with (`UNSPECIFIED`) is `INTERNAL`.

use hostkeeper_proto::hostkeeper::operator::v1::{
    AlertsRefusal, ApiTokensRefusal, AuditRefusal, EnrollmentRefusal, HostGroupsRefusal,
    HostsRefusal, LayoutRefusal, MdmRefusal, ReleasesRefusal, RepositoriesRefusal, RolloutsRefusal,
    ServicesRefusal, UsersRefusal,
};
use tonic::{Code, Status};

/// The status of `code` whose message is `token`.
pub(crate) fn token_status(code: Code, token: &str) -> Status {
    Status::new(code, token)
}

/// The status of a refusal whose enum names it `name` (`HOSTS_REFUSAL_UNAUTHORIZED`), with
/// `prefix` (`HOSTS_REFUSAL_`) taken off for the token.
fn refused(code: Code, name: &str, prefix: &str) -> Status {
    let token = name.trim_start_matches(prefix).to_ascii_lowercase();
    token_status(code, &token)
}

/// The status of a refusal by `Hosts`.
pub(crate) fn hosts(refusal: HostsRefusal) -> Status {
    let code = match refusal {
        HostsRefusal::Unspecified | HostsRefusal::StoreFailed => Code::Internal,
        HostsRefusal::Unauthorized => Code::Unauthenticated,
        HostsRefusal::Forbidden => Code::PermissionDenied,
        HostsRefusal::InvalidRequest => Code::InvalidArgument,
        HostsRefusal::UnknownService | HostsRefusal::UnknownHost => Code::NotFound,
        HostsRefusal::Capacity => Code::ResourceExhausted,
        HostsRefusal::AuditUnavailable => Code::Unavailable,
        HostsRefusal::DaemonTooOld | HostsRefusal::NothingToRetry => Code::FailedPrecondition,
    };
    refused(code, refusal.as_str_name(), "HOSTS_REFUSAL_")
}

/// The status of a refusal by `Releases`.
pub(crate) fn releases(refusal: ReleasesRefusal) -> Status {
    let code = match refusal {
        ReleasesRefusal::Unspecified | ReleasesRefusal::StoreFailed => Code::Internal,
        ReleasesRefusal::Unauthorized => Code::Unauthenticated,
        ReleasesRefusal::Forbidden => Code::PermissionDenied,
        ReleasesRefusal::KeysUnavailable | ReleasesRefusal::AuditUnavailable => Code::Unavailable,
        ReleasesRefusal::InvalidRequest
        | ReleasesRefusal::ConfirmMismatch
        | ReleasesRefusal::UnknownTarget
        | ReleasesRefusal::IndexDigestMismatch
        | ReleasesRefusal::IndexInvalid
        | ReleasesRefusal::InvalidWait
        | ReleasesRefusal::TargetInvalid
        | ReleasesRefusal::LimitsInHostGroup => Code::InvalidArgument,
        ReleasesRefusal::UnknownService | ReleasesRefusal::ReleaseUnknown => Code::NotFound,
        ReleasesRefusal::GenerationConflict => Code::Aborted,
        ReleasesRefusal::HeldBad
        | ReleasesRefusal::BelowMinVersion
        | ReleasesRefusal::NotHalted
        | ReleasesRefusal::NotMarkedBad
        | ReleasesRefusal::NoDigest
        | ReleasesRefusal::SelfService
        | ReleasesRefusal::AlreadyBad
        | ReleasesRefusal::AlreadyHalted
        | ReleasesRefusal::NotInChannel
        | ReleasesRefusal::ChannelDigestMismatch
        | ReleasesRefusal::TooManyWaits
        | ReleasesRefusal::ReleaseExists
        | ReleasesRefusal::RepositoryUnknown
        | ReleasesRefusal::NotInHostGroup
        | ReleasesRefusal::AmbiguousHostGroup => Code::FailedPrecondition,
    };
    refused(code, refusal.as_str_name(), "RELEASES_REFUSAL_")
}

/// The status of a refusal by `Services`.
pub(crate) fn services(refusal: ServicesRefusal) -> Status {
    let code = match refusal {
        ServicesRefusal::Unspecified | ServicesRefusal::StoreFailed => Code::Internal,
        ServicesRefusal::Unauthorized => Code::Unauthenticated,
        ServicesRefusal::Forbidden => Code::PermissionDenied,
        ServicesRefusal::InvalidRequest => Code::InvalidArgument,
        ServicesRefusal::UnknownService
        | ServicesRefusal::UnknownHost
        | ServicesRefusal::UnknownRole
        | ServicesRefusal::UnknownHostGroup
        | ServicesRefusal::RepositoryUnknown
        | ServicesRefusal::PublisherUnknown => Code::NotFound,
        ServicesRefusal::NotAssigned
        | ServicesRefusal::NotHeld
        | ServicesRefusal::BelowMinVersion
        | ServicesRefusal::PlacedByHostGroup
        | ServicesRefusal::NotInHostGroup
        | ServicesRefusal::InUse => Code::FailedPrecondition,
        ServicesRefusal::AuditUnavailable => Code::Unavailable,
    };
    refused(code, refusal.as_str_name(), "SERVICES_REFUSAL_")
}

/// The status of a refusal by `Enrollment`.
pub(crate) fn enrollment(refusal: EnrollmentRefusal) -> Status {
    let code = match refusal {
        EnrollmentRefusal::Unspecified | EnrollmentRefusal::StoreFailed => Code::Internal,
        EnrollmentRefusal::Unauthorized => Code::Unauthenticated,
        EnrollmentRefusal::Forbidden => Code::PermissionDenied,
        EnrollmentRefusal::UnknownRequest
        | EnrollmentRefusal::NoRecord
        | EnrollmentRefusal::UnknownHostGroup
        | EnrollmentRefusal::UnknownRack => Code::NotFound,
        EnrollmentRefusal::KeyMismatch
        | EnrollmentRefusal::DuplicateClaim
        | EnrollmentRefusal::NotPending
        | EnrollmentRefusal::Denied
        | EnrollmentRefusal::RecordMismatch
        | EnrollmentRefusal::RecordContested
        | EnrollmentRefusal::RecordUsed
        | EnrollmentRefusal::HostGroupDiffers
        | EnrollmentRefusal::PlacementOverlap
        | EnrollmentRefusal::PlacementOutOfRack => Code::FailedPrecondition,
        EnrollmentRefusal::HostIdTaken
        | EnrollmentRefusal::RecordExists
        | EnrollmentRefusal::RecordConflict => Code::AlreadyExists,
        EnrollmentRefusal::PendingFull | EnrollmentRefusal::Capacity => Code::ResourceExhausted,
        EnrollmentRefusal::AuditUnavailable => Code::Unavailable,
        EnrollmentRefusal::PlacementInvalid
        | EnrollmentRefusal::InvalidHostId
        | EnrollmentRefusal::InvalidRole
        | EnrollmentRefusal::InvalidService
        | EnrollmentRefusal::InvalidWindow
        | EnrollmentRefusal::InvalidRequest
        | EnrollmentRefusal::ServicesRetired => Code::InvalidArgument,
    };
    refused(code, refusal.as_str_name(), "ENROLLMENT_REFUSAL_")
}

/// The status of a refusal by `Audit`.
pub(crate) fn audit(refusal: AuditRefusal) -> Status {
    let code = match refusal {
        AuditRefusal::Unspecified | AuditRefusal::StoreFailed => Code::Internal,
        AuditRefusal::Unauthorized => Code::Unauthenticated,
        AuditRefusal::Forbidden => Code::PermissionDenied,
        AuditRefusal::InvalidRequest => Code::InvalidArgument,
    };
    refused(code, refusal.as_str_name(), "AUDIT_REFUSAL_")
}

/// The status of a refusal by `Alerts`.
pub(crate) fn alerts(refusal: AlertsRefusal) -> Status {
    let code = match refusal {
        AlertsRefusal::Unspecified | AlertsRefusal::StoreFailed => Code::Internal,
        AlertsRefusal::Unauthorized => Code::Unauthenticated,
        AlertsRefusal::Forbidden => Code::PermissionDenied,
    };
    refused(code, refusal.as_str_name(), "ALERTS_REFUSAL_")
}

/// The status of a refusal by `Rollouts`.
pub(crate) fn rollouts(refusal: RolloutsRefusal) -> Status {
    let code = match refusal {
        RolloutsRefusal::Unspecified | RolloutsRefusal::StoreFailed => Code::Internal,
        RolloutsRefusal::Unauthorized => Code::Unauthenticated,
        RolloutsRefusal::Forbidden => Code::PermissionDenied,
        RolloutsRefusal::InvalidRequest => Code::InvalidArgument,
        RolloutsRefusal::UnknownService => Code::NotFound,
        RolloutsRefusal::NoDesired
        | RolloutsRefusal::NotWaiting
        | RolloutsRefusal::AlreadyPaused
        | RolloutsRefusal::NotPaused
        | RolloutsRefusal::AmbiguousHostGroup
        | RolloutsRefusal::NotInHostGroup => Code::FailedPrecondition,
        RolloutsRefusal::AuditUnavailable => Code::Unavailable,
    };
    refused(code, refusal.as_str_name(), "ROLLOUTS_REFUSAL_")
}

/// The status of a refusal by `Mdm`.
pub(crate) fn mdm(refusal: MdmRefusal) -> Status {
    let code = match refusal {
        MdmRefusal::Unspecified => Code::Internal,
        MdmRefusal::Unauthorized => Code::Unauthenticated,
        MdmRefusal::Forbidden => Code::PermissionDenied,
        MdmRefusal::InvalidRequest => Code::InvalidArgument,
        MdmRefusal::StoreFailed => Code::Internal,
        MdmRefusal::AuditUnavailable => Code::Unavailable,
        MdmRefusal::NoPendingEnrollment => Code::NotFound,
        MdmRefusal::SecretsUnavailable
        | MdmRefusal::NameLocked
        | MdmRefusal::EnrollmentUnavailable
        | MdmRefusal::SerialNotInAbm
        | MdmRefusal::NoPendingRequest
        | MdmRefusal::AbmUnavailable
        | MdmRefusal::AbmKeyMissing
        | MdmRefusal::AbmCredentialRefused
        | MdmRefusal::AbmClockRefused => Code::FailedPrecondition,
        MdmRefusal::MdmcertUnavailable | MdmRefusal::AbmUnreachable => Code::Unavailable,
        MdmRefusal::ReplyUndecryptable
        | MdmRefusal::ReplyNotForRequest
        | MdmRefusal::CertificateInvalid
        | MdmRefusal::KeyInvalid
        | MdmRefusal::KeyMismatch
        | MdmRefusal::CertificateExpired
        | MdmRefusal::TopicChanged
        | MdmRefusal::AbmTokenUnreadable => Code::InvalidArgument,
    };
    refused(code, refusal.as_str_name(), "MDM_REFUSAL_")
}

/// The status of a refusal by `Repositories`.
pub(crate) fn repositories(refusal: RepositoriesRefusal) -> Status {
    let code = match refusal {
        RepositoriesRefusal::Unspecified | RepositoriesRefusal::StoreFailed => Code::Internal,
        RepositoriesRefusal::Unauthorized => Code::Unauthenticated,
        RepositoriesRefusal::Forbidden => Code::PermissionDenied,
        RepositoriesRefusal::InvalidRequest | RepositoriesRefusal::KeyringInvalid => {
            Code::InvalidArgument
        }
        RepositoriesRefusal::UnknownRepository => Code::NotFound,
        RepositoriesRefusal::InUse
        | RepositoriesRefusal::Conflict
        | RepositoriesRefusal::CredentialUnusable => Code::FailedPrecondition,
        RepositoriesRefusal::Capacity => Code::ResourceExhausted,
        RepositoriesRefusal::AuditUnavailable => Code::Unavailable,
    };
    refused(code, refusal.as_str_name(), "REPOSITORIES_REFUSAL_")
}

/// The status of a refusal by `HostGroups`.
pub(crate) fn host_groups(refusal: HostGroupsRefusal) -> Status {
    let code = match refusal {
        HostGroupsRefusal::Unspecified | HostGroupsRefusal::StoreFailed => Code::Internal,
        HostGroupsRefusal::Unauthorized => Code::Unauthenticated,
        HostGroupsRefusal::Forbidden => Code::PermissionDenied,
        HostGroupsRefusal::InvalidRequest
        | HostGroupsRefusal::InvalidHostGroup
        | HostGroupsRefusal::InvalidPackage
        | HostGroupsRefusal::InvalidPolicy
        | HostGroupsRefusal::TooManyPackages => Code::InvalidArgument,
        HostGroupsRefusal::AuditUnavailable => Code::Unavailable,
        HostGroupsRefusal::UnknownHostGroup | HostGroupsRefusal::UnknownHost => Code::NotFound,
        HostGroupsRefusal::HostkeeperNotListed
        | HostGroupsRefusal::GenerationMismatch
        | HostGroupsRefusal::NotEmpty
        | HostGroupsRefusal::SameHostGroup
        | HostGroupsRefusal::ReservedHostGroup => Code::FailedPrecondition,
    };
    refused(code, refusal.as_str_name(), "HOST_GROUPS_REFUSAL_")
}

/// The status of a refusal by `Users`.
pub(crate) fn users(refusal: UsersRefusal) -> Status {
    let code = match refusal {
        UsersRefusal::Unspecified | UsersRefusal::StoreFailed => Code::Internal,
        UsersRefusal::Unauthorized => Code::Unauthenticated,
        UsersRefusal::Forbidden => Code::PermissionDenied,
        UsersRefusal::InvalidRequest => Code::InvalidArgument,
        UsersRefusal::NameTaken => Code::AlreadyExists,
        UsersRefusal::UnknownUser => Code::NotFound,
        UsersRefusal::LastEnabled | UsersRefusal::OwnPassword => Code::FailedPrecondition,
        UsersRefusal::AuditUnavailable => Code::Unavailable,
    };
    refused(code, refusal.as_str_name(), "USERS_REFUSAL_")
}

/// The status of a refusal by `ApiTokens`.
pub(crate) fn api_tokens(refusal: ApiTokensRefusal) -> Status {
    let code = match refusal {
        ApiTokensRefusal::Unspecified | ApiTokensRefusal::StoreFailed => Code::Internal,
        ApiTokensRefusal::Unauthorized => Code::Unauthenticated,
        ApiTokensRefusal::Forbidden => Code::PermissionDenied,
        ApiTokensRefusal::InvalidRequest => Code::InvalidArgument,
        ApiTokensRefusal::NameTaken => Code::AlreadyExists,
        ApiTokensRefusal::UnknownToken => Code::NotFound,
        ApiTokensRefusal::AuditUnavailable => Code::Unavailable,
    };
    refused(code, refusal.as_str_name(), "API_TOKENS_REFUSAL_")
}

/// The status of a refusal by `Layout`.
pub(crate) fn layout(refusal: LayoutRefusal) -> Status {
    let code = match refusal {
        LayoutRefusal::Unspecified | LayoutRefusal::StoreFailed => Code::Internal,
        LayoutRefusal::Unauthorized => Code::Unauthenticated,
        LayoutRefusal::Forbidden => Code::PermissionDenied,
        LayoutRefusal::InvalidRequest => Code::InvalidArgument,
        LayoutRefusal::AuditUnavailable => Code::Unavailable,
        LayoutRefusal::UnknownRow
        | LayoutRefusal::UnknownRack
        | LayoutRefusal::UnknownPlacement
        | LayoutRefusal::UnknownDevice => Code::NotFound,
        LayoutRefusal::NameTaken => Code::AlreadyExists,
        LayoutRefusal::InUse
        | LayoutRefusal::Overlap
        | LayoutRefusal::OutOfRack
        | LayoutRefusal::Capacity => Code::FailedPrecondition,
    };
    refused(code, refusal.as_str_name(), "LAYOUT_REFUSAL_")
}
