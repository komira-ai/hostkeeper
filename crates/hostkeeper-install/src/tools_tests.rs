//! Tests of the tool paths and the helpers that read a tool's output.

use hostkeeper_launchd::LaunchctlOutput;

use super::*;

#[test]
fn each_tool_has_its_absolute_path() {
    let paths: Vec<&str> = Tool::ALL.iter().map(|t| t.path()).collect();
    assert_eq!(
        paths,
        [
            "/usr/sbin/pkgutil",
            "/usr/sbin/installer",
            "/usr/bin/lsbom",
            "/bin/launchctl"
        ]
    );
}

/// Catches: a cut test that is off by one at the runner's cap.
#[test]
fn a_stream_may_be_cut_only_from_the_runners_cap() {
    assert!(!may_be_cut(&[]));
    assert!(!may_be_cut(&vec![b'x'; OUTPUT_MAX - 1]));
    assert!(may_be_cut(&vec![b'x'; OUTPUT_MAX]));
}

fn output(stdout: &str, stderr: &str) -> ToolOutput {
    ToolOutput {
        code: Some(1),
        stdout: stdout.as_bytes().to_vec(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

#[test]
fn the_last_line_is_the_last_non_empty_line_of_standard_error() {
    let out = output("from stdout\n", "first\n  second  \n\n");
    assert_eq!(last_line(&out), "second");
}

#[test]
fn the_last_line_falls_back_to_standard_output_when_standard_error_is_blank() {
    let out = output("one\ntwo\n\n", " \n\n");
    assert_eq!(last_line(&out), "two");
}

#[test]
fn the_last_line_is_empty_when_both_streams_are_blank() {
    assert_eq!(last_line(&output("", "\n")), "");
    assert_eq!(last_line(&LaunchctlOutput::success("")), "");
}
