//! The reason tokens of the last line, and `leases_live` among them.

use hostkeeper_hook::{
    Class, LineError, MAX_REASON_LEN, REASON_LEASES_LIVE, Report, is_reason_token,
};

/// The line of a report whose reason is `reason`.
fn line(reason: &str) -> String {
    format!(r#"{{"class":"environment","reason":"{reason}","retry_after_secs":60}}"#)
}

/// Catches a deferral reason for live leases the strict reader refuses, a constant that drifts
/// from the word hooks write, or a validator that lets an empty, upper-case or over-long token
/// through: the line is read with its exact fields, and each malformed token is refused with its
/// own error.
#[test]
fn leases_live_is_an_accepted_reason_token_and_a_malformed_one_is_not() {
    let report = Report::parse_line(&line(REASON_LEASES_LIVE)).expect("the line reads");
    assert_eq!(report.class, Class::Environment);
    assert_eq!(report.reason.as_deref(), Some("leases_live"));
    assert_eq!(report.retry_after_secs, Some(60));
    assert!(report.reversible);
    assert!(!report.reversible_stated);
    assert!(!report.promotion_pending);
    assert_eq!(report.detail, None);

    assert_eq!(REASON_LEASES_LIVE, "leases_live");
    assert!(is_reason_token(REASON_LEASES_LIVE));

    for bad in ["", "LEASES_LIVE"] {
        assert_eq!(
            Report::parse_line(&line(bad)),
            Err(LineError::BadReason(bad.to_owned())),
            "{bad:?}"
        );
    }
    let long = "a".repeat(MAX_REASON_LEN + 1);
    assert_eq!(
        Report::parse_line(&line(&long)),
        Err(LineError::BadReason(long))
    );

    let longest = "a".repeat(MAX_REASON_LEN);
    let report = Report::parse_line(&line(&longest)).expect("the longest token reads");
    assert_eq!(report.reason, Some(longest));
}
