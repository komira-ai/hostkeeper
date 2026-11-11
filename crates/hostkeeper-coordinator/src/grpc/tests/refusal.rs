//! The status of every refusal value of the operator API: the code the proto's comment on the
//! value names, and a message that is exactly the value's token.

use std::fmt::Debug;

use hostkeeper_proto::hostkeeper::operator::v1::{
    AlertsRefusal, ApiTokensRefusal, AuditRefusal, EnrollmentRefusal, HostGroupsRefusal,
    HostsRefusal, MdmRefusal, ReleasesRefusal, RepositoriesRefusal, RolloutsRefusal,
    ServicesRefusal, UsersRefusal,
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
/// `EnrollmentRefusal`, `AuditRefusal`, `AlertsRefusal`, `RolloutsRefusal`, `MdmRefusal`, `RepositoriesRefusal`, `HostGroupsRefusal`, `UsersRefusal` and `ApiTokensRefusal` becomes the status
/// its proto comment names: the code of the comment and the
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
            (
                HostsRefusal::DaemonTooOld,
                FailedPrecondition,
                "daemon_too_old",
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
                ReleasesRefusal::NotInChannel,
                FailedPrecondition,
                "not_in_channel",
            ),
            (
                ReleasesRefusal::ChannelDigestMismatch,
                FailedPrecondition,
                "channel_digest_mismatch",
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
            (
                ReleasesRefusal::InvalidWait,
                InvalidArgument,
                "invalid_wait",
            ),
            (
                ReleasesRefusal::TooManyWaits,
                FailedPrecondition,
                "too_many_waits",
            ),
            (
                ReleasesRefusal::TargetInvalid,
                InvalidArgument,
                "target_invalid",
            ),
            (
                ReleasesRefusal::ReleaseExists,
                FailedPrecondition,
                "release_exists",
            ),
            (ReleasesRefusal::ReleaseUnknown, NotFound, "release_unknown"),
            (
                ReleasesRefusal::RepositoryUnknown,
                FailedPrecondition,
                "repository_unknown",
            ),
            (
                ReleasesRefusal::NotInHostGroup,
                FailedPrecondition,
                "not_in_host_group",
            ),
            (
                ReleasesRefusal::AmbiguousHostGroup,
                FailedPrecondition,
                "ambiguous_host_group",
            ),
            (
                ReleasesRefusal::LimitsInHostGroup,
                InvalidArgument,
                "limits_in_host_group",
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
            (
                ServicesRefusal::PlacedByHostGroup,
                FailedPrecondition,
                "placed_by_host_group",
            ),
            (
                ServicesRefusal::UnknownHostGroup,
                NotFound,
                "unknown_host_group",
            ),
            (
                ServicesRefusal::NotInHostGroup,
                FailedPrecondition,
                "not_in_host_group",
            ),
            (
                ServicesRefusal::RepositoryUnknown,
                NotFound,
                "repository_unknown",
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
            (
                EnrollmentRefusal::RecordMismatch,
                FailedPrecondition,
                "record_mismatch",
            ),
            (
                EnrollmentRefusal::RecordContested,
                FailedPrecondition,
                "record_contested",
            ),
            (
                EnrollmentRefusal::InvalidWindow,
                InvalidArgument,
                "invalid_window",
            ),
            (
                EnrollmentRefusal::RecordUsed,
                FailedPrecondition,
                "record_used",
            ),
            (
                EnrollmentRefusal::ServicesRetired,
                InvalidArgument,
                "services_retired",
            ),
            (
                EnrollmentRefusal::HostGroupDiffers,
                FailedPrecondition,
                "host_group_differs",
            ),
            (
                EnrollmentRefusal::UnknownHostGroup,
                NotFound,
                "unknown_host_group",
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

    assert_table(
        &[
            (AlertsRefusal::Unspecified, Internal, "unspecified"),
            (AlertsRefusal::Unauthorized, Unauthenticated, "unauthorized"),
            (AlertsRefusal::Forbidden, PermissionDenied, "forbidden"),
            (AlertsRefusal::StoreFailed, Internal, "store_failed"),
        ],
        refusal::alerts,
    );

    assert_table(
        &[
            (RolloutsRefusal::Unspecified, Internal, "unspecified"),
            (
                RolloutsRefusal::Unauthorized,
                Unauthenticated,
                "unauthorized",
            ),
            (RolloutsRefusal::Forbidden, PermissionDenied, "forbidden"),
            (
                RolloutsRefusal::InvalidRequest,
                InvalidArgument,
                "invalid_request",
            ),
            (RolloutsRefusal::UnknownService, NotFound, "unknown_service"),
            (RolloutsRefusal::NoDesired, FailedPrecondition, "no_desired"),
            (
                RolloutsRefusal::NotWaiting,
                FailedPrecondition,
                "not_waiting",
            ),
            (
                RolloutsRefusal::AlreadyPaused,
                FailedPrecondition,
                "already_paused",
            ),
            (RolloutsRefusal::NotPaused, FailedPrecondition, "not_paused"),
            (RolloutsRefusal::StoreFailed, Internal, "store_failed"),
            (
                RolloutsRefusal::AuditUnavailable,
                Unavailable,
                "audit_unavailable",
            ),
            (
                RolloutsRefusal::AmbiguousHostGroup,
                FailedPrecondition,
                "ambiguous_host_group",
            ),
            (
                RolloutsRefusal::NotInHostGroup,
                FailedPrecondition,
                "not_in_host_group",
            ),
        ],
        refusal::rollouts,
    );

    assert_table(
        &[
            (MdmRefusal::Unspecified, Internal, "unspecified"),
            (MdmRefusal::Unauthorized, Unauthenticated, "unauthorized"),
            (MdmRefusal::Forbidden, PermissionDenied, "forbidden"),
            (
                MdmRefusal::InvalidRequest,
                InvalidArgument,
                "invalid_request",
            ),
            (MdmRefusal::StoreFailed, Internal, "store_failed"),
            (
                MdmRefusal::AuditUnavailable,
                Unavailable,
                "audit_unavailable",
            ),
            (
                MdmRefusal::SecretsUnavailable,
                FailedPrecondition,
                "secrets_unavailable",
            ),
            (
                MdmRefusal::MdmcertUnavailable,
                Unavailable,
                "mdmcert_unavailable",
            ),
            (
                MdmRefusal::NoPendingRequest,
                FailedPrecondition,
                "no_pending_request",
            ),
            (
                MdmRefusal::ReplyUndecryptable,
                InvalidArgument,
                "reply_undecryptable",
            ),
            (
                MdmRefusal::ReplyNotForRequest,
                InvalidArgument,
                "reply_not_for_request",
            ),
            (
                MdmRefusal::CertificateInvalid,
                InvalidArgument,
                "certificate_invalid",
            ),
            (MdmRefusal::KeyInvalid, InvalidArgument, "key_invalid"),
            (MdmRefusal::KeyMismatch, InvalidArgument, "key_mismatch"),
            (
                MdmRefusal::CertificateExpired,
                InvalidArgument,
                "certificate_expired",
            ),
            (MdmRefusal::TopicChanged, InvalidArgument, "topic_changed"),
        ],
        refusal::mdm,
    );

    assert_table(
        &[
            (RepositoriesRefusal::Unspecified, Internal, "unspecified"),
            (
                RepositoriesRefusal::Unauthorized,
                Unauthenticated,
                "unauthorized",
            ),
            (
                RepositoriesRefusal::Forbidden,
                PermissionDenied,
                "forbidden",
            ),
            (
                RepositoriesRefusal::InvalidRequest,
                InvalidArgument,
                "invalid_request",
            ),
            (
                RepositoriesRefusal::KeyringInvalid,
                InvalidArgument,
                "keyring_invalid",
            ),
            (
                RepositoriesRefusal::UnknownRepository,
                NotFound,
                "unknown_repository",
            ),
            (RepositoriesRefusal::InUse, FailedPrecondition, "in_use"),
            (
                RepositoriesRefusal::Conflict,
                FailedPrecondition,
                "conflict",
            ),
            (RepositoriesRefusal::Capacity, ResourceExhausted, "capacity"),
            (RepositoriesRefusal::StoreFailed, Internal, "store_failed"),
            (
                RepositoriesRefusal::AuditUnavailable,
                Unavailable,
                "audit_unavailable",
            ),
            (
                RepositoriesRefusal::CredentialUnusable,
                FailedPrecondition,
                "credential_unusable",
            ),
        ],
        refusal::repositories,
    );

    assert_table(
        &[
            (HostGroupsRefusal::Unspecified, Internal, "unspecified"),
            (
                HostGroupsRefusal::Unauthorized,
                Unauthenticated,
                "unauthorized",
            ),
            (HostGroupsRefusal::Forbidden, PermissionDenied, "forbidden"),
            (
                HostGroupsRefusal::InvalidRequest,
                InvalidArgument,
                "invalid_request",
            ),
            (HostGroupsRefusal::StoreFailed, Internal, "store_failed"),
            (
                HostGroupsRefusal::AuditUnavailable,
                Unavailable,
                "audit_unavailable",
            ),
            (
                HostGroupsRefusal::UnknownHostGroup,
                NotFound,
                "unknown_host_group",
            ),
            (
                HostGroupsRefusal::InvalidHostGroup,
                InvalidArgument,
                "invalid_host_group",
            ),
            (
                HostGroupsRefusal::InvalidPackage,
                InvalidArgument,
                "invalid_package",
            ),
            (
                HostGroupsRefusal::InvalidPolicy,
                InvalidArgument,
                "invalid_policy",
            ),
            (
                HostGroupsRefusal::HostkeeperNotListed,
                FailedPrecondition,
                "hostkeeper_not_listed",
            ),
            (
                HostGroupsRefusal::TooManyPackages,
                InvalidArgument,
                "too_many_packages",
            ),
            (
                HostGroupsRefusal::GenerationMismatch,
                FailedPrecondition,
                "generation_mismatch",
            ),
            (HostGroupsRefusal::NotEmpty, FailedPrecondition, "not_empty"),
            (HostGroupsRefusal::UnknownHost, NotFound, "unknown_host"),
            (
                HostGroupsRefusal::SameHostGroup,
                FailedPrecondition,
                "same_host_group",
            ),
        ],
        refusal::host_groups,
    );

    assert_table(
        &[
            (UsersRefusal::Unspecified, Internal, "unspecified"),
            (UsersRefusal::Unauthorized, Unauthenticated, "unauthorized"),
            (UsersRefusal::Forbidden, PermissionDenied, "forbidden"),
            (
                UsersRefusal::InvalidRequest,
                InvalidArgument,
                "invalid_request",
            ),
            (UsersRefusal::NameTaken, AlreadyExists, "name_taken"),
            (UsersRefusal::UnknownUser, NotFound, "unknown_user"),
            (
                UsersRefusal::LastEnabled,
                FailedPrecondition,
                "last_enabled",
            ),
            (
                UsersRefusal::OwnPassword,
                FailedPrecondition,
                "own_password",
            ),
            (UsersRefusal::StoreFailed, Internal, "store_failed"),
            (
                UsersRefusal::AuditUnavailable,
                Unavailable,
                "audit_unavailable",
            ),
        ],
        refusal::users,
    );

    assert_table(
        &[
            (ApiTokensRefusal::Unspecified, Internal, "unspecified"),
            (
                ApiTokensRefusal::Unauthorized,
                Unauthenticated,
                "unauthorized",
            ),
            (ApiTokensRefusal::Forbidden, PermissionDenied, "forbidden"),
            (
                ApiTokensRefusal::InvalidRequest,
                InvalidArgument,
                "invalid_request",
            ),
            (ApiTokensRefusal::NameTaken, AlreadyExists, "name_taken"),
            (ApiTokensRefusal::UnknownToken, NotFound, "unknown_token"),
            (ApiTokensRefusal::StoreFailed, Internal, "store_failed"),
            (
                ApiTokensRefusal::AuditUnavailable,
                Unavailable,
                "audit_unavailable",
            ),
        ],
        refusal::api_tokens,
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
