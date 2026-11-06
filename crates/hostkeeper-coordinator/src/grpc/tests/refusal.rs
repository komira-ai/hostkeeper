//! The status of every refusal value of the operator API: the code the proto's comment on the
//! value names, and a message that is exactly the value's token.

use std::fmt::Debug;

use hostkeeper_proto::hostkeeper::operator::v1::{
    AuditRefusal, EnrollmentRefusal, HostsRefusal, ReleasesRefusal, ServicesRefusal,
};
use tonic::{Code, Status};

use crate::grpc::refusal;

/// Asserts that `table` names every value of the enum `E` once, in the order of its numbers,
/// and that `status` turns each into a status of the code and the message the table names with
/// no details.
fn assert_table<E>(table: &[(E, Code, &str)], status: impl Fn(E) -> Status)
where
    E: Copy + Debug + PartialEq + TryFrom<i32>,
{
    let values: Vec<E> = (0_i32..)
        .map_while(|number| E::try_from(number).ok())
        .collect();
    let named: Vec<E> = table.iter().map(|(value, _, _)| *value).collect();
    assert_eq!(values, named, "the table names every value of the enum");
    for (value, code, token) in table {
        let refused = status(*value);
        assert_eq!(
            (refused.code(), refused.message()),
            (*code, *token),
            "{value:?}"
        );
        assert!(refused.details().is_empty(), "{value:?} carries details");
    }
}

