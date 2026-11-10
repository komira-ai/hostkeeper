//! `registry::keys`: the keys of an OpenPGP keyring and their fingerprints.

use hostkeeper_release::registry::{Key, KeyringError, keys};

use super::real;
use super::support::*;

/// Proves a keyring that `gpg --export` wrote parses to its primary key and its subkey, by the
/// fingerprints `gpg --with-colons` printed.
#[test]
fn a_real_keyring_lists_its_primary_key_and_subkey() {
    let found = keys(&real::keyring()).expect("a real keyring");
    assert_eq!(
        found,
        vec![Key {
            fingerprint: real::PRIMARY.into(),
            subkeys: vec![real::SUBKEY.into()],
        }]
    );
}

/// Proves a version 4 key's fingerprint is the SHA-1 of 0x99, its 16 bit length and its body,
/// whatever packet format and length form carry it.
#[test]
fn a_version_4_fingerprint_is_the_sha1_however_the_packet_is_framed() {
    for pad in [0, 10, 185, 186, 187, 8000, 8191, 8192, 9000] {
        let body = v4_body(7, pad);
        let want = v4_print(&body);
        for packet in [
            new(6, &body),
            old(6, 4, &body),
            if pad < 250 {
                old(6, 1, &body)
            } else {
                old(6, 2, &body)
            },
        ] {
            let found = keys(&packet).expect("a key");
            assert_eq!(found[0].fingerprint, want, "pad {pad}");
        }
    }
}

/// Proves a version 6 key's fingerprint is the SHA-256 of 0x9B, its 32 bit length and its body.
#[test]
fn a_version_6_fingerprint_is_the_sha256_with_a_32_bit_length() {
    let body = v6_body(9, 40);
    let found = keys(&key(&body)).expect("a key");
    assert_eq!(found[0].fingerprint, v6_print(&body));
    assert_eq!(found[0].fingerprint.len(), 64);
}

/// Proves several keys keep their order, each subkey follows the key before it, and the packets
/// that are neither (signatures, user ids, attributes, trust) are skipped, also between a key
/// and its subkey.
#[test]
fn keys_and_subkeys_keep_their_order_and_other_packets_are_skipped() {
    let (a, b, s1, s2, s3) = (
        v4_body(1, 3),
        v6_body(2, 3),
        v4_body(3, 3),
        v6_body(4, 3),
        v4_body(5, 3),
    );
    let mut ring = Vec::new();
    ring.extend(key(&a));
    ring.extend(new(13, b"id"));
    ring.extend(old(2, 1, b"sig"));
    ring.extend(subkey(&s1));
    ring.extend(new(17, b"attr"));
    ring.extend(old(12, 1, b"trust"));
    ring.extend(subkey(&s2));
    ring.extend(key(&b));
    ring.extend(old(14, 2, &s3));
    let found = keys(&ring).expect("two keys");
    assert_eq!(
        found,
        vec![
            Key {
                fingerprint: v4_print(&a),
                subkeys: vec![v4_print(&s1), v6_print(&s2)],
            },
            Key {
                fingerprint: v6_print(&b),
                subkeys: vec![v4_print(&s3)],
            },
        ]
    );
}

fn refused(bytes: &[u8]) -> KeyringError {
    keys(bytes).expect_err("a refused keyring")
}

/// Proves an empty keyring, and a keyring whose first packet is not a public key, is refused.
#[test]
fn a_keyring_must_start_with_a_public_key() {
    assert_eq!(refused(&[]), KeyringError::NotKey);
    assert_eq!(refused(&new(2, b"sig")), KeyringError::NotKey);
    assert_eq!(refused(&new(13, b"id")), KeyringError::NotKey);
    assert_eq!(refused(&subkey(&v4_body(1, 1))), KeyringError::NotKey);
}

/// Proves a secret key or secret subkey is refused wherever it stands.
#[test]
fn a_secret_key_is_refused() {
    let public = key(&v4_body(1, 1));
    let mut ring = public.clone();
    ring.extend(new(5, &v4_body(2, 1)));
    assert_eq!(refused(&ring), KeyringError::Secret(public.len()));
    let mut ring = public.clone();
    ring.extend(old(7, 1, &v4_body(2, 1)));
    assert_eq!(refused(&ring), KeyringError::Secret(public.len()));
    assert_eq!(refused(&new(5, &v4_body(2, 1))), KeyringError::Secret(0));
}

