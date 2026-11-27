use super::*;

#[test]
fn the_properties_are_set_only_by_a_value_that_is_not_blank() {
    // Catches: the key's absence (`{}`, what buck2 prints for an unset key), an empty or blank
    // value, another key, a value that is not text, or output that is not JSON read as set.
    assert!(has_value(
        r#"{"buildrules_re.linux_x86_64_properties":"OSFamily=Linux,ISA=x86-64"}"#
    ));
    for json in [
        "{}",
        r#"{"buildrules_re.linux_x86_64_properties":""}"#,
        r#"{"buildrules_re.linux_x86_64_properties":"  "}"#,
        r#"{"buildrules_re.linux_aarch64_properties":"OSFamily=Linux"}"#,
        r#"{"buildrules_re.linux_x86_64_properties":["OSFamily=Linux"]}"#,
        "not json",
        "",
    ] {
        assert!(!has_value(json), "{json}");
    }
}

#[test]
fn every_error_says_what_is_wrong() {
    // Catches: an error whose words do not name the program, the key, the status or what the
    // command printed.
    let cases = [
        (
            Error::Start {
                program: PathBuf::from("/x/buck2"),
                why: "No such file or directory (os error 2)".to_owned(),
            },
            "cannot run \"/x/buck2\": No such file or directory (os error 2)",
        ),
        (
            Error::Audit {
                status: "exit status: 3".to_owned(),
                stderr: "no .buckconfig".to_owned(),
            },
            "`buck2 audit config` failed (exit status: 3): no .buckconfig",
        ),
        (
            Error::NoWorkers,
            "buildrules_re.linux_x86_64_properties is empty: farm-connect wrote no [komira_re] \
             linux_x86_64_properties, so buck2 has no farm to build on",
        ),
        (
            Error::Table(table::Error::Sync {
                status: "exit status: 1".to_owned(),
            }),
            "the generated build table is stale, and `buildrules sync` failed (exit status: 1); \
             its output is above",
        ),
    ];
    for (error, words) in cases {
        assert_eq!(error.to_string(), words);
    }
}

#[test]
fn every_line_of_a_command_reaches_the_log_and_a_last_line_without_a_newline_too() {
    // Catches: a line dropped, two lines run together because the buffer was not cleared, or the
    // last line lost when the command did not end it with a newline.
    let mut log = Vec::new();
    tee(&b"one\ntwo\nthree"[..], &mut log);
    assert_eq!(log, b"one\ntwo\nthree");
}
