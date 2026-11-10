//! The real [`Fetcher`]: one HTTPS GET, read in chunks into the sink.
//!
//! [`NetFetcher`] holds the adapter's own rules over a [`Transport`] that sends the request and
//! returns the response head and a body reader: a status other than 2xx is
//! [`NetError::Status`] (a redirect too: no redirect is followed, to another host, another scheme
//! or the same one); the announced length goes to the sink before the first byte; a body that
//! ends before that length is [`NetError::Truncated`]; and the deadline, a point on the injected
//! clock, is turned into the transport's total timeout and checked again before each read. The
//! tests drive it with an in-memory transport.
//!
//! A store's request ([`Fetcher::get_request`]) adds three rules, all in [`NetFetcher`] so that an
//! in-memory transport drives them: a redirect (301, 302, 303, 307, 308) is followed up to the
//! request's count, each target `https://` (a relative `Location` is resolved against the URL it
//! came from; any other scheme is [`NetError::Connect`], and a redirect past the count is the
//! status); the credential is sent to the request's origin only, dropped at the first redirect
//! to another origin and never added back; and a `401` with a `Bearer` challenge is answered when
//! the request allows it, the URL and the realm are on the request's origin and no `Bearer`
//! credential was sent: one GET of the realm with `service` and `scope` (and the `Basic`
//! credential, if any), at most 64 KiB of JSON with a `token` or an `access_token`, then the
//! first request again with `Authorization: Bearer <token>`, once.
//!
//! [`UreqTransport`] is the transport of a host: a blocking client with rustls, HTTPS only, that
//! follows no redirect (or, made with [`UreqTransport::following_redirects`], a bounded number of
//! `https://` redirects), uses no proxy, keeps no cookies, sends no credentials and asks for no
//! compression. Its trusted roots are given to it: the channel's TLS is the web's (design section
//! 5.1), and the bytes are trusted only once their digest and attestation are checked.

use std::io::{self, Read};
use std::sync::Arc;
use std::time::Duration;

use hostkeeper_steps::Clock;
use serde_json::Value;
use ureq::http::Uri;
use ureq::tls::{Certificate, RootCerts, TlsConfig, TlsProvider};

use crate::ports::{Fetcher, Got, Limit, NetError, Request, Sink, Url, origin_of, remaining_secs};

/// The most bytes read from the body at once.
pub const CHUNK_BYTES: usize = 64 * 1024;

/// The longest total timeout handed to the transport: seven days. A deadline further away is
/// still enforced by the checks between reads, and a duration this size cannot overflow the
/// transport's own clock arithmetic.
pub const MAX_TRANSPORT_SECS: u64 = 7 * 24 * 60 * 60;

/// The timeouts of one request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeouts {
    /// The longest the transport waits to connect.
    pub connect: Duration,
    /// The longest the whole request may take, the body included.
    pub total: Duration,
}

/// The most bytes of a token response that are read.
pub const MAX_TOKEN_BYTES: u64 = 64 * 1024;

/// A response: its status, the length it announced, the `Location` and `WWW-Authenticate`
/// header values it carried (when they are text), and its body.
pub struct Response {
    pub status: u16,
    pub content_length: Option<u64>,
    pub location: Option<String>,
    pub www_authenticate: Option<String>,
    pub body: Box<dyn Read>,
}

/// Sends one GET and returns the response as it came, following no redirect.
pub trait Transport {
    fn open(&self, url: &str, timeouts: Timeouts) -> Result<Response, NetError>;

    /// [`Transport::open`] with the value of an `Authorization` header, and a redirect handed
    /// back whatever the transport follows otherwise ([`NetFetcher::get_request`] follows them
    /// itself, to drop the credential at another origin). The default sends no header.
    fn open_with(
        &self,
        url: &str,
        authorization: Option<&str>,
        timeouts: Timeouts,
    ) -> Result<Response, NetError> {
        let _ = authorization;
        self.open(url, timeouts)
    }
}

/// The [`Fetcher`] over a [`Transport`], with the deadline read on `clock`.
pub struct NetFetcher<T> {
    transport: T,
    clock: Arc<dyn Clock>,
}

impl<T: Transport> NetFetcher<T> {
    pub fn new(transport: T, clock: Arc<dyn Clock>) -> NetFetcher<T> {
        NetFetcher { transport, clock }
    }
}

