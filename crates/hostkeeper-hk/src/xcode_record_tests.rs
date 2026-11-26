//! The record's JSON: what cannot be written as one is an error, and a record is indented JSON
//! ending in one newline. What each check records is asserted in tests/cli/xcode_verify.rs.

use std::collections::BTreeMap;

use super::json;
use crate::Stop;

/// Proves: a record is its JSON, indented, with one newline at its end; and a value that cannot be
/// written as JSON (a map whose keys are not strings) is a failure naming why.
#[test]
fn a_record_is_indented_json_and_what_is_not_json_is_a_failure() {
    let mut record = BTreeMap::new();
    record.insert("record", 1);
    assert_eq!(json(&record), Ok("{\n  \"record\": 1\n}\n".to_owned()));
    let mut keyed = BTreeMap::new();
    keyed.insert((1, 2), 3);
    assert_eq!(
        json(&keyed),
        Err(Stop::fail(
            "cannot write the record as JSON: key must be a string"
        ))
    );
}
