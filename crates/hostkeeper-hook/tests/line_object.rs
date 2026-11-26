//! The strict read of the last line takes a JSON object and nothing else: serde would read a
//! struct from the array of its fields in order too, and the contract's line is an object.

use hostkeeper_hook::{Class, LineError, Report};

/// The error of a last line that is JSON but not an object.
fn not_an_object() -> Result<Report, LineError> {
    Err(LineError::Malformed("not a JSON object".to_owned()))
}

/// Catches the strict read taking an array that lists every field of the report in order (the
/// sequence form of the struct) as a report: all six, null or of the right type, are refused.
#[test]
fn an_array_of_every_field_is_not_a_report() {
    for line in [
        "[null,null,null,null,null,null]",
        "[\"environment\",\"leases_live\",300,false,true,\"held\"]",
    ] {
        assert_eq!(Report::from_stdout(line), not_an_object(), "{line}");
        assert_eq!(Report::parse_line(line), not_an_object(), "{line}");
        // The last line decides: an object before it does not make it one.
        let stdout = format!("{{\"class\":\"release\"}}\n{line}\n");
        assert_eq!(Report::from_stdout(&stdout), not_an_object(), "{line}");
    }
}

/// Catches a strict read that refuses only arrays, or that reads another JSON value that is not
/// an object (a string, a number, a boolean, null) as a report.
#[test]
fn no_json_value_but_an_object_is_a_report() {
    for line in ["\"release\"", "0", "true", "null", "[]"] {
        assert_eq!(Report::from_stdout(line), not_an_object(), "{line}");
    }
}

/// Catches the object check refusing what it must still read: an empty object, a full one, and
/// an object whose `reversible` is null (stated and false, unlike one that leaves it out).
#[test]
fn an_object_is_still_read_field_by_field() {
    assert_eq!(Report::from_stdout("{}"), Ok(Report::default()));
    let full = "{\"class\":\"environment\",\"reason\":\"leases_live\",\"retry_after_secs\":300,\
                \"reversible\":false,\"promotion_pending\":true,\"detail\":\"held\"}";
    let report = Report::from_stdout(full).unwrap();
    assert_eq!(report.class, Class::Environment);
    assert_eq!(report.reason.as_deref(), Some("leases_live"));
    assert_eq!(report.retry_after_secs, Some(300));
    assert!(!report.reversible && report.reversible_stated);
    assert!(report.promotion_pending);
    assert_eq!(report.detail.as_deref(), Some("held"));

    let null = Report::from_stdout("{\"reversible\":null}").unwrap();
    assert!(!null.reversible && null.reversible_stated);
    let absent = Report::from_stdout("{\"class\":\"release\"}").unwrap();
    assert!(absent.reversible && !absent.reversible_stated);
    // A field of the wrong type in an object is still refused, with serde's words for it.
    assert!(matches!(
        Report::from_stdout("{\"retry_after_secs\":\"soon\"}"),
        Err(LineError::Malformed(message)) if message.starts_with("invalid type: string \"soon\"")
    ));
    // Text that is not JSON at all is refused before the object check.
    assert!(matches!(
        Report::from_stdout("{\"class\":"),
        Err(LineError::Malformed(message)) if message != "not a JSON object"
    ));
}
