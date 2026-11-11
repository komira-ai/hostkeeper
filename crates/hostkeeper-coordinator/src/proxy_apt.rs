//! The package proxy's apt routes (`docs/design/package-proxy.md`, sections 3 and 4):
//! `GET /apt/<repo>/<path>` and `HEAD` on the MDM listener, which answer a public repository's
//! files from the cache ([`crate::proxy_cache`]) and read the repository's own origin
//! ([`crate::proxy_origin`]) for what it does not hold.
//!
//! **Who may ask what.** No client certificate is asked: the listener carries the name's public
//! certificate, so that apt, which trusts only public roots, can reach it. The guard against an
//! open proxy is the shape of the route, and a request that fails it is refused before any
//! origin is read:
//!
//! - a method other than `GET` and `HEAD` is 405;
//! - a `<repo>` that is not a repository name, or is not an entry of the registry
//!   ([`crate::repositories`]) whose `access` is public, is 404, and so is every route when the
//!   coordinator has no cache directory (`--proxy-cache-dir`);
//! - a `<path>` that is not 1 to 1024 bytes of segments with no empty, `.` or `..` segment and no
//!   `%` other than the escapes of `+` and `~`, which are read as those characters (apt writes a
//!   `+` as `%2b`) (the locator grammar of [`hostkeeper_release::grammar::check_locator`]) is 400.
//!
//! The origin is the entry's `uri` (its base URL, so no setting of origins is needed), and the
//! path under it is the request's own, so a request cannot name another host. The route family is
//! served when the name has a certificate ([`crate::mdm_server`]); until then it answers 503.
//!
//! **What is fresh.** A file under `pool/` is immutable under its name and is kept until evicted.
//! Every other file of the repository (the indexes `Release`, `InRelease`, `Packages*`,
//! `Sources*`, and whatever else is under `dists/`) is served without asking the origin while its
//! age is under `proxy_index_ttl_s`, and after that revalidated with one conditional read
//! ([`crate::proxy_cache::ProxyCache::get_index`]); the origin's 304 renews it.
//!
//! **What the client may ask.**
//!
//! - `If-None-Match` and `If-Modified-Since` are judged against the validators of the answer, and
//!   a match is 304 with no body: for a file inside its TTL that is no origin read at all. A
//!   conditional request of a file not held fills it all the same, and the answer is 304 when the
//!   origin's validators match.
//! - `Range` of one `first-last`, `first-` or `-suffix` span is answered 206 from a stored file,
//!   416 past its end, and on a miss is passed to the origin without being stored (a partial body
//!   is never committed). A suffix span of a file not held, several spans, an invalid span and a
//!   `Range` whose `If-Range` does not match the stored file are ignored: the answer is the whole
//!   file, 200, which a client must accept.
//! - `HEAD` is the headers of the same answer, from the stored file when it is held and, for an
//!   index, fresh, else from one `HEAD` of the origin. Neither stores anything.
//!
//! **Failure.** 404 is the origin's 404; any other failure of the origin before the body begins is
//! 502 `origin_failed`, with one warn line that names the repository, the path and the kind; more
//! reads in flight than `proxy_fetches` is 503 `busy`. A body that breaks off after its headers
//! ends the connection, so a client never takes it for whole. Nothing is held whole in memory.

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt as _;
use hostkeeper_release::grammar::{check_locator, is_repository_name};
use hostkeeper_release::registry;
use http_body_util::{Either, Full, StreamBody};
use hyper::body::Frame;
use hyper::header::{
    ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG, HeaderMap, HeaderName,
    HeaderValue, IF_MODIFIED_SINCE, IF_NONE_MATCH, IF_RANGE, LAST_MODIFIED, RANGE,
};
use hyper::{Method, Response, StatusCode};

use crate::grpc::RouteBody;
use crate::http::error;
use crate::mdm_server::{Family, Refusal};
use crate::origin_https::OriginUrl;
use crate::proxy_cache::{
    BodyStream, ByteRange, FetchError, Fetching, Key, Limits, Meta, ProxyCache, ProxyError,
    Request as Read, is_etag,
};
use crate::service::Coordinator;

