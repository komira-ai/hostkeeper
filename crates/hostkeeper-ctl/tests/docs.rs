//! `docs/ctl.md` and the golden command tree, against what the command table makes now.
//!
//! The doc must equal the generated text; when it does not, the test writes the current text to
//! `ctl.md` in this test's scratch directory (under the target directory) and prints it, so it
//! can be copied over. The golden file may only grow: every line it holds (a command, a flag
//! with its value, a secret's flags, a request-file field, a global flag, the exit code of a gRPC
//! code) must still be generated, so a renamed flag or a changed exit code fails here.

use std::collections::BTreeSet;
use std::path::Path;

use hostkeeper_ctl::doc::{markdown, tree_lines};
use hostkeeper_proto::ctl::COMMANDS;

/// The fixture table `build.rs` generated, for the negative cases.
#[expect(
    dead_code,
    reason = "the generated file also holds the local-service and reserved-flag lists, which only the unit tests read"
)]
mod fixture {
    use hostkeeper_proto::ctl::{Command, EnumValue, Field, FieldKind, Input, Label};

    include!(concat!(env!("OUT_DIR"), "/fixture_commands.rs"));
}

const GOLDEN: &str = include_str!("golden/command_tree.txt");

/// The golden lines (not blank, not `#` comments) that `current` no longer has.
fn missing<'a>(golden: &'a str, current: &[String]) -> Vec<&'a str> {
    let current: BTreeSet<&str> = current.iter().map(String::as_str).collect();
    golden
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter(|l| !current.contains(l))
        .collect()
}

/// Catches `docs/ctl.md` left behind when a proto, a flag rule or an exit code changes.
#[test]
fn docs_ctl_md_is_current() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/ctl.md");
    let written = std::fs::read_to_string(path).unwrap_or_default();
    let current = markdown(COMMANDS);
    if written != current {
        // Beside this test binary, in the target directory, for the developer to copy.
        let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("ctl.md");
        std::fs::write(&out, &current).expect("write the current doc");
        panic!(
            "docs/ctl.md is not what the protos make; the current text is in {}:\n{current}",
            out.display()
        );
    }
    // The doc names what it must: every command and the exit codes.
    for command in COMMANDS {
        assert!(current.contains(&format!(
            "#### `hostkeeper ctl {} {}`",
            command.service_command, command.method_command
        )));
    }
    assert!(current.contains("| 3 | the server refused the caller"));
    // A proto change is a doc change.
    assert_ne!(markdown(fixture::COMMANDS), current);
}

/// Catches a flag renamed, a value spelling changed, a field moved from a flag to the request
/// file, a global flag renamed or an exit code changed: a golden line no longer generated.
#[test]
fn ctl_flag_names_are_stable() {
    let current = tree_lines(COMMANDS);
    let lost = missing(GOLDEN, &current);
    let report = format!(
        "lines of tests/golden/command_tree.txt no longer generated: {lost:#?}\n\
         The current lines (a golden file may only gain lines):\n{}",
        current.join("\n")
    );
    assert!(lost.is_empty(), "{report}");
    for line in [
        "command releases set-desired /hostkeeper.operator.v1.Releases/SetDesired",
        "flag releases set-desired --service STRING",
        "flag hosts get --node-id STRING",
        "exit INVALID_ARGUMENT 2",
        "global --token-file PATH",
        "global --request FILE|-",
        "global --help",
    ] {
        assert!(GOLDEN.lines().any(|l| l == line), "{line}\n{report}");
    }
    // An addition passes; a rename and a removal fail.
    let mut grown = current.clone();
    grown.push("command new rpc /a.New/Rpc".to_owned());
    assert!(missing(GOLDEN, &grown).is_empty());
    let renamed: Vec<String> = current
        .iter()
        .map(|l| l.replace("--node-id", "--node"))
        .collect();
    assert!(!missing(GOLDEN, &renamed).is_empty());
    // The request flag renamed and the help flag removed are both detected.
    let request_renamed: Vec<String> = current
        .iter()
        .map(|l| l.replace("global --request ", "global --req "))
        .collect();
    assert_eq!(
        missing(GOLDEN, &request_renamed),
        ["global --request FILE|-"]
    );
    let help_removed: Vec<String> = current
        .iter()
        .filter(|l| *l != "global --help")
        .cloned()
        .collect();
    assert_eq!(missing(GOLDEN, &help_removed), ["global --help"]);
    let fixture_lines = tree_lines(fixture::COMMANDS);
    assert!(
        missing(
            "flag widgets make --colour VALUE\nvalue widgets make --colour red\n",
            &fixture_lines
        )
        .is_empty()
    );
    assert_eq!(
        missing(
            "# a comment\n\nvalue widgets make --colour green\n",
            &fixture_lines
        ),
        ["value widgets make --colour green"]
    );
}
