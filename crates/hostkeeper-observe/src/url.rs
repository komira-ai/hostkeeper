//! [`HealthUrl`]: a health URL the probe may ask, checked once when it is made.
//!
//! The scheme is `http` and the host is a loopback literal, `127.0.0.1` or `[::1]` (any spelling
//! of `::1` inside the brackets), never a name: nothing is resolved, so no DNS answer can send the
//! probe elsewhere. The same two addresses the manifest loader accepts, and no others:
//!
//! - `localhost` is a name, and names are not resolved;
//! - `127.0.0.2` and the rest of `127.0.0.0/8` are loopback too, but the design names
//!   `127.0.0.1` alone, and a URL the loader refused never reaches the probe;
//! - `0.0.0.0` is the unspecified address, not a loopback one;
//! - `[::ffff:127.0.0.1]` is an IPv4 address written as IPv6, so it is not `::1`.
//!
//! No user name or password. The port is explicit or 80. The path and query are kept as written,
//! in the characters of a URL path and query (no space, control character, `#` or byte above
//! ASCII), so the request line the probe writes cannot be split.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// Why a text is not a [`HealthUrl`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum UrlRefusal {
    /// It does not start with `http://`.
    #[error("the health URL is not http://")]
    Scheme,
    /// It has a user name or password (`user@`).
    #[error("the health URL has a user name")]
    Userinfo,
    /// Its host is not `127.0.0.1` or `[::1]`.
    #[error("the health URL's host is not 127.0.0.1 or [::1]")]
    Host,
    /// Its port is not a number from 1 to 65535.
    #[error("the health URL's port is not a number from 1 to 65535")]
    Port,
    /// Its path or query holds a character a request line may not.
    #[error("the health URL's path or query holds a character it may not")]
    Path,
}

impl UrlRefusal {
    /// The refusal as a stable token.
    pub fn token(self) -> &'static str {
        match self {
            UrlRefusal::Scheme => "scheme",
            UrlRefusal::Userinfo => "userinfo",
            UrlRefusal::Host => "host",
            UrlRefusal::Port => "port",
            UrlRefusal::Path => "path",
        }
    }
}

/// A health URL that passed every check of this module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HealthUrl {
    text: String,
    host: String,
    addr: SocketAddr,
    target: String,
}

impl HealthUrl {
    /// Checks `text`, or says which rule it breaks.
    pub fn parse(text: &str) -> Result<HealthUrl, UrlRefusal> {
        let rest = text.strip_prefix("http://").ok_or(UrlRefusal::Scheme)?;
        let split = rest.find(['/', '?']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(split);
        if authority.contains('@') {
            return Err(UrlRefusal::Userinfo);
        }
        // The port follows the last `:` outside an IPv6 literal's brackets.
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) if !port.contains(']') => (host, Some(port)),
            _ => (authority, None),
        };
        let ip = loopback(host).ok_or(UrlRefusal::Host)?;
        let port = port.map_or(Some(80), parse_port).ok_or(UrlRefusal::Port)?;
        let path_ok = tail
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() | b"-._~!$&'()*+,;=:@/%?".contains(&b));
        if !path_ok {
            return Err(UrlRefusal::Path);
        }
        let target = if tail.starts_with('/') {
            tail.to_owned()
        } else {
            format!("/{tail}")
        };
        Ok(HealthUrl {
            text: text.to_owned(),
            host: host.to_owned(),
            addr: SocketAddr::new(ip, port),
            target,
        })
    }

    /// The address the probe connects to.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The path and query the request asks for: `/` when the URL has none.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// The whole request the probe writes: a GET of the target, closing the connection after
    /// the answer.
    pub fn request(&self) -> String {
        format!(
            "GET {} HTTP/1.1\r\nHost: {}:{}\r\nUser-Agent: hostkeeper\r\nAccept: */*\r\nConnection: close\r\n\r\n",
            self.target,
            self.host,
            self.addr.port()
        )
    }
}

impl fmt::Display for HealthUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

/// `host` as an address if it is `127.0.0.1`, or `::1` in brackets.
fn loopback(host: &str) -> Option<IpAddr> {
    match host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        Some(inner) => inner
            .parse::<Ipv6Addr>()
            .ok()
            .filter(|ip| *ip == Ipv6Addr::LOCALHOST)
            .map(IpAddr::V6),
        None => host
            .parse::<Ipv4Addr>()
            .ok()
            .filter(|ip| *ip == Ipv4Addr::LOCALHOST)
            .map(IpAddr::V4),
    }
}

/// A port of digits only, 1 to 65535.
fn parse_port(port: &str) -> Option<u16> {
    let digits = port.bytes().all(|b| b.is_ascii_digit());
    port.parse::<u16>().ok().filter(|p| digits & (*p != 0))
}
