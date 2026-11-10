//! The program a launchd plist starts, as launchd chooses it.

use super::plist_program;

const GOOD: &str = "/usr/local/lib/web/current/bin/web";
const BAD: &str = "/usr/local/lib/web/1.0.0/bin/web";

/// A plist whose `ProgramArguments` begin with `first`.
fn plist(first: &str) -> String {
    format!("<key>ProgramArguments</key>\n<array>\n<string>{first}</string>\n</array>\n")
}

/// Catches the daemon hashing a file launchd does not start: `plist_program` gives the
/// `Program` string when the plist has that key, over a `ProgramArguments` that follows or
/// precedes it, else the first `ProgramArguments` string; a `Program` key not followed by a
/// string, or a string never closed, gives nothing rather than the arguments; a commented-out
/// `Program` is skipped; references are decoded; a plist with neither key gives nothing.
#[test]
fn plist_program_is_the_program_key_else_the_first_argument() {
    let program = |value: &str| format!("<key>Program</key>\n<string>{value}</string>\n");
    let arguments = plist(BAD);
    assert_eq!(plist_program(&arguments), Some(BAD.to_owned()));
    assert_eq!(plist_program(&program(GOOD)), Some(GOOD.to_owned()));
    let before = format!("{}{arguments}", program(GOOD));
    let after = format!("{arguments}{}", program(GOOD));
    assert_eq!(plist_program(&before), Some(GOOD.to_owned()));
    assert_eq!(plist_program(&after), Some(GOOD.to_owned()));
    let not_a_string = format!("<key>Program</key><true/>{arguments}");
    assert_eq!(plist_program(&not_a_string), None);
    let unclosed = format!("<key>Program</key><string>{GOOD}");
    assert_eq!(plist_program(&unclosed), None);
    let commented = format!("<!-- {} -->{arguments}", program(GOOD));
    assert_eq!(plist_program(&commented), Some(BAD.to_owned()));
    assert_eq!(
        plist_program(&program("&#47;bin&#x2F;a&amp;b")),
        Some("/bin/a&b".to_owned())
    );
    assert_eq!(plist_program("<plist><dict></dict></plist>"), None);
    assert_eq!(plist_program(""), None);
}
