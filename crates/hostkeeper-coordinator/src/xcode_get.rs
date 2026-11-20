//! The route that serves a host an Xcode archive of the store (docs/design/maintenance-updates.md,
//! sections 2.0 and 2.2): `GET /v1/xcode/<sha256>` on the heartbeat surface
//! ([`crate::xcode_store::PREFIX`]), which the agent listener and the LAN listener both serve. The
//! bytes are the archive exactly as the operator pushed it, read from the file of the store, never
//! held whole in memory.
//!
//! The caller is a host: its client certificate names one, as for a heartbeat. No certificate or
//! one that names no host is 403 `forbidden`, a denied or retiring host 403 `denied`, a method
//! other than `GET` 405 `method_not_allowed`. Only a host of a host group whose `xcode_keep` is at
//! least 1 reads the store: a host in no group, or in a group that keeps no Xcode, is 403
//! `forbidden`, with a warn line that names the node id. A `<sha256>` that is not 64 lowercase hex
//! characters, or that the store does not hold, is 404 `not_found`; a store that cannot be read is
//! 500 `store_failed`, logged.
//!
//! A held archive is answered 200 with `Accept-Ranges: bytes`, `ETag: "<sha256>"`,
//! `Content-Type: application/octet-stream` and the `Content-Length` of the file as it is on disk.
//! A copy that broke can be resumed: `Range: bytes=<first>-<last>`, `bytes=<first>-` or
//! `bytes=-<count>` (one range, decimal numbers, no space) is answered 206 with
//! `Content-Range: bytes <first>-<last>/<size>` and exactly those bytes; a `<last>` past the end is
//! taken as the last byte. A range that starts at or past the end of the file, a suffix of no bytes
//! and any range of an empty file are 416 with `Content-Range: bytes */<size>` and no body. A
//! `Range` the route does not understand (several ranges, a unit other than `bytes`, a first
//! position past the last, text that is not a range) is ignored, as RFC 9110 lets a server do, and
//! the whole file is answered; so is a `Range` that comes with an `If-Range` that is not the
//! `ETag` (the archive's bytes are named by their digest, so the tag is the only validator).
//!
//! The body is read from the file in chunks of at most 64 KiB as the connection takes them. It
//! keeps the connection's hold ([`crate::wait::ConnectionHold`]) until it ends or is dropped, so
//! an HTTP/2 connection is not cut in the middle of a copy; a read that fails ends the body with
//! an error and is logged.

use std::io::{self, SeekFrom};

use bytes::Bytes;
use futures::stream;
use hostkeeper_release::grammar::is_sha256;
use http_body_util::{Either, Full, StreamBody};
use hyper::body::Frame;
use hyper::header::{
    ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG, IF_RANGE, RANGE,
};
use hyper::http::response::Builder;
use hyper::{HeaderMap, Method, Request, Response, StatusCode};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::enrollment::Admission;
use crate::grpc::RouteBody;
use crate::http::{DENIED, Peer, error};
use crate::identity::Identity;
use crate::service::Coordinator;
use crate::wait::{ConnectionHold, ConnectionWaits};
use crate::xcode_store::{PREFIX, XcodeStore};

/// The most bytes of the file read, and given to the connection, at a time.
const CHUNK: u64 = 64 * 1024;

/// What the `Range` of a request asks of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Spec {
    /// `bytes=<first>-`: from `first` to the end.
    From(u64),
    /// `bytes=<first>-<last>`.
    Between(u64, u64),
    /// `bytes=-<count>`: the last `count` bytes.
    Suffix(u64),
}

/// What a file of a known size is answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Asked {
    /// The whole file: no range, or one that is ignored.
    Whole,
    /// The bytes `first` to `last`, both included.
    Part { first: u64, last: u64 },
    /// No byte of the file is in the range.
    Unsatisfiable,
}

