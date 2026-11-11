//! The package proxy's read of a public origin (`docs/design/package-proxy.md`, sections 3 and 4):
//! one GET or HEAD over HTTPS that follows redirects, for the cache ([`crate::proxy_cache`]) and
//! the route ([`crate::proxy_apt`]).
//!
//! Unlike the read of a private repository ([`crate::repository_origin`]), which follows none, a
//! public origin may answer with a redirect (a release asset is served from a storage host), so
//! up to `proxy_redirects` of the answers 301, 302, 303, 307 and 308 are followed. A hop goes to
//! an `https://` URL, or to a path on the same host; a redirect to `http://` or to anything else
//! is a failure, and so is one past the limit. The place a request goes to is named by the origin's
//! own answer and never by the client of the proxy, and no header of the client's but the range
//! and the validators is sent on.
//!
//! The origin's certificate must chain to the roots the coordinator is built with. Each read is
//! one connection per hop (`Connection: close`), with a deadline over the connection, the
//! handshake and the headers ([`CONNECT`]) and a deadline on each frame of the body ([`IDLE`]),
//! none over the whole. A body is streamed frame by frame and never held whole. 200 and 206 are a
//! body (a 200 needs a `Content-Length`, or chunks, so that a body cut short is seen as cut), 304
//! answers a conditional read as unchanged, 404 is [`FetchError::NotFound`], and any other status
//! or a failed exchange is [`FetchError::Failed`], whose text names the kind of failure and holds
//! no URL query.

use std::fmt;
use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt as _;
use futures::stream;
use hostkeeper_oidc::{Dial, Roots};
use http_body_util::{BodyStream, Empty};
use hyper::body::Incoming;
use hyper::header::{
    ACCEPT_ENCODING, CONNECTION, CONTENT_LENGTH, CONTENT_RANGE, ETAG, HOST, HeaderValue,
    IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED, LOCATION, RANGE, TRANSFER_ENCODING,
    USER_AGENT,
};
use hyper::{Method, Request as HttpRequest, StatusCode, Uri};
use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;
use tokio_rustls::TlsConnector;

use crate::config::StartError;
use crate::origin_https::{OriginTlsError, OriginUrl, connector, tcp_dial};
use crate::proxy_cache::{
    BodyStream as Frames, FetchError, Fetched, Origin, ProxyCache, Request, Slice,
};
use crate::repository_origin::{CONNECT, Driver, Forwarded};
use crate::service::Coordinator;

/// The longest `Location` followed, in bytes.
const MAX_LOCATION: usize = 4096;

/// Where one hop of a read goes.
#[derive(Clone, Debug)]
struct Target {
    host: String,
    port: u16,
    authority: String,
    server: ServerName<'static>,
    /// The path and the query, starting with `/`.
    path: String,
}

impl Target {
    /// `path` under the origin's base URL.
    fn under(base: &OriginUrl, path: &str) -> Self {
        Self {
            host: base.host().to_owned(),
            port: base.port(),
            authority: base.authority().to_owned(),
            server: base.server(),
            path: base.target(path),
        }
    }

    /// The place a redirect to `location` from the hop `self` goes to.
    fn redirected(&self, location: &str) -> Result<Self, FetchError> {
        let refuse = |why: &str| Err(FetchError::Failed(format!("a redirect {why}")));
        if location.len() > MAX_LOCATION {
            return refuse("is too long");
        }
        if location.starts_with("http://") {
            return refuse("to http was refused");
        }
        if location.starts_with('/') && !location.starts_with("//") {
            return Ok(Self {
                path: location.to_owned(),
                ..self.clone()
            });
        }
        if !location.starts_with("https://") {
            return refuse("is not an https URL or a path");
        }
        let uri: Uri = location
            .parse()
            .map_err(|_| FetchError::Failed("a redirect is not a URL".to_owned()))?;
        let authority = uri.authority().map(ToString::to_string).unwrap_or_default();
        let host = uri.host().unwrap_or_default().to_owned();
        let Ok(server) = ServerName::try_from(host.clone()) else {
            return refuse("names no host a certificate can be checked for");
        };
        if authority.contains('@') {
            return refuse("has a user");
        }
        Ok(Self {
            host,
            port: uri.port_u16().unwrap_or(443),
            authority,
            server,
            path: uri
                .path_and_query()
                .map_or_else(|| "/".to_owned(), ToString::to_string),
        })
    }
}

/// What a coordinator keeps of the package proxy: its cache and its reads of origins.
#[derive(Default)]
pub(crate) struct Proxy {
    /// The cache (`--proxy-cache-dir`), when there is one.
    pub(crate) cache: Option<Arc<ProxyCache>>,
    /// The reads of public origins.
    pub(crate) origin: Option<Arc<ProxyOrigin>>,
    /// The reader of the origins of pkgs, for the pull-through of `/pkg/`: with the cache only.
    pub(crate) pkg: Option<Arc<crate::pkg_origin::PkgOrigin>>,
}