/// The path every route of this module is under.
pub(crate) const PREFIX: &str = "/apt/";

/// The directory of a repository whose files never change under their names.
const IMMUTABLE: &str = "pool/";

/// `answer` as a route's.
fn plain(answer: Response<Full<Bytes>>) -> Response<RouteBody> {
    answer.map(Either::Left)
}

/// The answer for a request under [`PREFIX`], or none for any other path: the coordinator's other
/// routes answer it. While the name has no certificate the answer is 503.
pub(crate) async fn route(
    coordinator: &Coordinator,
    method: &Method,
    path: &str,
    headers: &HeaderMap,
) -> Option<Response<RouteBody>> {
    if !path.starts_with(PREFIX) {
        return None;
    }
    let server = coordinator.mdm_server()?;
    if !server.serves(Family::Proxy) {
        return Some(plain(Refusal::Unavailable.answer()));
    }
    Some(serve(coordinator, method, path, headers).await)
}

/// The settings the cache reads, now: their defaults for a store that cannot be read.
fn limits_of(coordinator: &Coordinator) -> Limits {
    match coordinator.mdm_setup().settings() {
        Ok(set) => Limits::from_settings(&set),
        Err(why) => {
            tracing::warn!(%why, "the package proxy could not read its settings; the defaults apply");
            Limits::from_settings(&BTreeMap::new())
        }
    }
}

/// `path` with the escapes of `+` (`%2b`) and `~` (`%7e`), either case, written as the characters.
/// apt writes a `+` of a file name as `%2b`; both are characters the locator grammar allows, so
/// they decode to one name, one cache entry and one origin path. Every other escape stays as it
/// is and is refused by the grammar's `%`, so `%2f`, `%2e` and `%25` cannot reach a path.
fn decode_plain_escapes(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut rest = path;
    while let Some(at) = rest.find('%') {
        out.push_str(&rest[..at]);
        let plain = match rest
            .get(at + 1..at + 3)
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("2b") => Some('+'),
            Some("7e") => Some('~'),
            _ => None,
        };
        out.push(plain.unwrap_or('%'));
        rest = &rest[at + if plain.is_some() { 3 } else { 1 }..];
    }
    out.push_str(rest);
    out
}

/// What a request asks for, once it has passed the route's shape.
struct Wanted<'a> {
    name: &'a str,
    under: &'a str,
}

