//! The OAuth 1.0a signature that opens an ABM session (`docs/design/coordinator-mdm.md`, 4.5;
//! measured in `GET /session`, spike abm): HMAC-SHA1 over `GET&enc(url)&enc(sorted params)`, keyed by
//! `enc(consumer_secret)&enc(access_secret)`, every value percent-encoded as RFC 3986 says
//! (only `A-Za-z0-9-._~` stay as they are). The request has no query and no body, so the
//! base string holds the six `oauth_*` parameters alone.

use std::fmt::Write as _;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ring::hmac;

use crate::mdm_secrets::AbmToken;

/// The realm ABM's OAuth takes.
const REALM: &str = "ADM";

/// `text` percent-encoded as RFC 3986 says: only unreserved characters are left as they are.
pub(crate) fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// The `Authorization` header value of `GET url` signed with `token`, at `timestamp` (unix
/// seconds) and with `nonce`.
pub(crate) fn authorization(token: &AbmToken, url: &str, timestamp: u64, nonce: &str) -> String {
    let timestamp = timestamp.to_string();
    // The parameters in the order of their names, which is the order the base string sorts them.
    let params = [
        ("oauth_consumer_key", token.consumer_key.as_str()),
        ("oauth_nonce", nonce),
        ("oauth_signature_method", "HMAC-SHA1"),
        ("oauth_timestamp", timestamp.as_str()),
        ("oauth_token", token.access_token.as_str()),
        ("oauth_version", "1.0"),
    ];
    let normalized = params
        .iter()
        .map(|(name, value)| format!("{name}={}", encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    let base = format!("GET&{}&{}", encode(url), encode(&normalized));
    let key = format!(
        "{}&{}",
        encode(&token.consumer_secret),
        encode(&token.access_secret)
    );
    let key = hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, key.as_bytes());
    let signature = STANDARD.encode(hmac::sign(&key, base.as_bytes()));
    let mut header = format!("OAuth realm=\"{REALM}\"");
    for (name, value) in params {
        let _ = write!(header, ", {name}=\"{}\"", encode(value));
    }
    let _ = write!(header, ", oauth_signature=\"{}\"", encode(&signature));
    header
}

#[cfg(test)]
#[path = "oauth_tests.rs"]
mod tests;
