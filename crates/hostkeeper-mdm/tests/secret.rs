//! The secret reader follows systemd's `EnvironmentFile=` rules, on the fake host.

use hostkeeper_mdm::{MdmError, secret};
use hostkeeper_mdm_step::fake::FakeHost;

const PATH: &str = "/etc/hostkeeper-mdm/nanohub.env";
const KEY: &str = "API_KEY";

/// A fake host holding `bytes` at [`PATH`] as a 0600 file of root's.
fn host_with(bytes: &[u8]) -> FakeHost {
    let host = FakeHost::new(0);
    host.put_file(PATH, 0o600, (0, 0), bytes);
    host
}

/// What `read` makes of `bytes`: the value, or the error's message.
fn read_of(bytes: &[u8]) -> Result<String, String> {
    secret::read(&host_with(bytes), PATH, KEY)
        .map(|s| s.expose().to_owned())
        .map_err(|e| e.to_string())
}

fn no_secret() -> String {
    format!("{PATH} has no {KEY}= line with a value")
}

fn syntax(line: usize, why: &str) -> String {
    format!(
        "{PATH} line {line}: {why}: systemd might read it differently from this tool; write the value on one line as KEY=value"
    )
}

/// The plain line `KEY=value` is the value.
#[test]
fn a_plain_assignment_is_the_value() {
    assert_eq!(read_of(b"API_KEY=abc123\n"), Ok("abc123".to_owned()));
    assert!(secret::has_value(b"API_KEY=abc123\n", KEY));
}

/// A last line without a newline still counts.
#[test]
fn an_assignment_without_a_final_newline_is_the_value() {
    assert_eq!(read_of(b"API_KEY=abc"), Ok("abc".to_owned()));
}

/// systemd keeps the last assignment of a key: a later non-empty value replaces an earlier one.
/// Catches reading the first `KEY=` line.
#[test]
fn the_last_of_two_assignments_is_the_value() {
    assert_eq!(read_of(b"API_KEY=old\nAPI_KEY=new\n"), Ok("new".to_owned()));
}

/// A later empty assignment empties the value systemd gives the service, so the file has no
/// value. Catches saying "has a value" for the earlier line.
#[test]
fn an_empty_assignment_after_a_value_leaves_no_value() {
    let bytes = b"API_KEY=abc\nAPI_KEY=\n";
    assert!(!secret::has_value(bytes, KEY));
    assert_eq!(read_of(bytes), Err(no_secret()));
}

/// An empty assignment first and a value after it: the value wins.
#[test]
fn a_value_after_an_empty_assignment_is_the_value() {
    let bytes = b"API_KEY=\nAPI_KEY=abc\n";
    assert!(secret::has_value(bytes, KEY));
    assert_eq!(read_of(bytes), Ok("abc".to_owned()));
}

/// Empty single and double quotes are an empty value, also as the last assignment.
#[test]
fn empty_quotes_are_an_empty_value() {
    for bytes in [
        &b"API_KEY=''\n"[..],
        b"API_KEY=\"\"\n",
        b"API_KEY=abc\nAPI_KEY=''\n",
        b"API_KEY=abc\nAPI_KEY=\"\"\n",
    ] {
        assert!(!secret::has_value(bytes, KEY), "{bytes:?}");
        assert_eq!(read_of(bytes), Err(no_secret()), "{bytes:?}");
    }
}

/// Quotes around a value are not part of it.
#[test]
fn quotes_around_a_value_are_dropped() {
    assert_eq!(read_of(b"API_KEY='abc'\n"), Ok("abc".to_owned()));
    assert_eq!(read_of(b"API_KEY=\"abc\"\n"), Ok("abc".to_owned()));
    assert!(secret::has_value(b"API_KEY='abc'\n", KEY));
}

/// Inside single quotes `$`, a double quote and spaces are literal.
#[test]
fn single_quoted_text_is_literal() {
    assert_eq!(
        read_of(b"API_KEY='a $b \"c\"  d'\n"),
        Ok("a $b \"c\"  d".to_owned())
    );
}

/// Inside double quotes a backslash escapes `"`, `\`, a backtick and `$`, and keeps its place
/// before any other character.
#[test]
fn double_quoted_text_takes_systemds_escapes() {
    assert_eq!(
        read_of(b"API_KEY=\"a\\\"b\\\\c\\`d\\$e\\xf\"\n"),
        Ok("a\"b\\c`d$e\\xf".to_owned())
    );
}

/// A backslash before a newline inside double quotes continues the value on the next line.
#[test]
fn a_backslash_newline_in_double_quotes_continues_the_value() {
    assert_eq!(read_of(b"API_KEY=\"ab\\\ncd\"\n"), Ok("abcd".to_owned()));
}

