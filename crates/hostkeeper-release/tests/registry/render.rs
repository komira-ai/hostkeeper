//! `registry::render`, `digest` and `keyring_sha256`: the files a host holds and the digest of them.

use hostkeeper_release::registry::{
    HostFile, KEYRING_SUFFIX, REPOSITORIES_DIR, SOURCES_SUFFIX, digest, keyring_file,
    keyring_sha256, render,
};
use sha2::{Digest, Sha256};

use super::support::*;

const KOMIRA_URI: &str = "https://repo.example.com/debian";

/// Proves the constants are the names a host's directory uses.
#[test]
fn the_names_of_a_hosts_directory() {
    assert_eq!(REPOSITORIES_DIR, "/etc/hostkeeper/repositories");
    assert_eq!(KEYRING_SUFFIX, ".gpg");
    assert_eq!(SOURCES_SUFFIX, ".sources");
    assert_eq!(keyring_file("komira"), "komira.gpg");
}

fn sources(uri: &str, suite: &str, components: &str, signed_by: &str) -> Vec<u8> {
    format!(
        "Types: deb\nURIs: {uri}\nSuites: {suite}\nComponents: {components}\nSigned-By: {signed_by}\n"
    )
    .into_bytes()
}

fn file(name: &str, bytes: Vec<u8>) -> HostFile {
    HostFile {
        name: name.into(),
        bytes,
    }
}

/// Proves komira renders the three files the held script writes, to the byte, sorted by name.
#[test]
fn komira_renders_the_files_the_held_script_writes() {
    let entry = entry("komira");
    let key = "/etc/hostkeeper/repositories/komira.gpg";
    assert_eq!(
        render(std::slice::from_ref(&entry), REPOSITORIES_DIR),
        vec![
            file(
                "komira-stable.sources",
                sources(KOMIRA_URI, "stable", "main", key)
            ),
            file(
                "komira-testing.sources",
                sources(KOMIRA_URI, "testing", "main", key)
            ),
            file("komira.gpg", entry.keyring.clone()),
        ]
    );
    assert_eq!(
        sources(KOMIRA_URI, "stable", "main", key),
        b"Types: deb\nURIs: https://repo.example.com/debian\nSuites: stable\nComponents: main\n\
          Signed-By: /etc/hostkeeper/repositories/komira.gpg\n"
    );
}

/// Proves components are joined by one space and the keyring is named under the directory given.
#[test]
fn components_are_joined_by_a_space_and_the_directory_is_the_one_given() {
    let mut entry = entry("a");
    entry.suites = vec!["only".into()];
    entry.components = vec!["main".into(), "contrib".into(), "non-free".into()];
    let files = render(&[entry], "/scratch/dir");
    assert_eq!(
        files[0].bytes,
        sources(
            KOMIRA_URI,
            "only",
            "main contrib non-free",
            "/scratch/dir/a.gpg"
        )
    );
}

/// Proves entries in any order render in one order: the bytes of the names.
#[test]
fn entries_render_sorted_by_file_name_whatever_their_order() {
    let (a, b) = (entry("b"), entry("a-b"));
    let names = |files: Vec<HostFile>| files.into_iter().map(|f| f.name).collect::<Vec<_>>();
    let want = [
        "a-b-stable.sources",
        "a-b-testing.sources",
        "a-b.gpg",
        "b-stable.sources",
        "b-testing.sources",
        "b.gpg",
    ];
    assert_eq!(names(render(&[a.clone(), b.clone()], "/d")), want);
    assert_eq!(names(render(&[b, a], "/d")), want);
    assert!(render(&[], "/d").is_empty());
}

/// Proves the digest is the SHA-256 of each file as its name, a zero byte, its length in eight
/// big-endian bytes and its bytes, and does not depend on the entries' order.
#[test]
fn the_digest_hashes_each_file_as_name_zero_length_bytes() {
    let one = entry("komira");
    let key = "/etc/hostkeeper/repositories/komira.gpg";
    let mut want = Sha256::new();
    for (name, bytes) in [
        (
            "komira-stable.sources",
            sources(KOMIRA_URI, "stable", "main", key),
        ),
        (
            "komira-testing.sources",
            sources(KOMIRA_URI, "testing", "main", key),
        ),
        ("komira.gpg", one.keyring.clone()),
    ] {
        want.update(name.as_bytes());
        want.update([0]);
        want.update(u64::try_from(bytes.len()).expect("short").to_be_bytes());
        want.update(&bytes);
    }
    let want: String = want.finalize().iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(digest(std::slice::from_ref(&one)), want);
    assert_eq!(want.len(), 64);
    let two = entry("other");
    assert_eq!(
        digest(&[one.clone(), two.clone()]),
        digest(&[two.clone(), one.clone()])
    );
    assert_ne!(digest(std::slice::from_ref(&one)), digest(&[one, two]));
}

/// Proves a change of a keyring, a uri, a suite or a component changes the digest, and a set
/// of no entry has the digest of nothing.
#[test]
fn any_change_to_the_files_changes_the_digest() {
    let base = entry("komira");
    let mut changes = Vec::new();
    let mut e = base.clone();
    e.keyring.push(0);
    changes.push(e);
    let mut e = base.clone();
    e.uri = "https://repo.example.com/other".into();
    changes.push(e);
    let mut e = base.clone();
    e.suites.pop();
    changes.push(e);
    let mut e = base.clone();
    e.components.push("extra".into());
    changes.push(e);
    let mut seen = vec![digest(&[base])];
    for e in changes {
        let d = digest(&[e]);
        assert!(!seen.contains(&d));
        seen.push(d);
    }
    assert_eq!(
        digest(&[]),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

/// Proves the keyring's SHA-256 is lower-case hex.
#[test]
fn the_keyring_sha256_is_lower_case_hex() {
    assert_eq!(
        keyring_sha256(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        keyring_sha256(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}
