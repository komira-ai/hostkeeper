//! Public keys for the unit tests of the operator keys, made at run time: the SSH string of the
//! type, then bytes a test picks, as `ssh-keygen` lays a blob out. No real key material is kept.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

/// An ed25519 public key whose blob is filled with `fill`, with `comment` after it (none when it
/// is empty).
pub(crate) fn key(fill: u8, comment: &str) -> String {
    let kind = "ssh-ed25519";
    let mut bytes = vec![0, 0, 0, 11];
    bytes.extend_from_slice(kind.as_bytes());
    bytes.extend_from_slice(&[0, 0, 0, 32]);
    bytes.extend_from_slice(&[fill; 32]);
    let blob = STANDARD.encode(bytes);
    match comment {
        "" => format!("{kind} {blob}"),
        _ => format!("{kind} {blob} {comment}"),
    }
}
