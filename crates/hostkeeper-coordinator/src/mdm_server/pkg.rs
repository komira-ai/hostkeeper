//! `GET /pkg/<sha256>.pkg` on the Macs' listener (`docs/design/coordinator-mdm.md` section 4.7,
//! `docs/design/package-proxy.md` section 3): the pkg of an `installs` entry, from the directory
//! `--mdm-pkg-dir` (files the host provides, named `<sha256>.pkg`).
//!
//! **What is served.** Only a digest that an entry of the setting `installs` names *now* (read at
//! every request, so an edit of the setting takes effect at once). Any other digest, a name that is
//! not 64 lower-case hexadecimal digits and `.pkg` (so no `..`, no separator, no `%`), a digest
//! with no file, and a coordinator started without `--mdm-pkg-dir` are `404 not_found`, and for a
//! digest that is not in the setting the directory is not touched. A method other than `GET` and
//! `HEAD` is `405 method_not_allowed`.
//!
//! **The digest check.** The file is read once, in chunks, and never held whole. A full `GET` sends
//! `Content-Length` and the bytes as they are read, hashing as it goes; the last chunk is held
//! back until the file has ended and its SHA-256 equals the one in the path, and on a mismatch (or
//! a file shorter than it was when the answer began) the body ends in an error instead of that
//! chunk, so the client's connection is cut with fewer bytes than the length it was promised and
//! no client holds a complete wrong file. A `HEAD` and a `Range` request cannot be checked as they
//! are streamed, so they are answered only for a file whose whole digest was checked, once, at its
//! present size, modification time, inode and change time (a file replaced with `cp -p` or `rsync
//! -t` keeps the first two and not the last): an earlier full `GET` that ended well records it, and
//! otherwise the file is hashed first (still in chunks); a file that fails is `500
//! pkg_digest_mismatch`.
//!
//! **Validators.** The digest names the bytes, so the strong `ETag` is `"<sha256>"`; a request
//! whose `If-None-Match` lists it (or is `*`) is `304` with no body and no read. The file carries
//! no `Last-Modified`, so `If-Modified-Since` has nothing to compare and is ignored, as RFC 9110
//! section 13.1.3 allows. `Range` takes one range (`a-b`, `a-`, `-n`) and is `206`; a range past
//! the end is `416`; several ranges, a malformed value and an `If-Range` other than the `ETag` are
//! answered as if there were no `Range`.

use std::collections::BTreeMap;
use std::io::{self, SeekFrom};
use std::os::unix::fs::MetadataExt as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use bytes::{Bytes, BytesMut};
use futures::stream;
use hostkeeper_mdm_gate::{Installs, PkgDigest};
use http_body_util::{Either, Full, StreamBody};
use hyper::body::Frame;
use hyper::header::{
    ACCEPT_RANGES, ALLOW, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG,
    HeaderMap, HeaderValue, IF_NONE_MATCH, IF_RANGE, RANGE,
};
use hyper::{Method, Response, StatusCode};
use sha2::{Digest, Sha256};
use tokio::fs::File;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncSeek, AsyncSeekExt as _};

use super::{Family, MdmServer};
use crate::grpc::RouteBody;
use crate::http::error;
use crate::mdm_setup::MdmSetup;
use crate::service::Coordinator;

/// The path every route of this module is under.
pub(crate) const PREFIX: &str = "/pkg/";

/// How many bytes one read of the file takes, and so the most a chunk of the body holds.
const CHUNK: usize = 64 * 1024;

/// What `--mdm-pkg-dir` names, and which of its files were checked whole.
#[derive(Debug, Default)]
pub(crate) struct Pkgs {
    dir: Option<PathBuf>,
    /// For each digest, the stamp of the file when its whole digest was last found equal to the
    /// name.
    checked: Mutex<BTreeMap<String, Stamp>>,
}

/// What identifies the file a check was made of: its size, modification time, device and inode,
/// and change time. A user cannot set the change time, so a file replaced or written in place under
/// the old size and modification time (`cp -p`, `rsync -t`, a restore) has another stamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
    device: u64,
    inode: u64,
    changed: (i64, i64),
}

impl Stamp {
    fn of(meta: &std::fs::Metadata) -> Self {
        Self {
            len: meta.len(),
            modified: meta.modified().ok(),
            device: meta.dev(),
            inode: meta.ino(),
            changed: (meta.ctime(), meta.ctime_nsec()),
        }
    }
}

