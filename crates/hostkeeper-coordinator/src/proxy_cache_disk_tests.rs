//! The names of the cache's files: a key's digest.

use super::{Key, is_digest};

/// Proves: a key's digest is 64 lowercase hex digits, the same for the same key, and differs when
/// the boundary between the origin and the path moves, so two keys never share a file.
///
/// Catches: a name made by joining the origin and the path with nothing between them.
#[test]
fn a_key_digest_is_a_sha256_that_keeps_the_origin_and_the_path_apart() {
    let key = Key::new("repo", "dists/stable/InRelease");
    assert!(is_digest(&key.digest()));
    assert_eq!(
        key.digest(),
        Key::new("repo", "dists/stable/InRelease").digest()
    );
    assert_ne!(Key::new("a", "bc").digest(), Key::new("ab", "c").digest());
    assert_eq!(
        Key::new("", "").digest(),
        "6e340b9cffb37a989ca544e6bb780a2c78901d3fb33738768511a30617afa01d"
    );
}

/// Proves: only 64 lowercase hex digits are taken for an object's name.
///
/// Catches: a start that takes a stray file for an object.
#[test]
fn only_a_lowercase_sha256_names_an_object() {
    assert!(!is_digest("abc"));
    assert!(!is_digest(&"A".repeat(64)));
    assert!(!is_digest(&"g".repeat(64)));
    assert!(is_digest(&"0".repeat(64)));
}
