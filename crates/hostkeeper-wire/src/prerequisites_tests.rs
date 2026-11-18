use super::*;

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| (*name).to_owned()).collect()
}

/// Proves every known prerequisite is read back from its own name, and that the one known today
/// applies to macOS and not to Linux.
#[test]
fn every_known_prerequisite_is_read_from_its_name() {
    for known in KNOWN {
        assert_eq!(Prerequisite::parse(known.name()), Some(known));
        assert!(is_name(known.name()));
    }
    let clt = Prerequisite::XcodeCommandLineTools;
    assert_eq!(clt.name(), "xcode-command-line-tools");
    assert!(clt.on_macos());
    assert!(!clt.on_linux());
    assert_eq!(Prerequisite::parse("xcode-command-line-tool"), None);
    assert_eq!(Prerequisite::parse(""), None);
}

/// Proves the grammar of a name at each of its edges.
#[test]
fn a_name_keeps_the_grammar() {
    assert!(is_name("a"));
    assert!(is_name("a-1"));
    assert!(is_name(&"a".repeat(MAX_NAME_BYTES)));
    for bad in [
        "",
        "1a",
        "-a",
        "a-",
        "a--b",
        "A",
        "a_b",
        "a b",
        "é",
        &"a".repeat(MAX_NAME_BYTES + 1),
    ] {
        assert!(!is_name(bad), "{bad:?} is no name");
    }
}

/// Proves a set is refused for too many names, a name out of the grammar or one twice, each
/// naming the place, and that the empty set, a full set and an unknown well-formed name pass.
#[test]
fn a_set_keeps_the_rules_of_a_set() {
    assert_eq!(check_set(&[]), Ok(()));
    assert_eq!(
        check_set(&names(&["xcode-command-line-tools", "later-thing"])),
        Ok(())
    );
    let full: Vec<String> = (0..MAX_PREREQUISITES).map(|n| format!("p{n}")).collect();
    assert_eq!(check_set(&full), Ok(()));
    let mut over = full.clone();
    over.push("p-over".to_owned());
    assert_eq!(
        check_set(&over),
        Err(PrerequisiteSetError::TooMany {
            count: MAX_PREREQUISITES + 1
        })
    );
    assert_eq!(
        check_set(&names(&["a", "B"])),
        Err(PrerequisiteSetError::Name { place: 2 })
    );
    assert_eq!(
        check_set(&names(&["a", "b", "a"])),
        Err(PrerequisiteSetError::Twice { place: 3, first: 1 })
    );
    assert_eq!(
        PrerequisiteSetError::Twice { place: 3, first: 1 }.to_string(),
        "prerequisite 3 is prerequisite 1 again"
    );
    assert_eq!(
        PrerequisiteSetError::TooMany { count: 17 }.to_string(),
        "17 prerequisites, more than the 16 a set may hold"
    );
    assert_eq!(
        PrerequisiteSetError::Name { place: 2 }.to_string(),
        "prerequisite 2 is not a prerequisite name (1 to 64 bytes of a-z, 0-9 and -, starting with a letter, not ending with - or holding --)"
    );
}

/// Proves the digest is the SHA-256 of the names each followed by a newline, so that the order
/// counts and the empty set has the digest of no bytes.
#[test]
fn the_digest_is_of_the_names_in_order() {
    assert_eq!(
        digest(&[]),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    // printf 'xcode-command-line-tools\n' | sha256sum
    assert_eq!(
        digest(&names(&["xcode-command-line-tools"])),
        "b8bd30efc075831d9224f57d243e12171dd8df33329d22baed4034dab09a4ca3"
    );
    assert_ne!(digest(&names(&["a", "b"])), digest(&names(&["b", "a"])));
    assert_eq!(digest(&names(&["a"])).len(), 64);
}