impl<T: Transport> NetFetcher<T> {
    /// The timeouts of a request sent now, or the deadline error when none is left.
    fn timeouts(&self, limit: &Limit) -> Result<Timeouts, NetError> {
        let left = remaining_secs(&limit.deadline, &*self.clock);
        if left == 0 {
            return Err(NetError::DeadlineExceeded);
        }
        Ok(Timeouts {
            connect: limit.connect_timeout,
            total: Duration::from_secs(left.min(MAX_TRANSPORT_SECS)),
        })
    }

    /// Gives a response that is the answer to `sink`: a status other than 2xx is the error, and
    /// otherwise the body is delivered.
    fn deliver(
        &self,
        response: Response,
        sink: &mut dyn Sink,
        limit: &Limit,
    ) -> Result<Got, NetError> {
        let Response {
            status,
            content_length,
            body,
            ..
        } = response;
        if !(200..300).contains(&status) {
            return Err(NetError::Status(status));
        }
        if sink.begin(content_length).is_err() {
            return Ok(Got::Stopped);
        }
        // Nothing past the announced length is read.
        let mut body = body.take(content_length.unwrap_or(u64::MAX));
        let mut buffer = vec![0_u8; CHUNK_BYTES];
        let mut received: u64 = 0;
        loop {
            if remaining_secs(&limit.deadline, &*self.clock) == 0 {
                return Err(NetError::DeadlineExceeded);
            }
            let read = body.read(&mut buffer).map_err(read_error)?;
            let Some(chunk) = buffer.get(..read).filter(|chunk| !chunk.is_empty()) else {
                break;
            };
            received = received.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
            if sink.chunk(chunk).is_err() {
                return Ok(Got::Stopped);
            }
        }
        if content_length.is_some_and(|length| received < length) {
            return Err(NetError::Truncated);
        }
        Ok(Got::Complete)
    }

    /// The token a registry's realm gives for a challenge, asked with `basic` (the `Basic`
    /// credential, if the request has one). Anything but a 2xx with a small JSON body holding a
    /// usable `token` or `access_token` leaves the `401` standing.
    fn token(
        &self,
        realm_url: &str,
        basic: Option<&str>,
        limit: &Limit,
    ) -> Result<String, NetError> {
        let timeouts = self.timeouts(limit)?;
        let response = self.transport.open_with(realm_url, basic, timeouts)?;
        if !(200..300).contains(&response.status) {
            return Err(NetError::Status(401));
        }
        let mut bytes = Vec::new();
        response
            .body
            .take(MAX_TOKEN_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(read_error)?;
        serde_json::from_slice::<Value>(&bytes)
            .ok()
            .filter(|_| u64::try_from(bytes.len()).is_ok_and(|n| n <= MAX_TOKEN_BYTES))
            .and_then(|json| {
                ["token", "access_token"].into_iter().find_map(|key| {
                    json.get(key)?
                        .as_str()
                        .filter(|token| is_header_text(token))
                        .map(str::to_owned)
                })
            })
            .ok_or(NetError::Status(401))
    }
}

impl<T: Transport> Fetcher for NetFetcher<T> {
    fn get(&self, url: &Url, sink: &mut dyn Sink, limit: Limit) -> Result<Got, NetError> {
        let timeouts = self.timeouts(&limit)?;
        let response = self.transport.open(url.as_str(), timeouts)?;
        self.deliver(response, sink, &limit)
    }