/// `digits` as a number: none for an empty text, a character that is not a decimal digit, or a
/// number that does not fit.
fn number(digits: &str) -> Option<u64> {
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// The range `text` (the value of a `Range` header) names: none unless it is `bytes=` and exactly
/// one of `<first>-`, `<first>-<last>` with `first` not past `last`, and `-<count>`.
fn parse(text: &str) -> Option<Spec> {
    let (first, last) = text.strip_prefix("bytes=")?.split_once('-')?;
    match (number(first), number(last)) {
        (Some(first), None) if last.is_empty() => Some(Spec::From(first)),
        (Some(first), Some(last)) if first <= last => Some(Spec::Between(first, last)),
        (None, Some(count)) if first.is_empty() => Some(Spec::Suffix(count)),
        _ => None,
    }
}

/// What `spec` asks of a file of `size` bytes.
fn resolve(spec: Spec, size: u64) -> Asked {
    let Some(end) = size.checked_sub(1) else {
        return Asked::Unsatisfiable;
    };
    match spec {
        Spec::From(first) if first <= end => Asked::Part { first, last: end },
        Spec::Between(first, last) if first <= end => Asked::Part {
            first,
            last: last.min(end),
        },
        Spec::Suffix(count) if count > 0 => Asked::Part {
            first: size.saturating_sub(count),
            last: end,
        },
        _ => Asked::Unsatisfiable,
    }
}

/// What the headers of a request ask of a file of `size` bytes whose tag is `etag`: the whole file
/// unless exactly one `Range` is there that is understood and no `If-Range` names another tag.
fn asked(headers: &HeaderMap, etag: &str, size: u64) -> Asked {
    if headers
        .get(IF_RANGE)
        .is_some_and(|tag| tag.to_str().ok() != Some(etag))
    {
        return Asked::Whole;
    }
    let mut ranges = headers.get_all(RANGE).iter();
    let (Some(value), None) = (ranges.next(), ranges.next()) else {
        return Asked::Whole;
    };
    value
        .to_str()
        .ok()
        .and_then(parse)
        .map_or(Asked::Whole, |spec| resolve(spec, size))
}

/// `answer` as a route's.
fn plain(answer: Response<Full<Bytes>>) -> Response<RouteBody> {
    answer.map(Either::Left)
}

/// The refusal `status` with the code `code`, as a route's answer.
fn refused(status: StatusCode, code: &'static str) -> Response<RouteBody> {
    plain(error(status, code))
}

/// The answer made of `builder` and `body`; a header that cannot be made is a 500.
fn built(builder: Builder, body: RouteBody) -> Response<RouteBody> {
    builder
        .body(body)
        .unwrap_or_else(|_| refused(StatusCode::INTERNAL_SERVER_ERROR, "store_failed"))
}

/// The host the certificate of `peer` names, if it may read; else the code of the 403.
fn caller(coordinator: &Coordinator, peer: &Peer) -> Result<Identity, &'static str> {
    let Some(identity) = peer
        .as_deref()
        .and_then(|der| coordinator.identify(der).ok())
    else {
        tracing::warn!("refused a read of the Xcode store: no certificate that names a host");
        return Err("forbidden");
    };
    if coordinator.admission(&identity) != Admission::Admitted {
        tracing::warn!(node_id = %identity.host_id, "refused a read of the Xcode store: the host is denied");
        return Err(DENIED);
    }
    Ok(identity)
}

/// The file being read: where it stands and how much of it is left to give.
struct Reading {
    file: File,
    left: u64,
    /// Keeps the connection while the body is read.
    _hold: Option<ConnectionHold>,
}

/// The body of `reading`: its next bytes, at most [`CHUNK`] a frame, until none is left. A read
/// that fails, or a file that ends early, ends the body with an error.
fn body_of(reading: Reading) -> RouteBody {
    let frames = stream::unfold(reading, |mut reading| async move {
        if reading.left == 0 {
            return None;
        }
        let mut chunk = vec![0_u8; usize::try_from(reading.left.min(CHUNK)).unwrap_or(0)];
        let read = match reading.file.read(&mut chunk).await {
            Ok(0) => Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
            other => other,
        };
        match read {
            Ok(count) => {
                chunk.truncate(count);
                reading.left = reading
                    .left
                    .saturating_sub(u64::try_from(count).unwrap_or(u64::MAX));
                Some((Ok(Frame::data(Bytes::from(chunk))), reading))
            }
            Err(why) => {
                tracing::warn!(%why, "an Xcode archive could not be read to its end");
                reading.left = 0;
                Some((Err(why), reading))
            }
        }
    });
    Either::Right(tonic::body::Body::new(StreamBody::new(frames)))
}

