//! The OAuth 1.0a header of `GET /session` ([`crate::abm::oauth`]).

use crate::abm::fake::sample_token;
use crate::abm::oauth::{authorization, encode};

/// Proves: RFC 3986 encoding leaves the unreserved characters (`A-Za-z0-9-._~`) as they are and
/// writes every other byte, the bytes of a multi-byte character included, as `%` and two upper-case
/// hex digits.
///
/// Catches: `+` for a space, lower-case hex, a `~` that is escaped, and a base string that no
/// longer matches the one Apple checks.
#[test]
fn percent_encoding_keeps_only_the_unreserved_characters() {
    assert_eq!(encode("AZaz09-._~"), "AZaz09-._~");
    assert_eq!(encode("a b/c+d=e&f"), "a%20b%2Fc%2Bd%3De%26f");
    assert_eq!(encode("é"), "%C3%A9");
    assert_eq!(encode(""), "");
}

/// Proves: the header is the realm, the six `oauth_*` parameters in the order the spike measured
/// and the signature, with the signature the HMAC-SHA1 of `GET&enc(url)&enc(params)` under
/// `enc(consumer_secret)&enc(access_secret)`: the exact header for fixed parts, a nonce and a
/// time, whose signature was computed apart from this code (Python's `hmac`, `hashlib`, `base64`
/// over the same base string).
///
/// Catches: a parameter left out or out of order, a secret used unencoded in the key, and a
/// signature over a different base string, each of which Apple answers `signature_invalid`.
#[test]
fn the_header_signs_the_base_string_the_way_abm_checks_it() {
    let header = authorization(
        &sample_token(),
        "https://mdmenrollment.apple.com/session",
        1_700_000_000,
        "0123456789abcdef",
    );
    assert_eq!(
        header,
        "OAuth realm=\"ADM\", oauth_consumer_key=\"CK_abc\", oauth_nonce=\"0123456789abcdef\", \
         oauth_signature_method=\"HMAC-SHA1\", oauth_timestamp=\"1700000000\", \
         oauth_token=\"AT~tok\", oauth_version=\"1.0\", \
         oauth_signature=\"6IMEKB1sC1NUVuN1qCSUyTjtLGU%3D\""
    );
}
