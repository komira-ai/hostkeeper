//! Keys and entries built by hand: the fingerprints are computed here, from the packets' bytes,
//! with the hashes of RFC 9580 section 5.5.4, not taken from the code under test.

use hostkeeper_proto::hostkeeper::release::v1::Repository;
use sha1::Sha1;
use sha2::{Digest, Sha256};

pub fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex"))
        .collect()
}

pub fn upper(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

/// A version 4 public key body: version, creation time, algorithm, then `pad` bytes of `seed`.
pub fn v4_body(seed: u8, pad: usize) -> Vec<u8> {
    let mut body = vec![4, 0, 0, 0, 1, 22];
    body.extend(std::iter::repeat_n(seed, pad));
    body
}

/// A version 6 public key body.
pub fn v6_body(seed: u8, pad: usize) -> Vec<u8> {
    let mut body = vec![6, 0, 0, 0, 1, 27];
    body.extend(std::iter::repeat_n(seed, pad));
    body
}

pub fn v4_print(body: &[u8]) -> String {
    let mut hash = Sha1::new();
    hash.update([0x99]);
    hash.update(u16::try_from(body.len()).expect("short").to_be_bytes());
    hash.update(body);
    upper(&hash.finalize())
}

pub fn v6_print(body: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update([0x9b]);
    hash.update(u32::try_from(body.len()).expect("short").to_be_bytes());
    hash.update(body);
    upper(&hash.finalize())
}

/// An old format packet with a length of `width` bytes (1, 2 or 4).
pub fn old(tag: u8, width: u8, body: &[u8]) -> Vec<u8> {
    let kind = match width {
        1 => 0,
        2 => 1,
        _ => 2,
    };
    let mut out = vec![0x80 | (tag << 2) | kind];
    let len = u32::try_from(body.len()).expect("short").to_be_bytes();
    out.extend(&len[4 - usize::from(width)..]);
    out.extend(body);
    out
}

/// A new format packet, its length as the shortest form allows.
pub fn new(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![0xc0 | tag];
    let n = body.len();
    if n < 192 {
        out.push(u8::try_from(n).expect("short"));
    } else if n < 8384 {
        let m = n - 192;
        out.push(u8::try_from(m >> 8).expect("short") + 192);
        out.push(u8::try_from(m & 0xff).expect("byte"));
    } else {
        out.push(255);
        out.extend(u32::try_from(n).expect("short").to_be_bytes());
    }
    out.extend(body);
    out
}

pub fn key(body: &[u8]) -> Vec<u8> {
    new(6, body)
}

pub fn subkey(body: &[u8]) -> Vec<u8> {
    new(14, body)
}

/// A valid entry: `name` at `uri`, suites `testing` and `stable`, component `main`, the real
/// keyring and its primary key's fingerprint.
pub fn entry(name: &str) -> Repository {
    Repository {
        name: name.into(),
        kind: "apt".into(),
        uri: "https://repo.example.com/debian".into(),
        suites: vec!["testing".into(), "stable".into()],
        components: vec!["main".into()],
        fingerprints: vec![super::real::PRIMARY.into()],
        keyring: super::real::keyring(),
    }
}

/// A valid entry holding `keyring` and listing `fingerprints`.
pub fn with_keys(name: &str, keyring: Vec<u8>, fingerprints: &[String]) -> Repository {
    Repository {
        fingerprints: fingerprints.to_vec(),
        keyring,
        ..entry(name)
    }
}
