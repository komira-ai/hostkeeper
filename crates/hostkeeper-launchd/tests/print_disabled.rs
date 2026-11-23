//! The reading of `launchctl print-disabled system`, launchd's override database of the system
//! domain.

use std::collections::BTreeMap;

use hostkeeper_launchd::launchctl::{parse_disabled, print_disabled_args};

const WORDS: &str = include_str!("fixtures/print-disabled-words.txt");
const LEGACY: &str = include_str!("fixtures/print-disabled-legacy.txt");

const DAEMON: &str = "dev.hostkeeper.hostkeeper";

/// A fixture without its leading `# ` comment lines.
fn fixture(text: &str) -> String {
    text.lines()
        .skip_while(|line| line.starts_with("# "))
        .map(|line| format!("{line}\n"))
        .collect()
}

fn entries(pairs: &[(&str, bool)]) -> BTreeMap<String, bool> {
    pairs
        .iter()
        .map(|(label, disabled)| ((*label).to_owned(), *disabled))
        .collect()
}

/// Proves: the command is `launchctl print-disabled system`.
#[test]
fn the_command_prints_the_system_domains_overrides() {
    assert_eq!(
        print_disabled_args(),
        vec!["print-disabled".to_owned(), "system".to_owned()]
    );
}

/// Proves: macOS 13's `disabled` and `enabled` words are read per label, the header and closing
/// brace ignored, and a label that is the prefix of another is its own entry.
#[test]
fn the_words_of_macos_13_are_read_per_label() {
    assert_eq!(
        parse_disabled(&fixture(WORDS)),
        entries(&[
            ("com.example.disabled-one", true),
            ("com.example.enabled-one", false),
            (DAEMON, true),
            ("dev.hostkeeper.hostkeeper-helper", false),
        ])
    );
}

/// Proves: the earlier releases' `true` (disabled) and `false` (enabled) are read the same way.
#[test]
fn the_true_and_false_of_earlier_releases_are_read_per_label() {
    assert_eq!(
        parse_disabled(&fixture(LEGACY)),
        entries(&[
            ("com.example.disabled-one", true),
            (DAEMON, false),
            ("dev.hostkeeper.hostkeeper-helper", true),
        ])
    );
}

/// Proves: a label absent from the output has no entry (launchd treats it as not disabled), and
/// the entry of a longer label is not the entry of a label that is its prefix.
#[test]
fn an_absent_label_has_no_entry_and_a_longer_label_is_not_its_prefix() {
    let only_longer =
        "disabled services = {\n\t\"dev.hostkeeper.hostkeeper-helper\" => disabled\n}\n";
    let parsed = parse_disabled(only_longer);
    assert_eq!(parsed.get(DAEMON), None);
    assert_eq!(parsed.get("dev.hostkeeper.hostkeeper-helper"), Some(&true));
    assert_eq!(
        parse_disabled("disabled services = {\n}\n"),
        BTreeMap::new()
    );
    assert_eq!(parse_disabled(""), BTreeMap::new());
}

/// Proves: a line that is not `"<label>" => <state>` with a known state is ignored, not an
/// error, and the lines around it are still read: an unknown state word, a missing arrow, an
/// unquoted label, an unclosed quote, a bare quote, and a blank line.
#[test]
fn a_malformed_line_is_ignored_and_the_lines_around_it_are_read() {
    let text = "disabled services = {\n\
        \t\"dev.hostkeeper.hostkeeper\" => disabled\n\
        \t\"com.example.odd\" => maybe\n\
        \t\"com.example.no-arrow\" disabled\n\
        \tcom.example.unquoted => disabled\n\
        \t\"com.example.unclosed => disabled\n\
        \t\"\n\
        \n\
        \t\"com.example.after\" => enabled\n\
        }\n";
    assert_eq!(
        parse_disabled(text),
        entries(&[(DAEMON, true), ("com.example.after", false)])
    );
}

/// Proves: a label printed twice keeps its last line's state.
#[test]
fn a_label_printed_twice_keeps_its_last_state() {
    let text = format!("\"{DAEMON}\" => disabled\n\"{DAEMON}\" => enabled\n");
    assert_eq!(parse_disabled(&text), entries(&[(DAEMON, false)]));
}