async fn serve(
    coordinator: &Coordinator,
    method: &Method,
    path: &str,
    headers: &HeaderMap,
) -> Response<RouteBody> {
    if method != Method::GET && method != Method::HEAD {
        return plain(error(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed"));
    }
    let not_found = || plain(error(StatusCode::NOT_FOUND, "not_found"));
    let Some((name, under)) = path
        .strip_prefix(PREFIX)
        .and_then(|rest| rest.split_once('/'))
    else {
        return not_found();
    };
    if !is_repository_name(name) {
        return not_found();
    }
    let decoded = decode_plain_escapes(under);
    let under = decoded.as_str();
    if check_locator(under).is_err() {
        return plain(error(StatusCode::BAD_REQUEST, "bad_request"));
    }
    let uri = coordinator.desired.with_repositories(|registry| {
        registry
            .get(name)
            .map(|kept| &kept.repository)
            .filter(|entry| !registry::is_private(entry))
            .map(|entry| entry.uri.clone())
    });
    // An entry whose URL cannot be read from is not served, like one that is not there.
    let base = uri.as_deref().and_then(|uri| OriginUrl::parse(uri).ok());
    let (Some(uri), Some(base), Some(cache)) = (uri, base, coordinator.proxy_cache()) else {
        return not_found();
    };
    let wanted = Wanted { name, under };
    let Some(client) = coordinator.proxy.origin.as_ref() else {
        return failed(&wanted, &FetchError::Failed("no origin client".to_owned()));
    };
    let limits = limits_of(coordinator);
    let key = Key::new(uri, under);
    if method == Method::HEAD {
        let read = Read {
            key: key.clone(),
            range: None,
            condition: None,
        };
        let (client, redirects) = (Arc::clone(client), limits.redirects);
        let origin_head = Box::pin(async move { client.head(&base, &read, redirects).await });
        return match head_of(cache, &key, &limits, origin_head).await {
            Ok(meta) => respond(headers, meta, None, None),
            Err(why) => refused(&wanted, &why),
        };
    }
    let origin = client.origin(base, limits.redirects);
    let range = range_of(headers, cache.peek(&key).as_ref());
    let answered = if under.starts_with(IMMUTABLE) {
        cache.get(&key, range, &origin, &limits).await
    } else {
        cache.get_index(&key, range, &origin, &limits).await
    };
    match answered {
        Ok(answer) => {
            let meta = Head {
                validator: answer.validator.clone(),
                last_modified: answer.last_modified.clone(),
                length: answer.length,
            };
            respond(headers, meta, answer.range, Some(answer.body))
        }
        Err(why) => refused(&wanted, &why),
    }
}

/// What the headers of an answer say of the file.
struct Head {
    validator: Option<String>,
    last_modified: Option<String>,
    /// The length of the whole file when the answer is a range, else the length of the body.
    length: Option<u64>,
}

/// The headers of a `HEAD`: from the stored file when it is held and not stale, else from
/// `origin`, which is read under a place among the reads in flight.
async fn head_of(
    cache: &ProxyCache,
    key: &Key,
    limits: &Limits,
    origin: Fetching,
) -> Result<Head, ProxyError> {
    let held = cache
        .peek(key)
        .filter(|meta| key.path.starts_with(IMMUTABLE) || !cache.is_stale(meta, limits));
    if let Some(meta) = held {
        return Ok(Head {
            validator: meta.validator,
            last_modified: meta.last_modified,
            length: Some(meta.length),
        });
    }
    let _place = cache.slot(limits)?;
    let fetched = origin.await?;
    Ok(Head {
        validator: fetched.validator,
        last_modified: fetched.last_modified,
        length: fetched.length,
    })
}

/// The text of the header `name`, if the request has it as text.
fn text_of(headers: &HeaderMap, name: HeaderName) -> Option<&str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// `text` as a whole number, if it is nothing but digits.
fn digits(text: &str) -> Option<u64> {
    text.bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| text.parse().ok())
        .flatten()
}

/// The single span of a `Range` header the cache can answer, if the request has one the stored
/// file `held` agrees to (`If-Range`).
fn range_of(headers: &HeaderMap, held: Option<&Meta>) -> Option<ByteRange> {
    let span = text_of(headers, RANGE)?.trim().strip_prefix("bytes=")?;
    if let Some(condition) = text_of(headers, IF_RANGE) {
        let held = held?;
        let strong = held.validator.as_deref().filter(|v| !v.starts_with("W/"));
        let same = strong == Some(condition.trim())
            || held.last_modified.as_deref() == Some(condition.trim());
        if !same {
            return None;
        }
    }
    let (first, last) = span.split_once('-').filter(|_| !span.contains(','))?;
    if first.is_empty() {
        // The last `n` bytes: only of a file held, whose length is known.
        let n = digits(last).filter(|n| *n > 0)?;
        return Some(ByteRange {
            start: held?.length.saturating_sub(n),
            end: None,
        });
    }
    let start = digits(first)?;
    let end = if last.is_empty() {
        None
    } else {
        Some(digits(last)?)
    };
    end.is_none_or(|end| end >= start)
        .then_some(ByteRange { start, end })
}

/// Whether `candidate` (an entity tag as `If-None-Match` lists it) is `etag`, weakly.
fn same_tag(candidate: &str, etag: &str) -> bool {
    let bare = |tag: &str| tag.strip_prefix("W/").unwrap_or(tag).to_owned();
    bare(candidate) == bare(etag)
}

/// Whether the request's `If-None-Match`, or without one its `If-Modified-Since`, says the client
/// holds the file already.
fn client_holds(headers: &HeaderMap, head: &Head) -> bool {
    if let Some(listed) = text_of(headers, IF_NONE_MATCH) {
        let etag = head.validator.as_deref().filter(|v| is_etag(v));
        return listed.trim() == "*"
            || etag.is_some_and(|etag| listed.split(',').any(|c| same_tag(c.trim(), etag)));
    }
    let date = |text: &str| httpdate::parse_http_date(text.trim()).ok();
    match (
        text_of(headers, IF_MODIFIED_SINCE).and_then(date),
        head.last_modified.as_deref().and_then(date),
    ) {
        (Some(since), Some(modified)) => modified <= since,
        _ => false,
    }
}

/// The answer for a file: 304 when the client holds it, 206 for a range, else 200, with `body`
/// (none for a `HEAD`, which keeps the length of the body it would have).
fn respond(
    request: &HeaderMap,
    head: Head,
    range: Option<(u64, u64)>,
    body: Option<BodyStream>,
) -> Response<RouteBody> {
    let mut response = Response::new(Either::Left(Full::new(Bytes::new())));
    let headers = response.headers_mut();
    headers.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    put(
        &mut response,
        ETAG,
        head.validator.as_deref().filter(|v| is_etag(v)),
    );
    put(&mut response, LAST_MODIFIED, head.last_modified.as_deref());
    if client_holds(request, &head) {
        *response.status_mut() = StatusCode::NOT_MODIFIED;
        return response;
    }
    let length = match range {
        Some((first, last)) => {
            *response.status_mut() = StatusCode::PARTIAL_CONTENT;
            let total = head
                .length
                .map_or_else(|| "*".to_owned(), |n| n.to_string());
            put(
                &mut response,
                CONTENT_RANGE,
                Some(&format!("bytes {first}-{last}/{total}")),
            );
            Some(last - first + 1)
        }
        None => head.length,
    };
    put(
        &mut response,
        CONTENT_LENGTH,
        length.map(|n| n.to_string()).as_deref(),
    );
    if let Some(body) = body {
        *response.body_mut() = Either::Right(tonic::body::Body::new(StreamBody::new(
            body.map(|frame| frame.map(Frame::data)),
        )));
    }
    response
}

/// Sets the header `name` of `response` to `value`, when there is one that is a header value.
fn put(response: &mut Response<RouteBody>, name: HeaderName, value: Option<&str>) {
    if let Some(value) = value.and_then(|v| HeaderValue::from_str(v).ok()) {
        response.headers_mut().insert(name, value);
    }
}

/// The answer to a read the cache refused.
fn refused(wanted: &Wanted<'_>, why: &ProxyError) -> Response<RouteBody> {
    match why {
        ProxyError::Busy => plain(error(StatusCode::SERVICE_UNAVAILABLE, "busy")),
        ProxyError::Origin(FetchError::NotFound) => {
            plain(error(StatusCode::NOT_FOUND, "not_found"))
        }
        ProxyError::Origin(why) => failed(wanted, why),
        ProxyError::Unsatisfiable(length) => {
            let mut response = plain(error(
                StatusCode::RANGE_NOT_SATISFIABLE,
                "range_not_satisfiable",
            ));
            put(
                &mut response,
                CONTENT_RANGE,
                Some(&format!("bytes */{length}")),
            );
            response
        }
    }
}

/// 502 for a failure of the origin, with the one line that names it.
fn failed(wanted: &Wanted<'_>, why: &FetchError) -> Response<RouteBody> {
    tracing::warn!(repository = %wanted.name, path = %wanted.under, %why, "the origin of a public repository failed");
    plain(error(StatusCode::BAD_GATEWAY, "origin_failed"))
}

#[cfg(test)]
#[path = "proxy_apt_tests.rs"]
mod tests;