/// `text` as a header value. The texts of this module are digits, hex digits and ASCII
/// punctuation, which every header value allows.
fn header(text: &str) -> HeaderValue {
    HeaderValue::from_str(text).unwrap_or(HeaderValue::from_static(""))
}

impl Pkgs {
    /// The pkgs in `dir`; none without `--mdm-pkg-dir`.
    pub(crate) fn new(dir: Option<PathBuf>) -> Self {
        Self {
            dir,
            checked: Mutex::default(),
        }
    }

    fn is_checked(&self, digest: &str, stamp: Stamp) -> bool {
        self.checked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(digest)
            == Some(&stamp)
    }

    fn record(&self, digest: &str, stamp: Stamp) {
        self.checked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(digest.to_owned(), stamp);
    }
}

/// The digest `path` (`/pkg/<sha256>.pkg`) names; none for any other form.
fn digest_in(path: &str) -> Option<PkgDigest> {
    PkgDigest::parse(path.strip_prefix(PREFIX)?.strip_suffix(".pkg")?)
}

/// A JSON error as the route's body.
fn failure(status: StatusCode, code: &'static str) -> Response<RouteBody> {
    error(status, code).map(Either::Left)
}

/// An answer with no body.
fn empty(status: StatusCode) -> Response<RouteBody> {
    let mut response = Response::new(Either::Left(Full::new(Bytes::new())));
    *response.status_mut() = status;
    response
}

/// Whether the setting `installs` names `digest` now. A setting that cannot be read is an error:
/// the answer is `500`, not a guess.
fn is_installed(setup: &MdmSetup, digest: &PkgDigest) -> Result<bool, ()> {
    let settings = setup.settings().map_err(|why| {
        tracing::error!(%why, "the MDM settings could not be read for a pkg");
    })?;
    let Some(text) = settings.get(hostkeeper_mdm_gate::SETTING_NAME) else {
        return Ok(false);
    };
    let installs = Installs::parse(text).map_err(|why| {
        tracing::error!(%why, "the MDM setting installs does not read, so no pkg is served");
    })?;
    Ok(installs
        .entries()
        .iter()
        .any(|entry| entry.sha256 == *digest))
}

/// The part of the file a request asks for.
#[derive(Debug, PartialEq, Eq)]
enum Wanted {
    /// All of it.
    Whole,
    /// Bytes `start` to `end`, both included.
    Part { start: u64, end: u64 },
    /// A range that starts past the end.
    Unsatisfiable,
}

