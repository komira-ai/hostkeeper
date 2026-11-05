//! The launchd plist of the updater's fallback timer job, written to a file and read back through
//! a property-list decoder: a well-formed plist whose values are the ones launchd will act on, an
//! escaped path included.

use std::path::Path;

use hostkeeper_steps::selfupdate::{Layout, launchd_fallback_job};
use hostkeeper_testkit::Scratch;
use plist::{Dictionary, Value};

/// A directory whose name holds every XML metacharacter.
const DIR: &str = "/opt/a&b<c>d\"e'f";
/// A label that holds one.
const LABEL: &str = "com.example.a&b";

/// `text` written to `name` in `scratch`, read back as a plist.
fn decoded(scratch: &Scratch, name: &str, text: &str) -> Value {
    let path = scratch.path().join(name);
    std::fs::write(&path, text).expect("write the plist");
    Value::from_file(&path).expect("a well-formed plist")
}

/// The outer dictionary of `value`, with its keys in the order they were written.
fn keys(value: &Value) -> Vec<String> {
    value
        .as_dictionary()
        .expect("a dictionary at the top")
        .keys()
        .cloned()
        .collect()
}

/// A dictionary of `entries`, in their order.
fn dict(entries: Vec<(&str, Value)>) -> Value {
    let dict: Dictionary = entries
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect();
    Value::Dictionary(dict)
}

/// An array of the strings `items`.
fn strings(items: &[String]) -> Value {
    Value::Array(items.iter().cloned().map(Value::String).collect())
}

/// Catches a plist that launchd would not read, or would read other than meant: the fallback
/// job's decodes to the `.fallback` label, the previous binary's verb with the directory,
/// `RunAtLoad` false and the interval given. Every path and the label come back exactly as
/// given, with their XML metacharacters, and the keys are in sorted order.
#[test]
fn the_fallback_job_decodes_to_the_values_launchd_acts_on() {
    let scratch = Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "plist-decode")
        .expect("create the scratch directory");
    let layout = Layout::new(Path::new(DIR));

    let job = decoded(
        &scratch,
        "fallback.plist",
        &launchd_fallback_job(&layout, LABEL, 60),
    );
    assert_eq!(
        job,
        dict(vec![
            ("Label", Value::String(format!("{LABEL}.fallback"))),
            (
                "ProgramArguments",
                strings(&[
                    format!("{DIR}/previous"),
                    "self-update-fallback".to_owned(),
                    "--dir".to_owned(),
                    DIR.to_owned(),
                ]),
            ),
            ("RunAtLoad", Value::Boolean(false)),
            ("StartInterval", Value::from(60u64)),
        ])
    );
    let mut sorted = keys(&job);
    sorted.sort();
    assert_eq!(keys(&job), sorted);
}