/// An unquoted backslash makes the next character literal, and before a newline it continues
/// the line.
#[test]
fn an_unquoted_backslash_escapes_or_continues() {
    assert_eq!(read_of(b"API_KEY=ab\\\ncd\n"), Ok("abcd".to_owned()));
    assert_eq!(read_of(b"API_KEY=a\\#b\\ c\n"), Ok("a#b c".to_owned()));
}

/// Whitespace around the key and before the value is dropped, and so is whitespace after an
/// unquoted value.
#[test]
fn whitespace_around_key_and_value_is_dropped() {
    assert_eq!(read_of(b"  API_KEY  =  abc \t \n"), Ok("abc".to_owned()));
    assert_eq!(read_of(b"\tAPI_KEY=abc\r\n"), Ok("abc".to_owned()));
}

/// Whitespace inside quotes stays.
#[test]
fn whitespace_inside_quotes_is_kept() {
    assert_eq!(read_of(b"API_KEY=\" a b \"  \n"), Ok(" a b ".to_owned()));
}

/// A `#` or `;` line is a comment, also an indented one, and a comment may end in a backslash
/// newline that continues it. Blank lines are skipped. A `#` inside an unquoted value is part of
/// it.
#[test]
fn comments_and_blank_lines_are_skipped() {
    let bytes = b"# API_KEY=comment\n\n  ; API_KEY=other\n   \n# go on \\\nAPI_KEY=still comment\nAPI_KEY=v#1\n";
    assert_eq!(read_of(bytes), Ok("v#1".to_owned()));
    assert!(!secret::holds_other_lines(bytes, KEY));
}

/// A commented-out assignment is no value.
#[test]
fn a_commented_out_assignment_is_no_value() {
    assert!(!secret::has_value(b"#API_KEY=abc\n", KEY));
    assert_eq!(read_of(b"#API_KEY=abc\n"), Err(no_secret()));
}

/// systemd does not know `export`: `export API_KEY=abc` assigns a variable named
/// `export API_KEY`, which it drops, so the key has no value and the line is another line.
#[test]
fn an_export_prefix_does_not_assign_the_key() {
    let bytes = b"export API_KEY=abc\n";
    assert!(!secret::has_value(bytes, KEY));
    assert!(secret::holds_other_lines(bytes, KEY));
    assert_eq!(read_of(bytes), Err(no_secret()));
}

/// A key that only starts like the wanted one is another key.
#[test]
fn a_longer_key_is_another_key() {
    let bytes = b"API_KEY_2=abc\n";
    assert!(!secret::has_value(bytes, KEY));
    assert!(secret::holds_other_lines(bytes, KEY));
}

/// `holds_other_lines`: only assignments of the key, blanks and comments hold nothing else.
#[test]
fn only_the_key_blanks_and_comments_hold_no_other_lines() {
    assert!(!secret::holds_other_lines(b"", KEY));
    assert!(!secret::holds_other_lines(b"API_KEY=\n\n# note\n", KEY));
    assert!(!secret::holds_other_lines(
        b"API_KEY = 'x'\nAPI_KEY=\"\"\n",
        KEY
    ));
}

/// `holds_other_lines`: another key or a line that is no assignment is another line.
#[test]
fn another_key_or_a_stray_line_is_another_line() {
    assert!(secret::holds_other_lines(b"OTHER=1\n", KEY));
    assert!(secret::holds_other_lines(b"API_KEY=\nOTHER=\n", KEY));
    assert!(secret::holds_other_lines(b"API_KEY=\njunk\n", KEY));
    assert!(secret::holds_other_lines(b"junk", KEY));
}

/// A file that cannot be read the way systemd will read it is no value, and counts as holding
/// other lines, so `install` refuses it and does not overwrite it.
#[test]
fn an_unreadable_file_is_no_value_and_holds_other_lines() {
    for bytes in [&b"API_KEY=\"abc\n"[..], b"API_KEY=\xff\n"] {
        assert!(!secret::has_value(bytes, KEY), "{bytes:?}");
        assert!(secret::holds_other_lines(bytes, KEY), "{bytes:?}");
    }
}

/// Bytes that are not UTF-8 are refused, with the file named, even where the key is fine.
#[test]
fn a_file_that_is_not_utf8_is_refused() {
    assert_eq!(
        read_of(b"API_KEY=abc\nOTHER=\xff\n"),
        Err(format!("{PATH} is not UTF-8 text"))
    );
}

