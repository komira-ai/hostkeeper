use serde_json::json;

use super::*;
use crate::desired::Desired;

fn version(text: &str) -> Version {
    Version::parse(text).expect("a version")
}

fn min_up(storage: u32) -> RoleLimits {
    let text = format!(r#"{{"version":"1.0","min_up":{{"storage":{storage}}}}}"#);
    Desired::parse(text.as_bytes()).expect("a state").min_up
}

/// A row of `desired` for `service`: `0001`'s five columns, then `columns`.
fn row(service: &str, columns: [Val; 4]) -> Rows {
    let mut values = vec![
        Val::Text("1.0".to_owned()),
        Val::Json(json!({})),
        Val::Json(json!({})),
        Val::Null,
        Val::Null,
    ];
    values.extend(columns);
    Rows::from([(vec![KeyVal::Text(service.to_owned())], values)])
}

/// The error a load of `columns` as the row of `web` answers.
fn refused(columns: [Val; 4]) -> String {
    records_of(&row("web", columns))
        .expect_err("the row is refused")
        .to_string()
}

/// Proves: a record goes to its four columns and back as it was: the generation, the write id,
/// and the replaced version with its minimums as a plain map; no record is written as the
/// default (generation 0, the rest `NULL`); and a row at the default, as every row written before `0003`
/// reads once it has run, is read as no record.
///
/// Catches: a record that does not come back from the database as it was committed, or a row
/// written before `0003` read as a write.
#[test]
fn a_record_goes_to_its_columns_and_back() {
    let record = WriteRecord {
        generation: 7,
        replaced: Some(Replaced {
            version: version("1.0"),
            min_up: min_up(2),
        }),
        write_id: Some(WriteId::parse("ci-42").expect("an id")),
    };
    let written = columns(Some(&record)).expect("the columns");
    assert_eq!(
        written,
        [
            Val::Int(7),
            Val::Text("ci-42".to_owned()),
            Val::Text("1.0".to_owned()),
            Val::Json(json!({"storage": 2})),
        ]
    );
    let read = records_of(&row("web", written)).expect("the row reads");
    assert_eq!(read, BTreeMap::from([("web".to_owned(), record)]));

    let none = columns(None).expect("the default");
    assert_eq!(none, [Val::Int(0), Val::Null, Val::Null, Val::Null]);
    assert_eq!(
        records_of(&row("web", none)).expect("reads"),
        BTreeMap::new()
    );
    let only_generation = [Val::Int(3), Val::Null, Val::Null, Val::Null];
    let read = records_of(&row("web", only_generation)).expect("reads");
    assert_eq!(read["web"].generation, 3);
}

/// Proves: a generation above `i64::MAX` is refused at the write, with the text that says so,
/// as every counter of the store is.
///
/// Catches: a generation that wraps to a negative number in the database.
#[test]
fn a_generation_above_the_largest_counter_is_refused_at_the_write() {
    let record = WriteRecord {
        generation: u64::MAX,
        ..WriteRecord::default()
    };
    let error = columns(Some(&record)).expect_err("too large");
    assert_eq!(
        error.to_string(),
        "database:counter: 18446744073709551615 is above the largest counter the database keeps"
    );
}

/// Proves: a load refuses a row whose record does not parse, naming the service and what is
/// wrong, each by its exact text: a negative generation, none, a write id that breaks its rule
/// or is not text, a replaced version without its minimums (and the reverse), a replaced
/// version that is not a version, minimums that are not a map of counts, that name a role no
/// role name can be, or that hold a count over the largest.
///
/// Catches: a start that runs on a record it cannot give back to a writer as it was written.
#[test]
fn a_record_that_does_not_parse_refuses_the_load() {
    let prefix = "database:desired: the desired state is malformed: the row web: ";
    let ok_version = || Val::Text("1.0".to_owned());
    let ok_min_up = || Val::Json(json!({"storage": 1}));
    let cases: [([Val; 4], &str); 10] = [
        (
            [Val::Int(-1), Val::Null, Val::Null, Val::Null],
            "a negative generation",
        ),
        (
            [Val::Null, Val::Null, Val::Null, Val::Null],
            "no generation",
        ),
        (
            [
                Val::Int(1),
                Val::Text("a b".to_owned()),
                Val::Null,
                Val::Null,
            ],
            "its write id: a write id is 1 to 128 visible ASCII characters",
        ),
        (
            [Val::Int(1), Val::Int(5), Val::Null, Val::Null],
            "a write id that is not text",
        ),
        (
            [Val::Int(1), Val::Null, ok_version(), Val::Null],
            "a replaced version without its minimums, or minimums alone",
        ),
        (
            [Val::Int(1), Val::Null, Val::Null, ok_min_up()],
            "a replaced version without its minimums, or minimums alone",
        ),
        (
            [
                Val::Int(1),
                Val::Null,
                Val::Text("v1".to_owned()),
                ok_min_up(),
            ],
            "its replaced version: the version must start with a digit",
        ),
        (
            [
                Val::Int(1),
                Val::Null,
                ok_version(),
                Val::Json(json!(["storage"])),
            ],
            "its replaced minimums: invalid type: sequence, expected a map",
        ),
        (
            [
                Val::Int(1),
                Val::Null,
                ok_version(),
                Val::Json(json!({"Bad Role": 1})),
            ],
            "its replaced minimums: \"Bad Role\" is not a role name",
        ),
        (
            [
                Val::Int(1),
                Val::Null,
                ok_version(),
                Val::Json(json!({"storage": 10_001})),
            ],
            "its replaced minimums: a count is over 10000",
        ),
    ];
    for (columns, why) in cases {
        assert_eq!(refused(columns), format!("{prefix}{why}"));
    }
}

/// Proves: a commit's rows of a desired state whose record holds a generation above `i64::MAX`
/// are refused whole, with the counter's text, before any row is written.
///
/// Catches: a state whose rows are cut without its record's columns when they cannot be written.
#[test]
fn the_rows_of_a_state_whose_generation_the_database_cannot_hold_are_refused() {
    let mut held = crate::desired::Held::default();
    held.desired.insert(
        "web".to_owned(),
        Desired::parse(br#"{"version":"1.0","min_up":{}}"#).expect("a state"),
    );
    held.records.insert(
        "web".to_owned(),
        WriteRecord {
            generation: u64::MAX,
            ..WriteRecord::default()
        },
    );
    let error = super::super::state::desired_rows(&held).expect_err("refused");
    assert_eq!(
        error.to_string(),
        "database:counter: 18446744073709551615 is above the largest counter the database keeps"
    );
}
