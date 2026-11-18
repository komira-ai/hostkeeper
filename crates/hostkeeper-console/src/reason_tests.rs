//! The reason of a console audit line.

use super::{MAX_REASON_BYTES, audit_reason};

/// Proves: the operator's reason comes first and each detail follows as `; key=value`, in order,
/// by exact equality, and with no reason the details stand alone.
///
/// Catches: details put before the operator's reason (which a cut would then lose), a detail
/// dropped, and a leading separator.
#[test]
fn the_reason_is_the_operators_then_the_details() {
    assert_eq!(
        audit_reason("boot order", &[("session", "ab12"), ("kind", "serial")]),
        "boot order; session=ab12; kind=serial"
    );
    assert_eq!(audit_reason("boot order", &[]), "boot order");
    assert_eq!(audit_reason("", &[("cause", "busy")]), "cause=busy");
}

/// Proves: a reason over the cap is cut to exactly 256 bytes, keeping the operator's reason
/// whole and the details as far as they fit; a cut that would split a character stops before
/// it, and a reason of exactly 256 bytes is kept whole.
///
/// Catches: an audit line refused or overrun for a long reason, and a cut in the middle of a
/// multi-byte character, which would not be a string.
#[test]
fn a_reason_over_the_cap_is_cut() {
    let reason = "r".repeat(250);
    let session = "0123456789abcdef".repeat(4);
    let cut = audit_reason(&reason, &[("session", &session)]);
    assert_eq!(cut.len(), MAX_REASON_BYTES);
    assert_eq!(cut, format!("{reason}; sess"));

    let exact = "x".repeat(MAX_REASON_BYTES);
    assert_eq!(audit_reason(&exact, &[]), exact);
    assert_eq!(audit_reason(&format!("{exact}y"), &[]), exact);

    let wide = format!("{}é", "x".repeat(MAX_REASON_BYTES - 1));
    assert_eq!(wide.len(), MAX_REASON_BYTES + 1);
    assert_eq!(audit_reason(&wide, &[]), "x".repeat(MAX_REASON_BYTES - 1));
}
