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
fn the_summary_is_the_last_commands_line_and_its_local_count() {
    // Catches: a summary read from an earlier line, a timestamp prefix kept in the line, or the
    // count of another field (cached, remote) read as the local one.
    let log = "\
[t] Commands: 9 (cached: 0, remote: 0, local: 9)
[t] Cache hits: 100%
[t] Commands: 3356 (cached: 3356, remote: 0, local: 0)
BUILD SUCCEEDED
";
    assert_eq!(
        summary(log),
        Some(("Commands: 3356 (cached: 3356, remote: 0, local: 0)", 0))
    );
    assert_eq!(
        summary("Commands: 2 (cached: 0, remote: 1, local: 12)"),
        Some(("Commands: 2 (cached: 0, remote: 1, local: 12)", 12))
    );
}

#[test]
fn a_log_with_no_readable_summary_has_none() {
    // Catches: a build log with no summary, or one whose local count is missing or not a number,
    // read as `local: 0`.
    for log in [
        "",
        "BUILD SUCCEEDED\n",
        "Commands: 2 (cached: 2, remote: 0)\n",
        "Commands: 2 (cached: 2, remote: 0, local: none)\n",
    ] {
        assert_eq!(summary(log), None, "{log}");
    }
}

#[test]
fn every_error_says_what_is_wrong() {
    // Catches: an error whose words do not name the key, the target, the status or the line.
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
            Error::Build {
                target: "//:ci".to_owned(),
                status: "exit status: 3".to_owned(),
            },
            "`buck2 build //:ci` failed (exit status: 3)",
        ),
        (
            Error::CutTwice {
                target: "//:ci".to_owned(),
                status: "exit status: 3".to_owned(),
            },
            "`buck2 build //:ci` failed (exit status: 3) on its second run too, every failure \
             again a cut connection to the farm: the farm or the network to it is down; run the \
             job again once the farm answers",
        ),
        (
            Error::Table(table::Error::Sync {
                status: "exit status: 1".to_owned(),
            }),
            "the generated build table is stale, and `buildrules sync` failed (exit status: 1); \
             its output is above",
        ),
        (
            Error::NoSummary,
            "the build printed no summary line (`Commands: N (cached: N, remote: N, local: N)`)",
        ),
        (
            Error::Local("Commands: 2 (cached: 0, remote: 0, local: 2)".to_owned()),
            "`Commands: 2 (cached: 0, remote: 0, local: 2)`: actions ran on this machine, not on \
             the farm (local must be 0)",
        ),
    ];
    for (error, words) in cases {
        assert_eq!(error.to_string(), words);
    }
}

#[test]
fn the_rerun_line_names_the_target_and_the_cuts() {
    // Catches: a line in the log and the job summary that does not say what ran again or why.
    assert_eq!(
        rerun_line("//:ci", 39),
        "xtask: every failure of `buck2 build //:ci` was a cut connection to the farm (39 \
         cut messages); running it once more\n"
    );
}

#[test]
fn only_a_buck2_that_ended_with_a_status_of_its_own_may_run_again() {
    // Catches: a buck2 ended by a signal (killed by the runner, or out of memory) run again
    // because what it printed before it died reads as a cut.
    use std::os::unix::process::ExitStatusExt as _;
    let cut_log = include_str!("cuts/build_cut.log");
    assert_eq!(cut(ExitStatus::from_raw(3 << 8), cut_log), Some(2));
    assert_eq!(cut(ExitStatus::from_raw(6), cut_log), None);
    assert_eq!(cut(ExitStatus::from_raw(3 << 8), "BUILD FAILED\n"), None);
}