/// Answers the `GET` or other request for a path under [`PREFIX`] from `peer`.
pub(crate) async fn answer<B>(
    coordinator: &Coordinator,
    store: &XcodeStore,
    peer: &Peer,
    request: Request<B>,
) -> Response<RouteBody> {
    let (parts, _) = request.into_parts();
    if parts.method != Method::GET {
        return refused(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed");
    }
    let identity = match caller(coordinator, peer) {
        Ok(identity) => identity,
        Err(code) => return refused(StatusCode::FORBIDDEN, code),
    };
    if coordinator.xcode_keep_of(&identity.host_id) == 0 {
        tracing::warn!(node_id = %identity.host_id, "refused a read of the Xcode store: the host's group keeps no Xcode");
        return refused(StatusCode::FORBIDDEN, "forbidden");
    }
    let Some(sha256) = parts
        .uri
        .path()
        .strip_prefix(PREFIX)
        .filter(|rest| is_sha256(rest))
    else {
        return refused(StatusCode::NOT_FOUND, "not_found");
    };
    match store.archive(sha256) {
        Ok(Some(_)) => {}
        Ok(None) => return refused(StatusCode::NOT_FOUND, "not_found"),
        Err(why) => {
            tracing::error!(%why, "the Xcode store could not be read");
            return refused(StatusCode::INTERNAL_SERVER_ERROR, "store_failed");
        }
    }
    let hold = parts
        .extensions
        .get::<ConnectionWaits>()
        .map(ConnectionWaits::hold);
    serve(store, sha256, &parts.headers, hold).await
}

/// The answer to a read of the archive `sha256`, whose record the store holds, by `headers`.
async fn serve(
    store: &XcodeStore,
    sha256: &str,
    headers: &HeaderMap,
    hold: Option<ConnectionHold>,
) -> Response<RouteBody> {
    let path = store.archive_path(sha256);
    let opened = async {
        let file = File::open(&path).await?;
        let size = file.metadata().await?.len();
        Ok::<_, io::Error>((file, size))
    };
    let (mut file, size) = match opened.await {
        Ok(opened) => opened,
        Err(why) if why.kind() == io::ErrorKind::NotFound => {
            return refused(StatusCode::NOT_FOUND, "not_found");
        }
        Err(why) => {
            tracing::error!(%why, path = %path.display(), "an Xcode archive could not be opened");
            return refused(StatusCode::INTERNAL_SERVER_ERROR, "store_failed");
        }
    };
    let etag = format!("\"{sha256}\"");
    let (status, first, length, range) = match asked(headers, &etag, size) {
        Asked::Whole => (StatusCode::OK, 0, size, None),
        Asked::Part { first, last } => (
            StatusCode::PARTIAL_CONTENT,
            first,
            last - first + 1,
            Some(format!("bytes {first}-{last}/{size}")),
        ),
        Asked::Unsatisfiable => {
            return built(
                Response::builder()
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(CONTENT_RANGE, format!("bytes */{size}"))
                    .header(CONTENT_LENGTH, 0_u64),
                Either::Left(Full::new(Bytes::new())),
            );
        }
    };
    if first > 0
        && let Err(why) = file.seek(SeekFrom::Start(first)).await
    {
        tracing::error!(%why, path = %path.display(), "an Xcode archive could not be read");
        return refused(StatusCode::INTERNAL_SERVER_ERROR, "store_failed");
    }
    let mut builder = Response::builder()
        .status(status)
        .header(ACCEPT_RANGES, "bytes")
        .header(ETAG, etag)
        .header(CONTENT_TYPE, "application/octet-stream")
        .header(CONTENT_LENGTH, length);
    if let Some(range) = range {
        builder = builder.header(CONTENT_RANGE, range);
    }
    built(
        builder,
        body_of(Reading {
            file,
            left: length,
            _hold: hold,
        }),
    )
}

#[cfg(test)]
#[path = "xcode_get_tests.rs"]
mod tests;
