//! An `https://` URL, parsed for its parts and nothing else (no network here).

use std::fmt;

use crate::dns;

/// An `https://` URL: a host name, an optional port, and a path with an optional query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpsUrl {
    url: String,
    host: String,
    port: u16,
    path: String,
}

/// Why a string is not an [`HttpsUrl`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HttpsUrlError {
    /// Another scheme, or none.
    #[error("{0:?} is not an https:// URL")]
    Scheme(String),
    /// A space or another whitespace or control character, which no URL holds as it is.
    #[error("{0:?} holds whitespace or a control character")]
    Space(String),
    /// A fragment (`#...`), which is never sent to the server.
    #[error("{0:?} has a fragment (#)")]
    Fragment(String),
    /// No host, a host that is not a host name, or a user part.
    #[error(
        "{0:?} names no host name (letters, digits and -, at most 63 characters a label and 253 in all), or names a user"
    )]
    Host(String),
    /// A port that is not a number from 1 to 65535.
    #[error("{0:?} has a port that is not a number from 1 to 65535")]
    Port(String),
}

impl HttpsUrl {
    /// `url` as an [`HttpsUrl`], or why it is not one.
    ///
    /// The authority (`host` or `host:port`) ends at the first `/` or `?`, so a query right
    /// after it (`https://example.com?x=1`) is the path `/?x=1`, not part of the host. The
    /// host is a host name (or a dotted IPv4 address, whose labels are digits) in either
    /// case.
    pub fn new(url: &str) -> Result<HttpsUrl, HttpsUrlError> {
        let Some(rest) = url.strip_prefix("https://") else {
            return Err(HttpsUrlError::Scheme(url.to_owned()));
        };
        if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(HttpsUrlError::Space(url.to_owned()));
        }
        if url.contains('#') {
            return Err(HttpsUrlError::Fragment(url.to_owned()));
        }
        let (authority, tail) = rest.split_at(rest.find(['/', '?']).unwrap_or(rest.len()));
        let path = match tail.chars().next() {
            None => "/".to_owned(),
            Some('?') => format!("/{tail}"),
            Some(_) => tail.to_owned(),
        };
        // No user part (`https://user:key@mdm.example.com/`): it would be printed with the
        // URL. No IPv6 literal (`[...]`) either.
        if authority.contains(['@', '[', ']']) {
            return Err(HttpsUrlError::Host(url.to_owned()));
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => {
                // Digits only: `u16` would also take a sign (`+443`).
                let digits = port.bytes().all(|b| b.is_ascii_digit());
                let port = port.parse::<u16>().ok().filter(|p| digits && *p != 0);
                let Some(port) = port else {
                    return Err(HttpsUrlError::Port(url.to_owned()));
                };
                (host, port)
            }
            None => (authority, 443),
        };
        if !dns::is_host_name(host, dns::Case::Any, 1) {
            return Err(HttpsUrlError::Host(url.to_owned()));
        }
        Ok(HttpsUrl {
            url: url.to_owned(),
            host: host.to_owned(),
            port,
            path,
        })
    }

    /// The host.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The port, 443 when the URL names none.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The path and query, `/` when the URL has neither, and `/` before a query that
    /// follows the authority directly.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The last part of the path, without the query.
    pub fn file_name(&self) -> &str {
        let path = self.path.split('?').next().unwrap_or_default();
        path.rsplit('/').next().unwrap_or_default()
    }
}

impl fmt::Display for HttpsUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.url)
    }
}