/// Proves that each value of `HostsRefusal`, `ReleasesRefusal`, `ServicesRefusal`,
/// `EnrollmentRefusal` and `AuditRefusal` becomes the status its proto comment names: the code of the comment and the
/// value's name, without its prefix and in lower case, as the whole message; and that
/// `UNSPECIFIED`, which no caller is refused with, is `INTERNAL`. A value the enum gains that
/// the table lacks fails the test.
#[test]
fn every_refusal_value_has_the_code_and_token_its_proto_comment_names() {
    use Code::{
        Aborted, AlreadyExists, FailedPrecondition, Internal, InvalidArgument, NotFound,
        PermissionDenied, ResourceExhausted, Unauthenticated, Unavailable,
    };

    assert_table(
        &[
            (HostsRefusal::Unspecified, Internal, "unspecified"),
            (HostsRefusal::Unauthorized, Unauthenticated, "unauthorized"),
            (HostsRefusal::Forbidden, PermissionDenied, "forbidden"),
            (
                HostsRefusal::InvalidRequest,
                InvalidArgument,
                "invalid_request",
            ),
            (HostsRefusal::UnknownService, NotFound, "unknown_service"),
            (HostsRefusal::UnknownHost, NotFound, "unknown_host"),
            (HostsRefusal::Capacity, ResourceExhausted, "capacity"),
            (HostsRefusal::StoreFailed, Internal, "store_failed"),
            (
                HostsRefusal::AuditUnavailable,
                Unavailable,
                "audit_unavailable",
            ),
        ],
        refusal::hosts,
    );

    assert_table(
        &[
            (ReleasesRefusal::Unspecified, Internal, "unspecified"),
            (
                ReleasesRefusal::Unauthorized,
                Unauthenticated,
                "unauthorized",
            ),
            (ReleasesRefusal::Forbidden, PermissionDenied, "forbidden"),
            (
                ReleasesRefusal::KeysUnavailable,
                Unavailable,
                "keys_unavailable",
            ),
            (
                ReleasesRefusal::InvalidRequest,
                InvalidArgument,
                "invalid_request",
            ),
            (ReleasesRefusal::UnknownService, NotFound, "unknown_service"),
            (
                ReleasesRefusal::GenerationConflict,
                Aborted,
                "generation_conflict",
            ),
            (ReleasesRefusal::HeldBad, FailedPrecondition, "held_bad"),
            (
                ReleasesRefusal::BelowMinVersion,
                FailedPrecondition,
                "below_min_version",
            ),
            (ReleasesRefusal::NotHalted, FailedPrecondition, "not_halted"),
            (
                ReleasesRefusal::NotMarkedBad,
                FailedPrecondition,
                "not_marked_bad",
            ),
            (ReleasesRefusal::StoreFailed, Internal, "store_failed"),
            (ReleasesRefusal::NoDigest, FailedPrecondition, "no_digest"),
            (
                ReleasesRefusal::SelfService,
                FailedPrecondition,
                "self_service",
            ),
            (
                ReleasesRefusal::ConfirmMismatch,
                InvalidArgument,
                "confirm_mismatch",
            ),
            (
                ReleasesRefusal::AlreadyBad,
                FailedPrecondition,
                "already_bad",
            ),
            (
                ReleasesRefusal::AlreadyHalted,
                FailedPrecondition,
                "already_halted",
            ),
            (
                ReleasesRefusal::UnknownTarget,
                InvalidArgument,
                "unknown_target",
            ),
            (
                ReleasesRefusal::AuditUnavailable,
                Unavailable,
                "audit_unavailable",
            ),
            (
                ReleasesRefusal::IndexDigestMismatch,
                InvalidArgument,
                "index_digest_mismatch",
            ),
            (
                ReleasesRefusal::IndexInvalid,
                InvalidArgument,
                "index_invalid",
            ),
        ],
        refusal::releases,
    );

    assert_table(
        &[
            (ServicesRefusal::Unspecified, Internal, "unspecified"),
            (
                ServicesRefusal::Unauthorized,
                Unauthenticated,
                "unauthorized",
            ),
            (ServicesRefusal::Forbidden, PermissionDenied, "forbidden"),
            (
                ServicesRefusal::InvalidRequest,
                InvalidArgument,
                "invalid_request",
            ),
            (ServicesRefusal::UnknownService, NotFound, "unknown_service"),
            (ServicesRefusal::UnknownHost, NotFound, "unknown_host"),
            (
                ServicesRefusal::NotAssigned,
                FailedPrecondition,
                "not_assigned",
            ),
            (ServicesRefusal::NotHeld, FailedPrecondition, "not_held"),
            (ServicesRefusal::StoreFailed, Internal, "store_failed"),
            (ServicesRefusal::UnknownRole, NotFound, "unknown_role"),
            (
                ServicesRefusal::BelowMinVersion,
                FailedPrecondition,
                "below_min_version",
            ),
            (
                ServicesRefusal::AuditUnavailable,
                Unavailable,
                "audit_unavailable",
            ),
        ],
        refusal::services,
    );

    assert_table(
        &[
            (EnrollmentRefusal::Unspecified, Internal, "unspecified"),
            (
                EnrollmentRefusal::Unauthorized,
                Unauthenticated,
                "unauthorized",
            ),
            (EnrollmentRefusal::Forbidden, PermissionDenied, "forbidden"),
            (
                EnrollmentRefusal::UnknownRequest,
                NotFound,
                "unknown_request",
            ),
            (
                EnrollmentRefusal::KeyMismatch,
                FailedPrecondition,
                "key_mismatch",
            ),
            (
                EnrollmentRefusal::HostIdTaken,
                AlreadyExists,
                "host_id_taken",
            ),
            (
                EnrollmentRefusal::PendingFull,
                ResourceExhausted,
                "pending_full",
            ),
            (
                EnrollmentRefusal::DuplicateClaim,
                FailedPrecondition,
                "duplicate_claim",
            ),
            (
                EnrollmentRefusal::InvalidHostId,
                InvalidArgument,
                "invalid_host_id",
            ),
            (
                EnrollmentRefusal::InvalidRole,
                InvalidArgument,
                "invalid_role",
            ),
            (
                EnrollmentRefusal::InvalidService,
                InvalidArgument,
                "invalid_service",
            ),
            (
                EnrollmentRefusal::InvalidRequest,
                InvalidArgument,
                "invalid_request",
            ),
            (
                EnrollmentRefusal::NotPending,
                FailedPrecondition,
                "not_pending",
            ),
            (EnrollmentRefusal::Denied, FailedPrecondition, "denied"),
            (EnrollmentRefusal::StoreFailed, Internal, "store_failed"),
            (
                EnrollmentRefusal::RecordExists,
                AlreadyExists,
                "record_exists",
            ),
            (
                EnrollmentRefusal::RecordConflict,
                AlreadyExists,
                "record_conflict",
            ),
            (EnrollmentRefusal::NoRecord, NotFound, "no_record"),
            (EnrollmentRefusal::Capacity, ResourceExhausted, "capacity"),
            (
                EnrollmentRefusal::AuditUnavailable,
                Unavailable,
                "audit_unavailable",
            ),
        ],
        refusal::enrollment,
    );

    assert_table(
        &[
            (AuditRefusal::Unspecified, Internal, "unspecified"),
            (AuditRefusal::Unauthorized, Unauthenticated, "unauthorized"),
            (AuditRefusal::Forbidden, PermissionDenied, "forbidden"),
            (
                AuditRefusal::InvalidRequest,
                InvalidArgument,
                "invalid_request",
            ),
            (AuditRefusal::StoreFailed, Internal, "store_failed"),
        ],
        refusal::audit,
    );
}

/// Proves that a status built from a token alone has that token as its whole message and the
/// code given.
#[test]
fn a_token_status_carries_the_code_and_exactly_the_token() {
    let status = refusal::token_status(Code::Unavailable, "keys_unavailable");
    assert_eq!(
        (status.code(), status.message()),
        (Code::Unavailable, "keys_unavailable")
    );
}
