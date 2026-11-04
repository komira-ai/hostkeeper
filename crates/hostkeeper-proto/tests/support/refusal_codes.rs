//! The gRPC code of every refusal value added since the committed baseline, as
//! `(enum, token, code)`: what a server returns with each, pinned by hand so that a comment that
//! moves a value to another valid code fails the layout test. Also the codes a refusal may name,
//! and the check of documented codes against the pins.

use std::collections::BTreeMap;

/// The new values of `CoordinatorRefusal` and `ReleasesRefusal`, and every value of
/// `EnrollmentRefusal` and `AuditRefusal` but the zero.
pub const NEW_REFUSAL_CODES: [(&str, &str, &str); 26] = [
    ("CoordinatorRefusal", "pending_full", "RESOURCE_EXHAUSTED"),
    ("CoordinatorRefusal", "key_not_p256", "INVALID_ARGUMENT"),
    ("CoordinatorRefusal", "bad_csr", "INVALID_ARGUMENT"),
    ("CoordinatorRefusal", "denied", "PERMISSION_DENIED"),
    ("CoordinatorRefusal", "unknown_request", "NOT_FOUND"),
    ("CoordinatorRefusal", "bad_signature", "UNAUTHENTICATED"),
    (
        "CoordinatorRefusal",
        "renewal_not_allowed",
        "FAILED_PRECONDITION",
    ),
    ("ReleasesRefusal", "unknown_target", "INVALID_ARGUMENT"),
    ("EnrollmentRefusal", "unauthorized", "UNAUTHENTICATED"),
    ("EnrollmentRefusal", "forbidden", "PERMISSION_DENIED"),
    ("EnrollmentRefusal", "unknown_request", "NOT_FOUND"),
    ("EnrollmentRefusal", "key_mismatch", "FAILED_PRECONDITION"),
    ("EnrollmentRefusal", "host_id_taken", "ALREADY_EXISTS"),
    ("EnrollmentRefusal", "pending_full", "RESOURCE_EXHAUSTED"),
    (
        "EnrollmentRefusal",
        "duplicate_claim",
        "FAILED_PRECONDITION",
    ),
    ("EnrollmentRefusal", "invalid_host_id", "INVALID_ARGUMENT"),
    ("EnrollmentRefusal", "invalid_role", "INVALID_ARGUMENT"),
    ("EnrollmentRefusal", "invalid_service", "INVALID_ARGUMENT"),
    ("EnrollmentRefusal", "invalid_request", "INVALID_ARGUMENT"),
    ("EnrollmentRefusal", "not_pending", "FAILED_PRECONDITION"),
    ("EnrollmentRefusal", "denied", "FAILED_PRECONDITION"),
    ("EnrollmentRefusal", "store_failed", "INTERNAL"),
    ("AuditRefusal", "unauthorized", "UNAUTHENTICATED"),
    ("AuditRefusal", "forbidden", "PERMISSION_DENIED"),
    ("AuditRefusal", "invalid_request", "INVALID_ARGUMENT"),
    ("AuditRefusal", "store_failed", "INTERNAL"),
];

/// The gRPC status codes a refusal may name: every code but `OK`.
pub const GRPC_CODES: [&str; 16] = [
    "CANCELLED",
    "UNKNOWN",
    "INVALID_ARGUMENT",
    "DEADLINE_EXCEEDED",
    "NOT_FOUND",
    "ALREADY_EXISTS",
    "PERMISSION_DENIED",
    "RESOURCE_EXHAUSTED",
    "FAILED_PRECONDITION",
    "ABORTED",
    "OUT_OF_RANGE",
    "UNIMPLEMENTED",
    "INTERNAL",
    "UNAVAILABLE",
    "DATA_LOSS",
    "UNAUTHENTICATED",
];

/// The pins of `pins` that `codes` breaks: each `(enum, token)` whose documented code is not the
/// pinned one, with the code documented instead, or `None` when the value is gone.
pub fn off_pin(
    codes: &BTreeMap<(String, String), String>,
    pins: &[(&str, &str, &str)],
) -> Vec<(String, String, Option<String>)> {
    pins.iter()
        .filter_map(|&(refusal, token, code)| {
            let found = codes.get(&(refusal.to_owned(), token.to_owned()));
            (found.map(String::as_str) != Some(code))
                .then(|| (refusal.to_owned(), token.to_owned(), found.cloned()))
        })
        .collect()
}
