//! The exit codes of `hostkeeper ctl`.
//!
//! | code | when |
//! |---|---|
//! | 0 | the call succeeded |
//! | 1 | anything else: the transport, `INTERNAL`, `UNAVAILABLE`, a malformed reply |
//! | 2 | a usage error, or the server refused the request as `INVALID_ARGUMENT` or `FAILED_PRECONDITION` |
//! | 3 | the server refused the caller: `UNAUTHENTICATED` or `PERMISSION_DENIED` |

use tonic::Code;

/// The call succeeded.
pub const SUCCESS: u8 = 0;
/// Anything else.
pub const FAILURE: u8 = 1;
/// A usage error, or a request the server refused.
pub const USAGE: u8 = 2;
/// A caller the server refused.
pub const DENIED: u8 = 3;

/// Each exit code with what it means, for `--help` and `docs/ctl.md`.
pub const MEANINGS: [(u8, &str); 4] = [
    (SUCCESS, "the call succeeded"),
    (
        FAILURE,
        "anything else: the transport, INTERNAL, UNAVAILABLE, a malformed reply",
    ),
    (
        USAGE,
        "a usage error, or the server refused the request: INVALID_ARGUMENT or FAILED_PRECONDITION",
    ),
    (
        DENIED,
        "the server refused the caller: UNAUTHENTICATED or PERMISSION_DENIED",
    ),
];

/// The exit code of a call that ended with `code`.
pub fn code(code: Code) -> u8 {
    match code {
        Code::Ok => SUCCESS,
        Code::InvalidArgument | Code::FailedPrecondition => USAGE,
        Code::Unauthenticated | Code::PermissionDenied => DENIED,
        Code::Cancelled
        | Code::Unknown
        | Code::DeadlineExceeded
        | Code::NotFound
        | Code::AlreadyExists
        | Code::ResourceExhausted
        | Code::Aborted
        | Code::OutOfRange
        | Code::Unimplemented
        | Code::Internal
        | Code::Unavailable
        | Code::DataLoss => FAILURE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codes::ALL;

    /// Catches any code mapped to the wrong exit: a refusal of the request read as a failure, a
    /// refused caller read as a refused request, or a failure read as success.
    #[test]
    fn exit_code_for_every_grpc_code() {
        let expected: [(Code, u8); 17] = [
            (Code::Ok, 0),
            (Code::Cancelled, 1),
            (Code::Unknown, 1),
            (Code::InvalidArgument, 2),
            (Code::DeadlineExceeded, 1),
            (Code::NotFound, 1),
            (Code::AlreadyExists, 1),
            (Code::PermissionDenied, 3),
            (Code::ResourceExhausted, 1),
            (Code::FailedPrecondition, 2),
            (Code::Aborted, 1),
            (Code::OutOfRange, 1),
            (Code::Unimplemented, 1),
            (Code::Internal, 1),
            (Code::Unavailable, 1),
            (Code::DataLoss, 1),
            (Code::Unauthenticated, 3),
        ];
        for (grpc, exit) in expected {
            assert_eq!(code(grpc), exit, "{grpc:?}");
        }
        // The table covers every code there is.
        let covered: Vec<Code> = expected.iter().map(|(c, _)| *c).collect();
        let all: Vec<Code> = ALL.iter().map(|(c, _)| *c).collect();
        assert_eq!(covered, all);
        assert_eq!(MEANINGS.map(|(c, _)| c), [0, 1, 2, 3]);
    }
}
