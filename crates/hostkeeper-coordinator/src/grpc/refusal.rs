//! A refusal of the operator API as a gRPC status: the one place a value of a refusal enum
//! becomes what a caller is told.
//!
//! The status code is the one the proto's comment on the value names, and the message is
//! exactly the value's token: its name without the enum's prefix, in lower case. Nothing else
//! is ever in the message, and no status carries details. A value the enum has but a caller
//! can never be refused with (`UNSPECIFIED`) is `INTERNAL`.

use hostkeeper_proto::hostkeeper::operator::v1::{
    AuditRefusal, EnrollmentRefusal, HostsRefusal, ReleasesRefusal, ServicesRefusal,
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
    };
    refused(code, refusal.as_str_name(), "HOSTS_REFUSAL_")
}

/// The status of a refusal by `Releases`.
pub(crate) fn releases(refusal: ReleasesRefusal) -> Status {
    let code = match refusal {
        ReleasesRefusal::Unspecified | ReleasesRefusal::StoreFailed => Code::Internal,
        ReleasesRefusal::Unauthorized => Code::Unauthenticated,
        ReleasesRefusal::Forbidden => Code::PermissionDenied,
        ReleasesRefusal::KeysUnavailable => Code::Unavailable,
        ReleasesRefusal::InvalidRequest
        | ReleasesRefusal::ConfirmMismatch
        | ReleasesRefusal::UnknownTarget => Code::InvalidArgument,
        ReleasesRefusal::UnknownService => Code::NotFound,
        ReleasesRefusal::GenerationConflict => Code::Aborted,
        ReleasesRefusal::HeldBad
        | ReleasesRefusal::BelowMinVersion
        | ReleasesRefusal::NotHalted
        | ReleasesRefusal::NotMarkedBad
        | ReleasesRefusal::NoDigest
        | ReleasesRefusal::SelfService
        | ReleasesRefusal::AlreadyBad
        | ReleasesRefusal::AlreadyHalted => Code::FailedPrecondition,
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
        ServicesRefusal::UnknownService | ServicesRefusal::UnknownHost => Code::NotFound,
        ServicesRefusal::NotAssigned | ServicesRefusal::NotHeld => Code::FailedPrecondition,
    };
    refused(code, refusal.as_str_name(), "SERVICES_REFUSAL_")
}

/// The status of a refusal by `Enrollment`.
pub(crate) fn enrollment(refusal: EnrollmentRefusal) -> Status {
    let code = match refusal {
        EnrollmentRefusal::Unspecified | EnrollmentRefusal::StoreFailed => Code::Internal,
        EnrollmentRefusal::Unauthorized => Code::Unauthenticated,
        EnrollmentRefusal::Forbidden => Code::PermissionDenied,
        EnrollmentRefusal::UnknownRequest | EnrollmentRefusal::NoRecord => Code::NotFound,
        EnrollmentRefusal::KeyMismatch
        | EnrollmentRefusal::DuplicateClaim
        | EnrollmentRefusal::NotPending
        | EnrollmentRefusal::Denied => Code::FailedPrecondition,
        EnrollmentRefusal::HostIdTaken
        | EnrollmentRefusal::RecordExists
        | EnrollmentRefusal::RecordConflict => Code::AlreadyExists,
        EnrollmentRefusal::PendingFull | EnrollmentRefusal::Capacity => Code::ResourceExhausted,
        EnrollmentRefusal::InvalidHostId
        | EnrollmentRefusal::InvalidRole
        | EnrollmentRefusal::InvalidService
        | EnrollmentRefusal::InvalidRequest => Code::InvalidArgument,
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