/// A construct whose systemd reading is not certain is refused with its line.
#[test]
fn constructs_with_an_uncertain_reading_are_refused() {
    assert_eq!(
        read_of(b"A=1\nAPI_KEY=\"abc\n"),
        Err(syntax(2, "a quote is not closed"))
    );
    assert_eq!(
        read_of(b"API_KEY='abc\n"),
        Err(syntax(1, "a quote is not closed"))
    );
    assert_eq!(read_of(b"\n=abc\n"), Err(syntax(2, "a line starts with =")));
    assert_eq!(
        read_of(b"API_KEY='abc'def\n"),
        Err(syntax(1, "text follows a closing quote"))
    );
    assert_eq!(
        read_of(b"API_KEY=\"abc\"def\n"),
        Err(syntax(1, "text follows a closing quote"))
    );
    assert_eq!(
        read_of(b"API_KEY='a\\b'\n"),
        Err(syntax(1, "a backslash in single quotes"))
    );
    assert_eq!(
        read_of(b"API_KEY=ab\\\n cd\n"),
        Err(syntax(1, "a continued line starts with white space"))
    );
    assert_eq!(
        read_of(b"X=1\nAPI_KEY=ab\\\n"),
        Err(syntax(2, "a backslash ends the file"))
    );
    assert_eq!(
        read_of(b"API_KEY=\"ab\\"),
        Err(syntax(1, "a backslash ends the file"))
    );
}

/// The line number names the line where the refused construct starts, after lines a quoted
/// value spans.
#[test]
fn a_refusal_names_the_line_where_the_construct_starts() {
    assert_eq!(
        read_of(b"A=\"x\ny\"\nB='\n"),
        Err(syntax(3, "a quote is not closed"))
    );
}

/// A value with a control character is refused, also one made by a quoted newline.
#[test]
fn a_value_with_a_control_character_is_refused() {
    let message = format!("{PATH}: the value of {KEY} holds a control character");
    assert_eq!(read_of(b"API_KEY=a\x07b\n"), Err(message.clone()));
    assert_eq!(read_of(b"API_KEY=\"a\nb\"\n"), Err(message.clone()));
    assert_eq!(read_of(b"API_KEY='a\tb'\n"), Err(message));
}

/// A missing file, a file without the key and an empty file are `NoSecret`.
#[test]
fn a_missing_file_or_key_is_no_secret() {
    let host = FakeHost::new(0);
    assert!(matches!(
        secret::read(&host, PATH, KEY),
        Err(MdmError::NoSecret { .. })
    ));
    assert_eq!(read_of(b"OTHER=1\n"), Err(no_secret()));
    assert_eq!(read_of(b""), Err(no_secret()));
}

/// A file of mode 0644 is refused with the fix in the message.
#[test]
fn a_file_of_mode_0644_is_refused() {
    let host = FakeHost::new(0);
    host.put_file(PATH, 0o644, (0, 0), b"API_KEY=abc\n");
    assert_eq!(
        secret::read(&host, PATH, KEY).unwrap_err().to_string(),
        format!(
            "{PATH} has mode 0644 and uid 0, not 0600 and root: chmod 600 {PATH}; chown root:root {PATH}"
        )
    );
}

/// A 0600 file of another user is refused.
#[test]
fn a_file_of_another_uid_is_refused() {
    let host = FakeHost::new(0);
    host.put_file(PATH, 0o600, (1000, 1000), b"API_KEY=abc\n");
    assert_eq!(
        secret::read(&host, PATH, KEY).unwrap_err().to_string(),
        format!(
            "{PATH} has mode 0600 and uid 1000, not 0600 and root: chmod 600 {PATH}; chown root:root {PATH}"
        )
    );
}

/// `check_mode` takes exactly mode 0600 and uid 0.
#[test]
fn check_mode_takes_only_0600_root() {
    assert!(secret::check_mode(PATH, 0o600, 0).is_ok());
    assert!(secret::check_mode(PATH, 0o640, 0).is_err());
    assert!(secret::check_mode(PATH, 0o600, 1).is_err());
}

/// A last line with an empty or quoted value and no newline is read as it would be with one.
#[test]
fn a_last_line_without_a_newline_is_read_whole() {
    assert!(!secret::has_value(b"API_KEY=abc\nAPI_KEY=", KEY));
    assert!(!secret::has_value(b"API_KEY=abc\nAPI_KEY=''", KEY));
    assert_eq!(read_of(b"API_KEY=\"abc\""), Ok("abc".to_owned()));
    assert!(!secret::holds_other_lines(b"API_KEY=abc\nAPI_KEY=", KEY));
}

/// A backslash that starts the value escapes the next character, also a newline.
#[test]
fn a_backslash_starting_the_value_escapes_the_next_character() {
    assert_eq!(read_of(b"API_KEY=\\#x\n"), Ok("#x".to_owned()));
    assert_eq!(read_of(b"API_KEY=\\\nabc\n"), Ok("abc".to_owned()));
}

/// A secret's `Debug` hides the value.
#[test]
fn a_secret_debug_hides_the_value() {
    let s = secret::read(&host_with(b"API_KEY=hunter2\n"), PATH, KEY).unwrap();
    assert_eq!(format!("{s:?}"), "Secret(..)");
}
