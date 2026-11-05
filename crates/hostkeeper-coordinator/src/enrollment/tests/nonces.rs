//! The poll nonces: one use each, for their own request only, the newest few per request, and
//! none when there are no random bytes.

use hostkeeper_enrollment::names::KeyHash;

use crate::enrollment::{NONCE_BYTES, NONCES_PER_REQUEST, Nonces, os_random};

/// Catches a nonce taken twice, taken for another request, kept past the newest
/// [`NONCES_PER_REQUEST`], or a nonce that is not [`NONCE_BYTES`] fresh random bytes.
#[test]
fn a_nonce_is_taken_once_for_its_own_request_and_only_the_newest_are_kept() {
    let nonces = Nonces::new(os_random());
    let (mine, theirs) = (KeyHash::of_spki(b"mine"), KeyHash::of_spki(b"theirs"));
    let given: Vec<Vec<u8>> = (0..=NONCES_PER_REQUEST)
        .map(|_| nonces.give(&mine).expect("a nonce"))
        .collect();
    assert!(given.iter().all(|nonce| nonce.len() == NONCE_BYTES));
    assert_ne!(given[0], given[1]);
    assert!(!nonces.take(&mine, &given[0]), "the oldest was forgotten");
    assert!(!nonces.take(&theirs, &given[1]), "not their nonce");
    assert!(nonces.take(&mine, &given[1]));
    assert!(!nonces.take(&mine, &given[1]), "taken once");
    for nonce in &given[2..] {
        assert!(nonces.take(&mine, nonce));
    }
    assert!(!nonces.take(&mine, &given[2]), "every nonce is taken");
}

/// Catches a nonce given when no random bytes could be had.
#[test]
fn no_random_bytes_give_no_nonce() {
    let nonces = Nonces::new(Box::new(|_: &mut [u8]| false));
    assert_eq!(nonces.give(&KeyHash::of_spki(b"mine")), None);
}