    fn get_request(
        &self,
        request: &Request<'_>,
        sink: &mut dyn Sink,
        limit: Limit,
    ) -> Result<Got, NetError> {
        let mut url = request.url.as_str().to_owned();
        let mut authorization = request.authorization.map(str::to_owned);
        let mut redirects = request.redirects;
        let mut challenged = false;
        loop {
            let timeouts = self.timeouts(&limit)?;
            let response = self
                .transport
                .open_with(&url, authorization.as_deref(), timeouts)?;
            let status = response.status;
            if matches!(status, 301 | 302 | 303 | 307 | 308) {
                let location = response
                    .location
                    .as_deref()
                    .filter(|_| redirects > 0)
                    .ok_or(NetError::Status(status))?;
                let next = resolve(&url, location).ok_or(NetError::Connect)?;
                redirects -= 1;
                if origin_of(&next).as_deref() != Some(request.origin) {
                    authorization = None;
                }
                url = next;
                continue;
            }
            let credential = authorization.as_deref();
            let answerable = (status == 401)
                & request.answer_challenge
                & !challenged
                & credential.is_none_or(|value| value.starts_with("Basic "))
                & (origin_of(&url).as_deref() == Some(request.origin));
            let realm = response
                .www_authenticate
                .as_deref()
                .and_then(token_request)
                .filter(|_| answerable)
                .filter(|asked| origin_of(&asked.realm).as_deref() == Some(request.origin));
            let Some(asked) = realm else {
                return self.deliver(response, sink, &limit);
            };
            let token = self.token(&asked.url(), credential, &limit)?;
            authorization = Some(format!("Bearer {token}"));
            challenged = true;
        }
    }
}

/// Whether `text` can be the value of a header: non-empty visible ASCII.
fn is_header_text(text: &str) -> bool {
    !text.is_empty() & text.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

/// What a `Bearer` challenge asks for.
#[derive(Debug, PartialEq, Eq)]
struct TokenRequest {
    realm: String,
    service: Option<String>,
    scope: Option<String>,
}

impl TokenRequest {
    /// The realm with `service` and `scope` as a percent-encoded query.
    fn url(&self) -> String {
        let query: Vec<String> = [("service", &self.service), ("scope", &self.scope)]
            .into_iter()
            .filter_map(|(key, value)| Some(format!("{key}={}", encode(value.as_deref()?))))
            .collect();
        let separator = if self.realm.contains('?') { '&' } else { '?' };
        match query.is_empty() {
            true => self.realm.clone(),
            false => format!("{}{separator}{}", self.realm, query.join("&")),
        }
    }
}

/// Percent-encodes everything but the unreserved characters of RFC 3986.
fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b.is_ascii_alphanumeric() | b"-._~".contains(&b) {
            true => char::from(b).to_string(),
            false => format!("%{b:02X}"),
        })
        .collect()
}

/// The `realm`, `service` and `scope` of a `WWW-Authenticate: Bearer` value; `None` for another
/// scheme or a challenge without a realm. Values are quoted strings (with `\` escapes) or bare
/// tokens, separated by commas.
fn token_request(header: &str) -> Option<TokenRequest> {
    let (scheme, mut rest) = header.trim().split_once(char::is_whitespace)?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let mut found = TokenRequest {
        realm: String::new(),
        service: None,
        scope: None,
    };
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_whitespace() | (c == ','));
        let Some((key, after)) = rest.split_once('=') else {
            break;
        };
        let (value, remainder) = match after.strip_prefix('"') {
            Some(quoted) => quoted_value(quoted),
            None => {
                let (bare, remainder) = after.split_once(',').unwrap_or((after, ""));
                (bare.to_owned(), remainder)
            }
        };
        let value = value.trim().to_owned();
        match key.trim().to_ascii_lowercase().as_str() {
            "realm" => found.realm = value,
            "service" => found.service = Some(value),
            "scope" => found.scope = Some(value),
            _ => {}
        }
        rest = remainder;
    }
    (!found.realm.is_empty()).then_some(found)
}

/// The text of a quoted string whose opening quote is gone, with `\x` read as `x`, and what
/// follows its closing quote.
fn quoted_value(text: &str) -> (String, &str) {
    let mut value = String::new();
    let mut escaped = false;
    for (at, c) in text.char_indices() {
        match (escaped, c) {
            (false, '\\') => escaped = true,
            (false, '"') => return (value, text.get(at + 1..).unwrap_or("")),
            _ => {
                value.push(c);
                escaped = false;
            }
        }
    }
    (value, "")
}

/// The URL a `Location` names, from the URL `base` it came with: an absolute `https://` URL, a
/// scheme-relative one, an absolute path or a path relative to `base`'s directory. The fragment
/// is dropped. `None` when the result is not an `https://` URL.
fn resolve(base: &str, location: &str) -> Option<String> {
    let location = location.split('#').next().unwrap_or_default();
    let scheme_end = location.find("://");
    let absolute = scheme_end.is_some_and(|at| {
        location.get(..at).is_some_and(|scheme| {
            scheme
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() | b"+-.".contains(&b))
        })
    });
    let target = if absolute {
        location.to_owned()
    } else if let Some(rest) = location.strip_prefix("//") {
        format!("https://{rest}")
    } else {
        let uri: Uri = base.parse().ok()?;
        let authority = uri.authority()?;
        let directory = uri.path().rsplit_once('/').map_or("/", |(dir, _)| dir);
        match location.starts_with('/') {
            true => format!("https://{authority}{location}"),
            false => format!("https://{authority}{directory}/{location}"),
        }
    };
    origin_of(&target).map(|_| target)
}

