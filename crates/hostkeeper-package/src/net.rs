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
//! [`UreqTransport`] is the transport of a host: a blocking client with rustls, HTTPS only, that
//! follows no redirect, uses no proxy, keeps no cookies, sends no credentials and asks for no
//! compression. Its trusted roots are given to it: the channel's TLS is the web's (design section
//! 5.1), and the bytes are trusted only once their digest and attestation are checked.

use std::io::{self, Read};
use std::sync::Arc;
use std::time::Duration;

use hostkeeper_steps::Clock;
use ureq::tls::{Certificate, RootCerts, TlsConfig, TlsProvider};

use crate::ports::{Fetcher, Got, Limit, NetError, Sink, Url, remaining_secs};

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

/// A response: its status, the length it announced, and its body.
pub struct Response {
    pub status: u16,
    pub content_length: Option<u64>,
    pub body: Box<dyn Read>,
}

/// Sends one GET and returns the response as it came, following no redirect.
pub trait Transport {
    fn open(&self, url: &str, timeouts: Timeouts) -> Result<Response, NetError>;
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

impl<T: Transport> Fetcher for NetFetcher<T> {
    fn get(&self, url: &Url, sink: &mut dyn Sink, limit: Limit) -> Result<Got, NetError> {
        let left = remaining_secs(&limit.deadline, &*self.clock);
        if left == 0 {
            return Err(NetError::DeadlineExceeded);
        }
        let timeouts = Timeouts {
            connect: limit.connect_timeout,
            total: Duration::from_secs(left.min(MAX_TRANSPORT_SECS)),
        };
        let Response {
            status,
            content_length,
            body,
        } = self.transport.open(url.as_str(), timeouts)?;
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
    /// parsed is skipped). The daemon gives it the host's roots.
    pub fn new(roots: &[Vec<u8>]) -> UreqTransport {
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
            .max_redirects(0)
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
}

/// The parts of a `ureq` response the adapter reads.
fn response_of(response: ureq::http::Response<ureq::Body>) -> Response {
    let status = response.status().as_u16();
    let body = response.into_body();
    Response {
        status,
        content_length: body.content_length(),
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
#[path = "net_tls_tests.rs"]
mod tls_tests;
