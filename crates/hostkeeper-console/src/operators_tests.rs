//! The console operators file: who may open a console and who may also change power.

use std::io;

use super::{Denied, FLAG, MAX_FILE_BYTES, Need, Operators, OperatorsError};
use crate::file::ReadError;
use crate::testutil::{put, scratch};

/// Proves: an operator the file names may open a console, and one whose entry says nothing of
/// power may change it too; a name the file does not hold is refused `NoConsoleAccess` for both,
/// however close to a listed name it is.
///
/// Catches: a console opened by a token the file does not name, a name matched by prefix or
/// case, and `power` defaulting to false.
#[test]
fn a_name_not_in_the_operators_file_is_refused() {
    let operators = Operators::parse("[[operator]]\nname = \"alice\"\n").expect("parsed");
    assert_eq!(operators.check("alice", Need::Console), Ok(()));
    assert_eq!(operators.check("alice", Need::Power), Ok(()));
    for name in ["bob", "alic", "alice2", "Alice", ""] {
        for need in [Need::Console, Need::Power] {
            assert_eq!(
                operators.check(name, need),
                Err(Denied::NoConsoleAccess),
                "{name} {need:?}"
            );
        }
    }
}

/// Proves: with no file, and with a file that names nobody, every name is refused
/// `NoConsoleAccess` for every call.
///
/// Catches: a coordinator without the file that lets any operator open a console, and an empty
/// file read as "everyone".
#[test]
fn no_operators_file_means_nobody() {
    let empty = Operators::parse("").expect("an empty file parses");
    assert_eq!(empty, Operators::nobody());
    for operators in [Operators::nobody(), empty] {
        for need in [Need::Console, Need::Power] {
            assert_eq!(operators.check("alice", need), Err(Denied::NoConsoleAccess));
        }
    }
}

/// Proves: an operator whose entry says `power = false` may open a console and is refused
/// `PowerNotPermitted` for power and the boot device, and `power = true` allows both.
///
/// Catches: a power call allowed to an operator the file limits to the console, and a console
/// refused to that operator.
#[test]
fn power_calls_need_the_power_flag() {
    let operators = Operators::parse(concat!(
        "[[operator]]\nname = \"bob\"\npower = false\n",
        "[[operator]]\nname = \"carol\"\npower = true\n",
    ))
    .expect("parsed");
    assert_eq!(operators.check("bob", Need::Console), Ok(()));
    assert_eq!(
        operators.check("bob", Need::Power),
        Err(Denied::PowerNotPermitted)
    );
    assert_eq!(operators.check("carol", Need::Power), Ok(()));
}

/// Proves: a file that breaks a rule is refused with an error naming the flag and the rule, by
/// exact text: a name outside the token grammar (empty, upper case, a space, 33 bytes), a name
/// twice, an unknown key in an entry or at the top, `power` that is not a boolean, and text that
/// is not TOML. A name of exactly 32 bytes, and one with a dash and a digit, are taken.
///
/// Catches: a file with a typo that is read as granting nobody (or somebody) without a word,
/// and an error that does not say which flag or entry is wrong.
#[test]
fn an_operators_file_that_breaks_a_rule_is_refused_naming_the_flag() {
    let long = "a".repeat(32);
    assert!(Operators::parse(&format!("[[operator]]\nname = \"{long}\"\n")).is_ok());
    let dashed = Operators::parse("[[operator]]\nname = \"ops-2\"\n").expect("a dash and a digit");
    assert_eq!(dashed.check("ops-2", Need::Console), Ok(()));
    let refused = |text: &str| Operators::parse(text).expect_err("refused").to_string();
    for (name, shown) in [
        ("", "\"\""),
        ("Alice", "\"Alice\""),
        ("al ice", "\"al ice\""),
    ] {
        assert_eq!(
            refused(&format!(
                "[[operator]]\nname = \"ok\"\n[[operator]]\nname = \"{name}\"\n"
            )),
            format!(
                "--console-operators-file: operator 2: the name {shown} is not 1 to 32 bytes of a-z, 0-9 and -"
            )
        );
    }
    assert!(
        refused(&format!("[[operator]]\nname = \"{long}a\"\n"))
            .starts_with("--console-operators-file: operator 1: the name")
    );
    assert_eq!(
        refused("[[operator]]\nname = \"alice\"\n[[operator]]\nname = \"alice\"\npower = false\n"),
        "--console-operators-file: the operator alice is named twice"
    );
    for text in [
        "[[operator]]\nname = \"alice\"\nrole = \"admin\"\n",
        "[[operators]]\nname = \"alice\"\n",
        "[[operator]]\nname = \"alice\"\npower = \"yes\"\n",
        "[[operator]]\npower = true\n",
        "not toml",
    ] {
        let text = refused(text);
        assert!(text.starts_with("--console-operators-file: "), "{text}");
        assert!(
            !text.contains('\n'),
            "the parser's quote of the file is not kept: {text}"
        );
    }
}

/// Proves: the file is read from disk with the rules of the text, and a path that is missing, a
/// directory, over the cap or not UTF-8 is refused naming the flag and not the path.
///
/// Catches: a file read without its cap, a directory read as an empty file (nobody, silently),
/// and an error that prints where the file is.
#[test]
fn the_operators_file_is_read_from_disk_and_a_bad_file_is_refused() {
    let dir = scratch("operators-read");
    let good = put(
        dir.path(),
        "good.toml",
        b"[[operator]]\nname = \"alice\"\n",
        0o644,
    );
    let read = Operators::read(&good).expect("read");
    assert_eq!(read.check("alice", Need::Power), Ok(()));

    let missing = dir.path().join("missing.toml");
    assert_eq!(
        Operators::read(&missing),
        Err(OperatorsError::Read(ReadError::Io {
            flag: FLAG,
            kind: io::ErrorKind::NotFound
        }))
    );
    assert_eq!(
        Operators::read(dir.path()),
        Err(OperatorsError::Read(ReadError::NotAFile { flag: FLAG }))
    );
    let big = put(
        dir.path(),
        "big.toml",
        &vec![b'#'; usize::try_from(MAX_FILE_BYTES).expect("small") + 1],
        0o644,
    );
    let too_long = Operators::read(&big).expect_err("refused");
    assert_eq!(
        too_long.to_string(),
        "--console-operators-file: it is over 65536 bytes"
    );
    let binary = put(dir.path(), "binary.toml", &[0xff, 0xfe], 0o644);
    assert_eq!(
        Operators::read(&binary).expect_err("refused").to_string(),
        "--console-operators-file: it is not UTF-8"
    );
    let at_cap = put(
        dir.path(),
        "cap.toml",
        &vec![b'#'; usize::try_from(MAX_FILE_BYTES).expect("small")],
        0o644,
    );
    assert_eq!(Operators::read(&at_cap), Ok(Operators::nobody()));
    let path = dir.path().display().to_string();
    for error in [
        too_long.to_string(),
        Operators::read(&missing).expect_err("refused").to_string(),
    ] {
        assert!(!error.contains(&path), "{error}");
    }
}
