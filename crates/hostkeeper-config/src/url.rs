//! The URLs the two files hold, checked without a URL library: a scheme, a host, an optional
//! port and an optional path, each in a small grammar. Nothing here resolves or fetches.

use std::net::Ipv6Addr;

/// The placeholder the manifest's health URL may put in the port's place.
pub(crate) const PORT_PLACEHOLDER: &str = "{port}";

/// A URL taken apart.
pub(crate) struct Parts<'a> {
    pub scheme: &'a str,
    pub host: &'a str,
    /// The text after `:` in the authority, if any: digits, or [`PORT_PLACEHOLDER`].
    pub port: Option<&'a str>,
}

/// A host: a DNS name or an IPv4 literal (letters, digits, `.` and `-`), or an IPv6 address in
/// brackets that parses as one.
fn is_host(host: &str) -> bool {
    let named = !host.is_empty()
        & host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() | b".-".contains(&b));
    let bracketed = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .is_some_and(|inner| inner.parse::<Ipv6Addr>().is_ok());
    named | bracketed
}

/// A port: 1 to 65535 in digits, or the placeholder when `placeholder` allows it.
fn is_port(port: &str, placeholder: bool) -> bool {
    let number =
        port.bytes().all(|b| b.is_ascii_digit()) & port.parse::<u16>().is_ok_and(|port| port != 0);
    number | (placeholder & (port == PORT_PLACEHOLDER))
}

/// The characters of a path (RFC 3986 `path-abempty`): `/`, the unreserved characters, the
/// sub-delimiters, `:` and `@`, and `%` only as the start of an escape of two hex digits. No
/// query, fragment, white space, `{` or `}`.
fn is_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    let escapes = path.match_indices('%').all(|(at, _)| {
        bytes
            .get(at + 1..at + 3)
            .is_some_and(|hex| hex.iter().all(u8::is_ascii_hexdigit))
    });
    let chars = path
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() | b"/-._~!$&'()*+,;=:@%".contains(&b));
    escapes & chars
}

/// Takes `text` apart as `<scheme>://<host>[:<port>][/<path>]`, or says why it is not one. No
/// user name, query or fragment is accepted. `placeholder` lets the port be [`PORT_PLACEHOLDER`].
pub(crate) fn parts(text: &str, placeholder: bool) -> Result<Parts<'_>, &'static str> {
    let (scheme, rest) = text.split_once("://").ok_or("has no `://`")?;
    let (authority, path) = rest.find('/').map_or((rest, ""), |at| rest.split_at(at));
    // The port follows the last `:` outside an IPv6 literal's brackets.
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !port.contains(']') => (host, Some(port)),
        _ => (authority, None),
    };
    if !is_host(host) {
        return Err("has no host, or a host outside letters, digits, `.`, `-` and an IPv6 literal");
    }
    if !port.is_none_or(|port| is_port(port, placeholder)) {
        return Err("has a port that is not 1 to 65535");
    }
    if !is_path(path) {
        return Err(
            "has a path outside the characters of a URL path or a `%` without two hex digits, or a query or fragment",
        );
    }
    Ok(Parts { scheme, host, port })
}
