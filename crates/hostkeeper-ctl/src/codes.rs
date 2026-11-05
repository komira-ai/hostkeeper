//! The gRPC status codes by name, as the gRPC specification spells them (`INVALID_ARGUMENT`).
//! The HTTP/JSON gateway's error body names its code this way, so its client and its server read
//! and write one table.

use tonic::Code;

/// Every code, by number, with its name.
pub const ALL: [(Code, &str); 17] = [
    (Code::Ok, "OK"),
    (Code::Cancelled, "CANCELLED"),
    (Code::Unknown, "UNKNOWN"),
    (Code::InvalidArgument, "INVALID_ARGUMENT"),
    (Code::DeadlineExceeded, "DEADLINE_EXCEEDED"),
    (Code::NotFound, "NOT_FOUND"),
    (Code::AlreadyExists, "ALREADY_EXISTS"),
    (Code::PermissionDenied, "PERMISSION_DENIED"),
    (Code::ResourceExhausted, "RESOURCE_EXHAUSTED"),
    (Code::FailedPrecondition, "FAILED_PRECONDITION"),
    (Code::Aborted, "ABORTED"),
    (Code::OutOfRange, "OUT_OF_RANGE"),
    (Code::Unimplemented, "UNIMPLEMENTED"),
    (Code::Internal, "INTERNAL"),
    (Code::Unavailable, "UNAVAILABLE"),
    (Code::DataLoss, "DATA_LOSS"),
    (Code::Unauthenticated, "UNAUTHENTICATED"),
];

/// The name of a code: `INVALID_ARGUMENT`.
pub fn name(code: Code) -> &'static str {
    // `ALL` is indexed by the code's number, 0 to 16, which is every code there is.
    ALL[code as usize].1
}

/// The code a name spells; `None` for a name that is no code's.
pub fn from_name(name: &str) -> Option<Code> {
    ALL.iter().find(|(_, n)| *n == name).map(|(code, _)| *code)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Catches a row out of order (a code given another's name) and a name that does not read
    /// back as its code.
    #[test]
    fn every_code_has_its_name_and_reads_back() {
        for (number, (code, spelled)) in ALL.iter().enumerate() {
            assert_eq!(*code as usize, number, "{spelled}");
            assert_eq!(name(*code), *spelled);
            assert_eq!(from_name(spelled), Some(*code));
        }
        assert_eq!(name(Code::FailedPrecondition), "FAILED_PRECONDITION");
        assert_eq!(from_name("invalid_argument"), None);
        assert_eq!(from_name(""), None);
    }
}