impl Proxy {
    /// The proxy at start: the cache `--proxy-cache-dir` names (none without the flag), under the
    /// limits of the settings in `setup`, and the origin client trusting `roots`.
    ///
    /// # Errors
    /// [`StartError`]: the cache cannot be opened, the settings cannot be read, or the roots hold
    /// no usable certificate.
    pub(crate) fn start(
        dir: Option<&std::path::Path>,
        setup: &crate::mdm_setup::MdmSetup,
        roots: Roots,
    ) -> Result<Self, StartError> {
        let cache = crate::proxy_cache::open_at_start(dir, setup)?.map(Arc::new);
        let pkg = cache
            .as_ref()
            .map(|_| crate::pkg_origin::PkgOrigin::new(roots.clone()).map(Arc::new))
            .transpose()?;
        Ok(Self {
            cache,
            origin: Some(Arc::new(ProxyOrigin::new(roots)?)),
            pkg,
        })
    }
}

/// Reads public origins over HTTPS, following redirects.
pub struct ProxyOrigin {
    connector: TlsConnector,
    dial: Dial,
}

impl fmt::Debug for ProxyOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProxyOrigin").finish_non_exhaustive()
    }
}

impl ProxyOrigin {
    /// An origin client trusting `roots`, dialling TCP.
    ///
    /// # Errors
    /// [`OriginTlsError::NoRoots`]: `roots` holds no usable certificate;
    /// [`OriginTlsError::Tls`]: the TLS client configuration is refused.
    pub fn new(roots: Roots) -> Result<Self, OriginTlsError> {
        Ok(Self {
            connector: connector(roots)?,
            dial: tcp_dial(),
        })
    }

    /// The same client dialling through `dial` instead of TCP (tests).
    #[must_use]
    pub fn with_dial(mut self, dial: Dial) -> Self {
        self.dial = dial;
        self
    }

    /// The reads of the cache ([`Origin`]) for the repository at `base`, following up to
    /// `redirects` redirects.
    pub(crate) fn origin(self: &Arc<Self>, base: OriginUrl, redirects: u64) -> Origin {
        let client = Arc::clone(self);
        Arc::new(move |request| {
            let (client, base) = (Arc::clone(&client), base.clone());
            Box::pin(async move { client.read(&base, &request, redirects, Method::GET).await })
        })
    }

    /// The headers the origin answers to a `HEAD` of `path` under `base`, as a [`Fetched`] with no
    /// body.
    ///
    /// # Errors
    /// [`FetchError`].
    pub(crate) async fn head(
        &self,
        base: &OriginUrl,
        request: &Request,
        redirects: u64,
    ) -> Result<Fetched, FetchError> {
        self.read(base, request, redirects, Method::HEAD).await
    }

    /// One read of `request.key.path` under `base`: the hops, then the answer.
    async fn read(
        &self,
        base: &OriginUrl,
        request: &Request,
        redirects: u64,
        method: Method,
    ) -> Result<Fetched, FetchError> {
        let mut target = Target::under(base, &request.key.path);
        let mut hops = 0;
        loop {
            let (response, driver) =
                tokio::time::timeout(CONNECT, self.exchange(&target, method.clone(), request))
                    .await
                    .map_err(|_| FetchError::Failed("the connection timed out".to_owned()))??;
            let status = response.status();
            if matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308) {
                if hops >= redirects {
                    return Err(FetchError::Failed("too many redirects".to_owned()));
                }
                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| FetchError::Failed("a redirect has no Location".to_owned()))?;
                target = target.redirected(location)?;
                hops += 1;
                continue;
            }
            return answer(response, driver, request, method == Method::HEAD);
        }
    }

    /// The connection, the handshake, the request and the answer's headers.
    async fn exchange(
        &self,
        target: &Target,
        method: Method,
        request: &Request,
    ) -> Result<(hyper::Response<Incoming>, Driver), FetchError> {
        let broken = |what: &str| FetchError::Failed(format!("the {what} failed"));
        let exchange = |_: hyper::Error| broken("HTTP exchange");
        let tcp = (self.dial)(target.host.clone(), target.port)
            .await
            .map_err(|_| broken("connection"))?;
        let tls = self
            .connector
            .connect(target.server.clone(), tcp)
            .await
            .map_err(|_| broken("TLS handshake"))?;
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake::<_, Empty<Bytes>>(TokioIo::new(tls))
                .await
                .map_err(exchange)?;
        let driver = Driver(tokio::spawn(async move {
            let _ = connection.await;
        }));
        let mut builder = HttpRequest::builder()
            .method(method)
            .uri(target.path.as_str())
            .header(HOST, target.authority.as_str())
            .header(USER_AGENT, "hostkeeper-coordinator")
            .header(CONNECTION, "close")
            .header(ACCEPT_ENCODING, "identity");
        if let Some(range) = request.range {
            let text = range.end.map_or_else(
                || format!("bytes={}-", range.start),
                |end| format!("bytes={}-{end}", range.start),
            );
            builder = builder.header(RANGE, text);
        }
        if let Some(condition) = &request.condition {
            if let Some(etag) = &condition.etag {
                builder = builder.header(IF_NONE_MATCH, header(etag)?);
            }
            if let Some(date) = &condition.last_modified {
                builder = builder.header(IF_MODIFIED_SINCE, header(date)?);
            }
        }
        let http = builder
            .body(Empty::<Bytes>::new())
            .map_err(|_| broken("request"))?;
        let response = sender.send_request(http).await.map_err(exchange)?;
        Ok((response, driver))
    }
}

