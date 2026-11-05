//! The one mapping from the errors of the enrollment store and the CA to what a caller is told:
//! every refusal token they name is a value of `CoordinatorRefusal`, every failure is an
//! internal error, and every refusal has the gRPC code its proto comment names.

use std::io;
use std::path::PathBuf;

use hostkeeper_enrollment::ca::{CsrDefect, ProfileError, SignError};
use hostkeeper_enrollment::enroll::{IssueError, PollError, PostError, RequestState, StoreError};
use hostkeeper_proto::hostkeeper::agent::v1::CoordinatorRefusal;
use tonic::Code;

use crate::enrollment::refusal::rpc_error;
use crate::enrollment::{RpcError, refusal_of};

fn store_failure() -> StoreError {
    StoreError::Io {
        path: PathBuf::from("store/requests/x.pb"),
        source: io::Error::other("a disk error"),
    }
}

/// Catches a refusal the store or the CA names whose token is no value of
/// `CoordinatorRefusal` (it would reach a caller as an internal error), a refusal mapped to
/// another value, and a failure of the coordinator told to a caller as a refusal.
#[test]
fn every_refusal_token_of_the_store_is_a_value_of_coordinator_refusal() {
    use CoordinatorRefusal as R;
    let bad_csr = [
        CsrDefect::TooLarge,
        CsrDefect::NotDer,
        CsrDefect::Signature,
        CsrDefect::Subject,
    ];
    let mut refusals: Vec<(RpcError, CoordinatorRefusal)> = bad_csr
        .into_iter()
        .map(|defect| {
            (
                rpc_error(PostError::Profile(ProfileError::BadCsr(defect))),
                R::BadCsr,
            )
        })
        .collect();
    refusals.extend([
        (
            rpc_error(PostError::Profile(ProfileError::KeyNotP256)),
            R::KeyNotP256,
        ),
        (rpc_error(ProfileError::KeyNotP256), R::KeyNotP256),
        (
            rpc_error(PostError::RenewalNotAllowed),
            R::RenewalNotAllowed,
        ),
        (rpc_error(IssueError::UnknownRequest), R::UnknownRequest),
        (rpc_error(IssueError::Denied), R::Denied),
        (rpc_error(PollError::UnknownRequest), R::UnknownRequest),
        (rpc_error(PollError::BadSignature), R::BadSignature),
        (rpc_error(PollError::BadNonce(3)), R::BadSignature),
    ]);
    for (got, want) in refusals {
        assert_eq!(got, RpcError::Refused(want));
    }

    let failures = [
        rpc_error(PostError::Store(store_failure())),
        rpc_error(IssueError::NotApproved(RequestState::Pending)),
        rpc_error(IssueError::Sign(SignError::EmptyValidity)),
        rpc_error(IssueError::Store(store_failure())),
        rpc_error(PollError::Store(store_failure())),
        rpc_error(store_failure()),
        // A refusal of the CA that is no refusal of the coordinator's: it cannot happen for a
        // host certificate, whose validity is fixed, so it is a failure.
        rpc_error(SignError::ValidityOverCap {
            requested: 2,
            cap: 1,
        }),
    ];
    assert!(
        failures.iter().all(|e| *e == RpcError::Internal),
        "{failures:?}"
    );
}

/// Catches a refusal sent with another gRPC code than its proto comment names, a token that is
/// not the value's name in lower case without its prefix, and a token sent for an error that is
/// no refusal.
#[test]
fn every_refusal_has_the_grpc_code_its_proto_comment_names() {
    use CoordinatorRefusal as R;
    let table = [
        (R::Forbidden, Code::PermissionDenied, "forbidden"),
        (
            R::InvalidHeartbeat,
            Code::InvalidArgument,
            "invalid_heartbeat",
        ),
        (R::PendingFull, Code::ResourceExhausted, "pending_full"),
        (R::KeyNotP256, Code::InvalidArgument, "key_not_p256"),
        (R::BadCsr, Code::InvalidArgument, "bad_csr"),
        (R::Denied, Code::PermissionDenied, "denied"),
        (R::UnknownRequest, Code::NotFound, "unknown_request"),
        (R::BadSignature, Code::Unauthenticated, "bad_signature"),
        (
            R::RenewalNotAllowed,
            Code::FailedPrecondition,
            "renewal_not_allowed",
        ),
    ];
    let values: Vec<CoordinatorRefusal> = (1..=32)
        .filter_map(|n| CoordinatorRefusal::try_from(n).ok())
        .collect();
    assert_eq!(
        values,
        table
            .iter()
            .map(|(refusal, _, _)| *refusal)
            .collect::<Vec<_>>(),
        "every value of the enum is in the table"
    );
    for (refusal, code, token) in table {
        let error = RpcError::Refused(refusal);
        assert_eq!((error.code(), error.token()), (code, token.to_owned()));
        let status = error.status();
        assert_eq!((status.code(), status.message()), (code, token));
        assert_eq!(refusal_of(token), Some(refusal));
    }
    assert_eq!(refusal_of("unspecified"), None);
    assert_eq!(refusal_of("key_mismatch"), None);
    let unspecified = RpcError::Refused(R::Unspecified);
    for (error, code) in [
        (RpcError::Malformed, Code::InvalidArgument),
        (RpcError::Internal, Code::Internal),
        (unspecified, Code::Internal),
    ] {
        assert_eq!(error.code(), code);
    }
    for error in [RpcError::Malformed, RpcError::Internal] {
        assert_eq!(error.token(), "");
        assert_eq!(error.status().message(), "");
    }
}