/// What a failed read of the body is.
fn read_error(error: io::Error) -> NetError {
    // `ureq` hands a timeout of the body over as an `io::Error` of kind `Other` that wraps it.
    let wrapped_timeout = error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<ureq::Error>())
        .is_some_and(|inner| matches!(inner, ureq::Error::Timeout(_)));
    if wrapped_timeout {
        return NetError::DeadlineExceeded;
    }
    match error.kind() {
        io::ErrorKind::TimedOut => NetError::DeadlineExceeded,
        io::ErrorKind::UnexpectedEof => NetError::Truncated,
        _ => NetError::Io,
    }
}

/// The [`Transport`] of a host: HTTPS through `ureq` and rustls.
pub struct UreqTransport {
    agent: ureq::Agent,
}

impl UreqTransport {
    /// A client that trusts exactly `roots`, each a DER certificate (a root that cannot be
    /// parsed is skipped), and follows no redirect. The daemon gives it the host's roots.
    pub fn new(roots: &[Vec<u8>]) -> UreqTransport {
        UreqTransport::with_redirects(roots, 0)
    }

    /// [`UreqTransport::new`], but following up to `redirects` redirects, each to an `https://`
    /// URL only (a redirect to another scheme is no connection). The response after the last
    /// redirect it follows is returned as it came, a further redirect included.
    /// `pxe update-hook prepare` downloads release assets with it: a GitHub release download
    /// answers with a redirect to its storage host, and the bytes are trusted only once their
    /// size, SHA-256 or attestation is checked.
    pub fn following_redirects(roots: &[Vec<u8>], redirects: u32) -> UreqTransport {
        UreqTransport::with_redirects(roots, redirects)
    }

    fn with_redirects(roots: &[Vec<u8>], redirects: u32) -> UreqTransport {
        let roots: Vec<Certificate<'static>> = roots
            .iter()
            .map(|der| Certificate::from_der(der).to_owned())
            .collect();
        let tls = TlsConfig::builder()
            .provider(TlsProvider::Rustls)
            .root_certs(RootCerts::new_with_certs(&roots))
            .build();
        let config = ureq::Agent::config_builder()
            .https_only(true)
            .http_status_as_error(false)
            .max_redirects(redirects)
            .max_redirects_will_error(false)
            .proxy(None)
            .tls_config(tls)
            .build();
        UreqTransport {
            agent: ureq::Agent::new_with_config(config),
        }
    }
}

impl Transport for UreqTransport {
    fn open(&self, url: &str, timeouts: Timeouts) -> Result<Response, NetError> {
        self.agent
            .get(url)
            .config()
            .timeout_connect(Some(timeouts.connect))
            .timeout_global(Some(timeouts.total))
            .build()
            .call()
            .map(response_of)
            .map_err(call_error)
    }

    /// Sends the header and follows no redirect, whatever the client was made to do: every 3xx
    /// is returned with its `Location`.
    fn open_with(
        &self,
        url: &str,
        authorization: Option<&str>,
        timeouts: Timeouts,
    ) -> Result<Response, NetError> {
        let mut request = self.agent.get(url);
        if let Some(value) = authorization {
            request = request.header("Authorization", value);
        }
        request
            .config()
            .max_redirects(0)
            .timeout_connect(Some(timeouts.connect))
            .timeout_global(Some(timeouts.total))
            .build()
            .call()
            .map(response_of)
            .map_err(call_error)
    }
}

/// The parts of a `ureq` response the adapter reads.
fn response_of(response: ureq::http::Response<ureq::Body>) -> Response {
    let status = response.status().as_u16();
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    let location = header("location");
    let www_authenticate = header("www-authenticate");
    let body = response.into_body();
    Response {
        status,
        content_length: body.content_length(),
        location,
        www_authenticate,
        body: Box::new(body.into_reader()),
    }
}

/// What a request that got no response is: the deadline when a timeout other than resolving or
/// connecting ran out, and no connection otherwise.
fn call_error(error: ureq::Error) -> NetError {
    match error {
        ureq::Error::Timeout(which)
            if !matches!(which, ureq::Timeout::Resolve | ureq::Timeout::Connect) =>
        {
            NetError::DeadlineExceeded
        }
        _ => NetError::Connect,
    }
}

#[cfg(test)]
#[path = "net_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "net_request_tests.rs"]
mod request_tests;

#[cfg(test)]
#[path = "net_tls_tests.rs"]
mod tls_tests;