/// The range `headers` ask of a file of `len` bytes whose `ETag` is `etag`.
fn wanted(headers: &HeaderMap, len: u64, etag: &str) -> Wanted {
    let Some(value) = headers.get(RANGE).and_then(|v| v.to_str().ok()) else {
        return Wanted::Whole;
    };
    if let Some(condition) = headers.get(IF_RANGE)
        && condition.as_bytes() != etag.as_bytes()
    {
        return Wanted::Whole;
    }
    let Some(spec) = value.trim().strip_prefix("bytes=") else {
        return Wanted::Whole;
    };
    let Some((first, last)) = spec.trim().split_once('-') else {
        return Wanted::Whole;
    };
    let number = |text: &str| -> Option<u64> {
        (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
            .then(|| text.parse().ok())
            .flatten()
    };
    let (first, last) = (first.trim(), last.trim());
    match (number(first), number(last)) {
        // `-n`: the last n bytes.
        (None, Some(n)) if first.is_empty() && (n == 0 || len == 0) => Wanted::Unsatisfiable,
        (None, Some(n)) if first.is_empty() => Wanted::Part {
            start: len.saturating_sub(n),
            end: len - 1,
        },
        (Some(start), None) if last.is_empty() => {
            if start < len {
                Wanted::Part {
                    start,
                    end: len - 1,
                }
            } else {
                Wanted::Unsatisfiable
            }
        }
        (Some(start), Some(end)) if start <= end => {
            if start < len {
                Wanted::Part {
                    start,
                    end: end.min(len - 1),
                }
            } else {
                Wanted::Unsatisfiable
            }
        }
        // Several ranges, `b-a` with b after a, or text that is no range: no `Range` at all.
        _ => Wanted::Whole,
    }
}

/// Whether `headers` hold an `If-None-Match` that lists `etag` or is `*`.
fn matches_none(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get_all(IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .any(|tag| tag == "*" || tag.strip_prefix("W/").unwrap_or(tag) == etag)
}

/// The SHA-256 of the next `len` bytes of `file`, read in chunks, in hex; of fewer bytes when the
/// file ends before that.
async fn hash_of<R: AsyncRead + Unpin>(file: &mut R, len: u64) -> io::Result<String> {
    let mut hasher = Sha256::new();
    let mut left = len;
    let mut buffer = vec![0u8; CHUNK];
    while left > 0 {
        let take = usize::try_from(left).map_or(CHUNK, |left| left.min(CHUNK));
        let read = file.read(&mut buffer[..take]).await?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        left -= read as u64;
    }
    Ok(hex::encode(hasher.finalize()))
}

/// The state of the body of a full `GET`: the file read so far and the chunk held back.
struct Streaming<R> {
    file: R,
    hasher: Sha256,
    held: Option<Bytes>,
    left: u64,
    done: bool,
    digest: String,
    stamp: Stamp,
    server: Arc<MdmServer>,
}

impl<R: AsyncRead + Unpin> Streaming<R> {
    /// The next frame of the body: the chunk read before this one, or at the end the held last
    /// chunk once the digest is found equal, or the error that cuts the connection.
    async fn next(&mut self) -> Option<io::Result<Frame<Bytes>>> {
        loop {
            if self.done {
                return None;
            }
            let mut buffer = BytesMut::zeroed(CHUNK);
            let take = usize::try_from(self.left).map_or(CHUNK, |left| left.min(CHUNK));
            let read = if take == 0 {
                Ok(0)
            } else {
                self.file.read(&mut buffer[..take]).await
            };
            match read {
                Ok(read) if read > 0 => {
                    buffer.truncate(read);
                    self.hasher.update(&buffer);
                    self.left -= read as u64;
                    if let Some(sent) = self.held.replace(buffer.freeze()) {
                        return Some(Ok(Frame::data(sent)));
                    }
                }
                ended => {
                    // The end of the file, or a read that failed: the digest decides the first,
                    // and the second is never whole.
                    self.done = true;
                    let hasher = std::mem::take(&mut self.hasher);
                    if matches!(ended, Ok(0))
                        && self.left == 0
                        && hex::encode(hasher.finalize()) == self.digest
                    {
                        self.server.pkgs.record(&self.digest, self.stamp);
                        return self.held.take().map(|last| Ok(Frame::data(last)));
                    }
                    tracing::error!(?ended, digest = %self.digest, "the bytes of a pkg do not match its digest or could not be read; the answer was cut");
                    return Some(Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "the pkg differs from its digest",
                    )));
                }
            }
        }
    }
}

/// The body of `length` bytes of `file` from byte `start`, with nothing hashed: a range of a file
/// that was checked whole. A seek or read that fails, or a file that ends early, ends the body in
/// an error.
fn part<R: AsyncRead + AsyncSeek + Send + Unpin + 'static>(
    file: R,
    start: u64,
    length: u64,
) -> RouteBody {
    let frames = stream::unfold(
        (file, length, Some(start)),
        |(mut file, left, from)| async move {
            if left == 0 {
                return None;
            }
            let take = usize::try_from(left).map_or(CHUNK, |left| left.min(CHUNK));
            let mut buffer = BytesMut::zeroed(take);
            let sought = match from {
                Some(start) => file.seek(SeekFrom::Start(start)).await.map(drop),
                None => Ok(()),
            };
            let read = match sought {
                Ok(()) => file.read(&mut buffer).await.and_then(|read| match read {
                    0 => Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "the pkg is shorter than it was",
                    )),
                    read => Ok(read),
                }),
                Err(why) => Err(why),
            };
            match read {
                Ok(read) => {
                    buffer.truncate(read);
                    Some((
                        Ok(Frame::data(buffer.freeze())),
                        (file, left - read as u64, None),
                    ))
                }
                Err(why) => Some((Err(why), (file, 0, None))),
            }
        },
    );
    Either::Right(tonic::body::Body::new(StreamBody::new(frames)))
}

