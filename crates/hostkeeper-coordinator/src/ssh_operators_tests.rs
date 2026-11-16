//! The operators file: what it parses, what it refuses, and the default deny of the match.

use std::os::unix::fs::PermissionsExt as _;

use super::{Class, Operators, OperatorsError, Target};
use crate::testutil::scratch;

const FILE: &str = r#"
[[operator]]
name = "alice"
classes = ["fleet", "mac-build"]

[[operator]]
name = "bob"
hosts = ["node-07", "node-08"]

[[operator]]
name = "carol"
classes = ["control-plane"]
"#;

fn target<'a>(id: &'a str, group: &'a str, class: Class) -> Target<'a> {
    Target { id, group, class }
}

/// Proves: an operator may reach a host when its entry lists the host's id in `hosts`, or the
/// host's class or host group in `classes`, and no other: the control-plane class is not the
/// fleet, a group is matched by its name only, a host in no group matches no group, and an
/// operator the file does not list may reach nothing.
///
/// Catches: a default that allows, a class that reaches the other class, and an empty group name
/// that matches an entry's empty string.
#[test]
fn an_operator_reaches_what_its_entry_lists_and_nothing_else() {
    let operators = Operators::parse(FILE).expect("the file");
    let fleet = |id| target(id, "", Class::Fleet);
    assert!(operators.permits("alice", &fleet("node-01")));
    assert!(operators.permits("alice", &target("mac-01", "mac-build", Class::Fleet)));
    assert!(!operators.permits("alice", &target("nuc", "", Class::ControlPlane)));
    assert!(operators.permits("bob", &fleet("node-07")));
    assert!(!operators.permits("bob", &fleet("node-09")));
    assert!(operators.permits("carol", &target("nuc", "", Class::ControlPlane)));
    assert!(!operators.permits("carol", &fleet("node-01")));
    assert!(!operators.permits("dave", &fleet("node-07")));
    assert!(!operators.permits("bob", &target("node-09", "node-07", Class::Fleet)));
    assert!(operators.lists("bob") && !operators.lists("dave"));
}

/// Proves: a file that lists no one, or no file content at all, permits no one.
///
/// Catches: an empty file that allows every operator.
#[test]
fn a_file_that_lists_no_one_permits_no_one() {
    for text in ["", "# nobody\n"] {
        let operators = Operators::parse(text).expect("an empty file");
        assert!(!operators.permits("alice", &target("node-01", "", Class::Fleet)));
        assert!(!operators.lists("alice"));
    }
    assert_eq!(Class::Fleet.as_str(), "fleet");
    assert_eq!(Class::ControlPlane.as_str(), "control-plane");
}

/// Proves: a file with a key it does not know, a value of the wrong type, a name that is not a
/// token name (capitals, too long, a name the audit keeps for itself) or a name twice is refused
/// with the reason.
///
/// Catches: a misspelt key (`host` for `hosts`) that is silently ignored, so an operator gets
/// less or more than the owner wrote.
#[test]
fn a_file_with_a_mistake_in_it_is_refused_with_the_reason() {
    let refused = |text: &str| Operators::parse(text).expect_err("refused");
    assert!(matches!(
        refused("[[operator]]\nname = \"a\"\nhost = [\"x\"]\n"),
        OperatorsError::Parse(_)
    ));
    assert!(matches!(
        refused("[[operator]]\nname = \"a\"\nhosts = \"x\"\n"),
        OperatorsError::Parse(_)
    ));
    assert!(matches!(refused("[[operator]]\nhosts = []\n"), OperatorsError::Parse(_)));
    for name in ["Alice", "", &"a".repeat(33), "coordinator"] {
        assert_eq!(
            refused(&format!("[[operator]]\nname = {name:?}\n")),
            OperatorsError::BadName(name.to_owned())
        );
    }
    assert_eq!(
        refused("[[operator]]\nname = \"a\"\n[[operator]]\nname = \"a\"\n"),
        OperatorsError::Duplicate("a".to_owned())
    );
    assert!(refused("[[operator]]\nname = \"a\"\nhost = 1\n").to_string().contains("does not parse"));
}

/// Proves: the file is read from its path, and a file that is missing, is a directory or is over
/// 256 KiB is refused naming the path.
///
/// Catches: a read that follows a huge file into memory.
#[test]
fn the_file_is_read_from_its_path_and_a_bad_path_is_refused_naming_it() {
    let dir = scratch("ssh-operators-read");
    let path = dir.path().join("operators.toml");
    std::fs::write(&path, FILE).expect("written");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("mode");
    assert!(Operators::read(&path).expect("read").lists("alice"));
    let missing = dir.path().join("missing.toml");
    let error = Operators::read(&missing).expect_err("missing");
    assert!(matches!(&error, OperatorsError::Read { path, .. } if *path == missing), "{error}");
    assert!(error.to_string().starts_with(&missing.display().to_string()));
    let big = dir.path().join("big.toml");
    std::fs::write(&big, "#".repeat(256 * 1024 + 1)).expect("written");
    let error = Operators::read(&big).expect_err("too big");
    assert!(error.to_string().contains("longer than 262144 bytes"), "{error}");
    let unreadable = Operators::read(dir.path()).expect_err("a directory");
    assert!(matches!(unreadable, OperatorsError::Read { .. }));
}