/// Proves a byte that is no packet header, an old format indeterminate length and a new format
/// partial length are refused, with the byte they start at.
#[test]
fn a_bad_header_or_length_form_is_refused() {
    assert_eq!(refused(&[0x01, 0x02]), KeyringError::Header(0));
    let public = key(&v4_body(1, 1));
    let mut ring = public.clone();
    ring.push(0x7f);
    assert_eq!(refused(&ring), KeyringError::Header(public.len()));
    assert_eq!(refused(&[0x80 | (6 << 2) | 3, 1]), KeyringError::Length(0));
    for partial in [224u8, 254] {
        assert_eq!(refused(&[0xc6, partial, 1]), KeyringError::Length(0));
    }
}

/// Proves a keyring that ends inside a packet's length or body is refused, however the length
/// is written.
#[test]
fn a_truncated_packet_is_refused() {
    let body = v4_body(1, 9000);
    for whole in [new(6, &body), old(6, 4, &body), old(6, 1, &v4_body(1, 3))] {
        for cut in 1..whole.len() {
            assert_eq!(
                refused(&whole[..cut]),
                KeyringError::Truncated(0),
                "cut {cut}"
            );
        }
    }
    let two = new(6, &v4_body(1, 190));
    assert_eq!(two[1] & 0xc0, 0xc0);
    for cut in 1..two.len() {
        assert_eq!(refused(&two[..cut]), KeyringError::Truncated(0));
    }
    let valid = key(&v4_body(1, 1));
    let mut ring = valid.clone();
    ring.extend(&valid[..2]);
    assert_eq!(refused(&ring), KeyringError::Truncated(valid.len()));
}

/// Proves a key packet with no body, with a version other than 4 and 6, and with a version 4 body
/// over 65535 bytes (the 16 bit length of its fingerprint) is refused, and 65535 is not.
#[test]
fn a_key_packet_needs_a_body_of_a_known_version_and_size() {
    assert_eq!(refused(&new(6, &[])), KeyringError::Empty(0));
    let mut ring = key(&v4_body(1, 1));
    let at = ring.len();
    ring.extend(subkey(&[]));
    assert_eq!(refused(&ring), KeyringError::Empty(at));
    for version in [0u8, 3, 5, 7] {
        let mut body = v4_body(1, 3);
        body[0] = version;
        assert_eq!(
            refused(&key(&body)),
            KeyringError::Version { at: 0, version }
        );
    }
    let biggest = v4_body(1, 65535 - 6);
    assert_eq!(biggest.len(), 65535);
    assert_eq!(
        keys(&old(6, 4, &biggest)).expect("fits")[0].fingerprint,
        v4_print(&biggest)
    );
    let over = v4_body(1, 65536 - 6);
    assert_eq!(refused(&old(6, 4, &over)), KeyringError::Oversize(0));
}

/// Proves every error says what is wrong.
#[test]
fn keyring_errors_say_where_and_why() {
    let table = [
        (
            KeyringError::Header(3),
            "byte 3 does not start an OpenPGP packet",
        ),
        (
            KeyringError::Length(4),
            "the packet at byte 4 has an indeterminate or partial length",
        ),
        (
            KeyringError::Truncated(5),
            "the keyring ends inside the packet at byte 5",
        ),
        (
            KeyringError::Secret(6),
            "the keyring holds a secret key at byte 6",
        ),
        (KeyringError::NotKey, "the first packet is not a public key"),
        (KeyringError::Empty(7), "the key packet at byte 7 is empty"),
        (
            KeyringError::Version { at: 8, version: 3 },
            "the key at byte 8 has version 3, not 4 or 6",
        ),
        (
            KeyringError::Oversize(9),
            "the key at byte 9 is too long for a version 4 fingerprint",
        ),
    ];
    for (error, text) in table {
        assert_eq!(error.to_string(), text);
    }
}
