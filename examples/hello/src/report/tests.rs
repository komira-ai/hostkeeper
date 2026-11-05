use super::{Class, Failure, Report, quote};

/// Catches a JSON string that a parser would read differently from the text.
#[test]
fn quote_escapes_what_json_needs() {
    assert_eq!(quote("plain"), "\"plain\"");
    assert_eq!(quote("a\"b\\c"), "\"a\\\"b\\\\c\"");
    assert_eq!(quote("l1\nl2\r\tx"), "\"l1\\nl2\\r\\tx\"");
    assert_eq!(quote("\u{1}\u{1f}\u{7f}"), "\"\\u0001\\u001f\\u007f\"");
    assert_eq!(quote("caf\u{e9} \u{1F600}"), "\"caf\u{e9} \u{1F600}\"");
    assert_eq!(quote(""), "\"\"");
}

/// Catches keys written out of the contract's order, or a key that is not set being written.
#[test]
fn the_line_has_its_keys_in_the_order_of_the_contract() {
    assert_eq!(
        Report::release().line(),
        "{\"class\":\"release\"}"
    );
    assert_eq!(
        Report::environment().line(),
        "{\"class\":\"environment\"}"
    );
    let full = Report {
        class: Class::Environment,
        reversible: Some(false),
        promotion_pending: Some(true),
        reason: Some("not_ready"),
        retry_after_secs: Some(2),
        detail: Some("a \"quoted\" detail".to_owned()),
    };
    assert_eq!(
        full.line(),
        "{\"class\":\"environment\",\"reversible\":false,\"promotion_pending\":true,\"reason\":\"not_ready\",\"retry_after_secs\":2,\"detail\":\"a \\\"quoted\\\" detail\"}"
    );
}

/// Catches a failure that exits 0, or one that loses its class, detail or reason.
#[test]
fn failures_exit_one_with_their_class_detail_and_reason() {
    let release = Failure::release("why");
    assert_eq!(release.code, 1);
    assert_eq!(release.report.class, Class::Release);
    assert_eq!(release.report.detail.as_deref(), Some("why"));
    assert_eq!(release.report.reason, None);
    let environment = Failure::environment("where").with_reason("bad_artifact");
    assert_eq!(environment.code, 1);
    assert_eq!(environment.report.class, Class::Environment);
    assert_eq!(environment.report.reason, Some("bad_artifact"));
    assert_eq!(Class::Release.as_str(), "release");
}