/// Answers the `method` request for `path` (a path under [`PREFIX`]) with `headers`.
pub(crate) async fn serve(
    coordinator: &Coordinator,
    server: &Arc<MdmServer>,
    method: &Method,
    path: &str,
    headers: &HeaderMap,
) -> Response<RouteBody> {
    if !server.serves(Family::Proxy) {
        return failure(StatusCode::SERVICE_UNAVAILABLE, "unavailable");
    }
    let Some(digest) = digest_in(path) else {
        return failure(StatusCode::NOT_FOUND, "not_found");
    };
    if method != Method::GET && method != Method::HEAD {
        let mut response = failure(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed");
        response
            .headers_mut()
            .insert(ALLOW, HeaderValue::from_static("GET, HEAD"));
        return response;
    }
    match is_installed(coordinator.mdm_setup(), &digest) {
        Ok(true) => {}
        Ok(false) => return failure(StatusCode::NOT_FOUND, "not_found"),
        Err(()) => return failure(StatusCode::INTERNAL_SERVER_ERROR, "internal"),
    }
    let Some(dir) = &server.pkgs.dir else {
        return failure(StatusCode::NOT_FOUND, "not_found");
    };
    let digest = digest.to_string();
    let opened = File::open(dir.join(format!("{digest}.pkg"))).await;
    let mut file = match opened {
        Ok(file) => file,
        Err(why) if why.kind() == io::ErrorKind::NotFound => {
            tracing::warn!(%digest, "an install names a pkg that is not in the pkg directory");
            return failure(StatusCode::NOT_FOUND, "not_found");
        }
        Err(why) => {
            tracing::error!(%why, %digest, "a pkg could not be opened");
            return failure(StatusCode::INTERNAL_SERVER_ERROR, "internal");
        }
    };
    let Some(meta) = file
        .metadata()
        .await
        .ok()
        .filter(std::fs::Metadata::is_file)
    else {
        // Not a regular file (a directory in its place), or not one that can be inspected.
        return failure(StatusCode::NOT_FOUND, "not_found");
    };
    let len = meta.len();
    let stamp = Stamp::of(&meta);
    let etag = format!("\"{digest}\"");
    let tag = header(&etag);
    let mut response = if matches_none(headers, &etag) {
        empty(StatusCode::NOT_MODIFIED)
    } else {
        let wanted = wanted(headers, len, &etag);
        if wanted == Wanted::Unsatisfiable {
            let mut response = empty(StatusCode::RANGE_NOT_SATISFIABLE);
            response
                .headers_mut()
                .insert(CONTENT_RANGE, header(&format!("bytes */{len}")));
            response
        } else {
            let whole_get = wanted == Wanted::Whole && method == Method::GET;
            if !whole_get && !server.pkgs.is_checked(&digest, stamp) {
                match hash_of(&mut file, len).await {
                    Ok(found) if found == digest => server.pkgs.record(&digest, stamp),
                    Ok(_) => {
                        tracing::error!(%digest, "the bytes of a pkg do not match its digest; it is not served");
                        return failure(StatusCode::INTERNAL_SERVER_ERROR, "pkg_digest_mismatch");
                    }
                    Err(why) => {
                        tracing::error!(%why, %digest, "a pkg could not be read");
                        return failure(StatusCode::INTERNAL_SERVER_ERROR, "internal");
                    }
                }
            }
            let (status, start, length) = match wanted {
                Wanted::Part { start, end } => {
                    (StatusCode::PARTIAL_CONTENT, start, end - start + 1)
                }
                _ => (StatusCode::OK, 0, len),
            };
            let body = if method == Method::HEAD {
                Either::Left(Full::new(Bytes::new()))
            } else if whole_get {
                let streaming = Streaming {
                    file,
                    hasher: Sha256::new(),
                    held: None,
                    left: len,
                    done: false,
                    digest: digest.clone(),
                    stamp,
                    server: Arc::clone(server),
                };
                let frames = stream::unfold(streaming, |mut streaming| async move {
                    streaming.next().await.map(|frame| (frame, streaming))
                });
                Either::Right(tonic::body::Body::new(StreamBody::new(frames)))
            } else {
                part(file, start, length)
            };
            let mut response = Response::new(body);
            *response.status_mut() = status;
            let out = response.headers_mut();
            out.insert(CONTENT_LENGTH, HeaderValue::from(length));
            out.insert(
                CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            );
            if status == StatusCode::PARTIAL_CONTENT {
                out.insert(
                    CONTENT_RANGE,
                    header(&format!("bytes {start}-{}/{len}", start + length - 1)),
                );
            }
            response
        }
    };
    let status = response.status();
    let out = response.headers_mut();
    out.insert(ETAG, tag);
    out.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    // An answer that is no part of the file (`416`) is not stored for a year.
    if status != StatusCode::RANGE_NOT_SATISFIABLE {
        out.insert(
            CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=31536000, immutable"),
        );
    }
    response
}

#[cfg(test)]
#[path = "pkg_tests.rs"]
mod tests;