/// `text` as a header value.
fn header(text: &str) -> Result<HeaderValue, FetchError> {
    HeaderValue::from_str(text)
        .map_err(|_| FetchError::Failed("a validator is not a header value".to_owned()))
}

/// The text of the header `name` of `response`, if it has one.
fn text_of<'a>(
    response: &'a hyper::Response<Incoming>,
    name: &hyper::header::HeaderName,
) -> Option<&'a str> {
    response.headers().get(name).and_then(|v| v.to_str().ok())
}

/// The `Fetched` an answer is, or why it is none.
fn answer(
    response: hyper::Response<Incoming>,
    driver: Driver,
    request: &Request,
    head: bool,
) -> Result<Fetched, FetchError> {
    let status = response.status();
    let validator = text_of(&response, &ETAG)
        .or_else(|| text_of(&response, &LAST_MODIFIED))
        .map(str::to_owned);
    let last_modified = text_of(&response, &LAST_MODIFIED).map(str::to_owned);
    let none = |unchanged| Fetched {
        validator: validator.clone(),
        last_modified: last_modified.clone(),
        unchanged,
        slice: None,
        length: None,
        body: Box::pin(stream::empty()),
    };
    match status {
        StatusCode::NOT_MODIFIED if request.condition.is_some() => return Ok(none(true)),
        StatusCode::NOT_FOUND => return Err(FetchError::NotFound),
        StatusCode::OK | StatusCode::PARTIAL_CONTENT => {}
        other => {
            return Err(FetchError::Failed(format!(
                "the origin answered {}",
                other.as_u16()
            )));
        }
    }
    let length = text_of(&response, &CONTENT_LENGTH).and_then(|v| v.parse::<u64>().ok());
    let chunked = text_of(&response, &TRANSFER_ENCODING)
        .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
    if length.is_none() && !chunked && !head {
        return Err(FetchError::Failed(
            "the origin gave neither a length nor chunks".to_owned(),
        ));
    }
    let slice = if status == StatusCode::PARTIAL_CONTENT {
        let slice = text_of(&response, &CONTENT_RANGE).and_then(slice_of);
        Some(slice.ok_or_else(|| {
            FetchError::Failed("the origin gave a 206 without a usable Content-Range".to_owned())
        })?)
    } else {
        None
    };
    let body: Frames = if head {
        drop(driver);
        Box::pin(stream::empty())
    } else {
        let forwarded = Forwarded::new(response.into_body(), driver);
        Box::pin(BodyStream::new(forwarded).filter_map(|frame| async move {
            match frame {
                Ok(frame) => frame.into_data().ok().map(Ok),
                Err(why) => Some(Err(FetchError::Failed(why.to_string()))),
            }
        }))
    };
    Ok(Fetched {
        validator,
        last_modified,
        unchanged: false,
        slice,
        length,
        body,
    })
}

/// The slice a `Content-Range` of `bytes first-last/total` (or `/*`) names.
fn slice_of(text: &str) -> Option<Slice> {
    let (span, total) = text.strip_prefix("bytes ")?.split_once('/')?;
    let (start, end) = span.split_once('-')?;
    let (start, end) = (start.parse::<u64>().ok()?, end.parse::<u64>().ok()?);
    let total = match total {
        "*" => None,
        total => Some(total.parse::<u64>().ok()?),
    };
    (start <= end).then_some(Slice { start, end, total })
}

impl Coordinator {
    /// The same coordinator with the package proxy `proxy` (`Proxy::start`).
    #[must_use]
    pub(crate) fn with_proxy(mut self, proxy: Proxy) -> Self {
        self.proxy = proxy;
        self
    }

    /// The same coordinator reading public origins for the package proxy with `origin`; without
    /// one, the proxy answers 502 ([`crate::proxy_apt`]).
    #[must_use]
    pub fn with_proxy_origin(mut self, origin: ProxyOrigin) -> Self {
        self.proxy.origin = Some(Arc::new(origin));
        self
    }
}

#[cfg(test)]
#[path = "proxy_origin_tests.rs"]
mod tests;
