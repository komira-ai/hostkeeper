//! The upload that resumes (docs/design/xcode-one-action.md, section 3), a child of
//! [`crate::xcode_put`]: `GET /v1/xcode/<sha256>/upload`, which says how much of an archive the
//! store holds, and `PUT /v1/xcode/<sha256>?version=…&build=…` with
//! `Content-Range: bytes <first>-<last>/<size>`, which sends one part of it. A `PUT` without
//! `Content-Range` is not this module's: it is the one request of [`crate::xcode_put`].
//!
//! **The query.** The operator's bearer token, a request with no client certificate, and a path of
//! a SHA-256 and `/upload` (the checks of the `PUT`, in the same order: 403 `forbidden`, 401
//! `unauthorized`, 400 `bad_request`) are answered 200 with
//! `{"state":"held"|"partial"|"none","received":<bytes>,"size":<bytes>}`. `held`: the store holds
//! an archive of these bytes (`received` and `size` are its size). `partial`: it keeps a partial
//! upload of them (`received` is the bytes held, `size` the size the upload was started with).
//! `none`: neither (both 0). A store that cannot be read is 500 `store_failed`.
//!
//! **The part.** Every check of the `PUT` runs first, in its order, with these differences:
//! the `Content-Range` is read with the path and the query (anything but `bytes <first>-<last>/<size>`
//! with decimal numbers, `first <= last < size`, once, is 400 `bad_request`); `<size>` is the size
//! `too_large` judges, since the body of a part is not the archive; and `Content-Length` must be
//! present and equal `<last> - <first> + 1`, else 400 `bad_request`. Then, under the lock of the
//! partial upload ([`crate::xcode_store::XcodeStore::lock_upload`]; a lock another writer holds is
//! 409 `partial_locked`, and so is a second request of this coordinator for the same SHA-256 while
//! the first runs):
//!
//! 1. a partial upload of another size, version or build than the request's is 409
//!    `partial_differs`;
//! 2. `<first>` must be the bytes the partial upload holds (0 when there is none), else 409
//!    `{"error":"offset_mismatch","received":<bytes held>}`; a partial upload is made only when
//!    this holds;
//! 3. the body is appended and synced, the lock refreshed with each block. A body that ends short,
//!    or fails, or has more bytes than the range, is 400 `bad_request`, and the bytes it did write
//!    stay, synced; a request that is cut off has nobody to answer, and keeps them the same way;
//! 4. while bytes are still due, 202 `{"received":<bytes held>}`;
//! 5. when the last byte is in, the whole file is read once and hashed, the lock refreshed before
//!    each block. A SHA-256 other than the path's is 400 `digest_mismatch`, and the partial upload
//!    is removed. Then the commit is the one of the `PUT` without `Content-Range`
//!    ([`super::commit`]), and 201, and the partial upload is removed. A commit that is refused
//!    or fails (503 `audit_unavailable`, 500 `store_failed`, a check of the commit), or a file
//!    that cannot be read, leaves the partial upload, whose bytes are all there;
//! 6. a partial upload that holds all `<size>` bytes (a request cut off after the last block, or a
//!    commit that failed) takes no body: whatever part of it a request names (hk sends the last
//!    byte again), step 2 is not asked and the file is hashed and committed as in 5.

use std::path::Path;
use std::time::Duration;

use bytes::{Buf, Bytes};
use http_body_util::{BodyExt, Full};
use hyper::body::Body;
use hyper::header::CONTENT_RANGE;
use hyper::{HeaderMap, Request, Response, StatusCode};
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::{
    BAD_REQUEST, BoxError, Done, FORBIDDEN, PARTIAL_DIFFERS, PARTIAL_LOCKED, PREFIX, Peer, Refusal,
    Stop, UNAUTHORIZED, Wanted, bearer_in, commit, store_failed,
};
use crate::http::{error, json};
use crate::service::Coordinator;
use crate::tokens::Operator;
use crate::xcode_store::{Digesting, LockError, UploadLock, UploadRecord, XcodeStore};

/// The bytes of a part the request names, and the archive's whole size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Range {
    /// The first byte of the part, counted from 0.
    pub(super) first: u64,
    /// The last byte of the part.
    pub(super) last: u64,
    /// The archive's size.
    pub(super) size: u64,
}

impl Range {
    /// How many bytes the part has.
    pub(super) fn len(&self) -> u64 {
        self.last - self.first + 1
    }
}

/// `text` as digits alone: a decimal number.
fn digits(text: &str) -> Option<u64> {
    if text.bytes().all(|b| b.is_ascii_digit()) {
        text.parse().ok()
    } else {
        None
    }
}

/// `bytes <first>-<last>/<size>` as a range: `first <= last < size`.
fn parse(text: &str) -> Option<Range> {
    let (span, size) = text.strip_prefix("bytes ")?.split_once('/')?;
    let (first, last) = span.split_once('-')?;
    let range = Range {
        first: digits(first)?,
        last: digits(last)?,
        size: digits(size)?,
    };
    (range.first <= range.last && range.last < range.size).then_some(range)
}

/// The part the `Content-Range` of `headers` names: none when it has none.
///
/// # Errors
/// 400 `bad_request`: it has more than one, or one that is not a range of the form above.
pub(super) fn content_range(headers: &HeaderMap) -> Result<Option<Range>, Refusal> {
    let mut values = headers.get_all(CONTENT_RANGE).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(BAD_REQUEST);
    }
    let text = value.to_str().map_err(|_| BAD_REQUEST)?;
    parse(text).map(Some).ok_or(BAD_REQUEST)
}

/// The refusal that answers a lock that could not be had or kept.
fn locked(why: LockError) -> Refusal {
    match why {
        LockError::Held => PARTIAL_LOCKED,
        LockError::Store(why) => store_failed(why),
    }
}

/// `body` appended to `out` as it arrives, the lock refreshed before each block is written: how
/// many bytes were written, which is the count that was asked for or the refusal that stopped
/// it. Each frame must arrive within `patience`; more than `expected` bytes is 400 `bad_request`
/// at the block that passes it, which is not written.
async fn receive_part<B, W>(
    body: B,
    out: &mut W,
    lock: &mut UploadLock<'_>,
    expected: u64,
    patience: Duration,
) -> Result<u64, Refusal>
where
    B: Body,
    B::Error: Into<BoxError>,
    W: AsyncWrite + Unpin,
{
    let mut body = Box::pin(body);
    let mut written = 0u64;
    loop {
        let frame = match tokio::time::timeout(patience, body.frame()).await {
            Ok(Some(Ok(frame))) => frame,
            Ok(None) => break,
            Err(_) | Ok(Some(Err(_))) => return Err(BAD_REQUEST),
        };
        let Ok(mut data) = frame.into_data() else {
            continue;
        };
        let chunk = data.copy_to_bytes(data.remaining());
        written = written.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
        if written > expected {
            return Err(BAD_REQUEST);
        }
        lock.refresh().map_err(locked)?;
        out.write_all(&chunk).await.map_err(store_failed)?;
    }
    Ok(written)
}

/// The SHA-256 and size of the file at `path`, read once, the lock refreshed before each block
/// is read: a file of 11 GB takes longer to read than a lock is fresh for.
async fn hash_file(path: &Path, lock: &mut UploadLock<'_>) -> Result<Digesting, Refusal> {
    let mut file = tokio::fs::File::open(path).await.map_err(store_failed)?;
    let mut digest = Digesting::default();
    let mut block = vec![0u8; 1 << 20];
    loop {
        lock.refresh().map_err(locked)?;
        let read = file.read(&mut block).await.map_err(store_failed)?;
        if read == 0 {
            return Ok(digest);
        }
        digest.feed(&block[..read]);
    }
}

/// Removes the partial upload of `sha256`: its bytes are stored, or are not the archive's.
fn forget(store: &XcodeStore, sha256: &str) {
    if let Err(why) = store.remove_upload(sha256) {
        tracing::warn!(%why, %sha256, "a completed partial Xcode upload could not be removed");
    }
}

/// Takes the part `range` of the archive `wanted` from `body`, or says why not. The checks of the
/// `PUT` have passed.
pub(super) async fn part<B>(
    coordinator: &Coordinator,
    store: &XcodeStore,
    operator: &Operator,
    wanted: &Wanted,
    range: Range,
    body: B,
    patience: Duration,
) -> Result<Done, Stop>
where
    B: Body,
    B::Error: Into<BoxError>,
{
    let sha256 = wanted.sha256.as_str();
    let mut lock = store.lock_upload(sha256).map_err(locked)?;
    let record = UploadRecord {
        version: wanted.id.version.as_str().to_owned(),
        build: wanted.id.build.clone(),
        size: range.size,
        by: operator.name().to_owned(),
    };
    let held = store.upload(sha256).map_err(store_failed)?;
    if held.as_ref().is_some_and(|held| {
        held.record.size != record.size
            || held.record.version != record.version
            || held.record.build != record.build
    }) {
        return Err(PARTIAL_DIFFERS.into());
    }
    let received = held.as_ref().map_or(0, |held| held.received);
    // A partial that holds every byte (a request cut off after the last block, or a commit that
    // failed) takes no more: whatever part is sent, it is hashed and committed as it is.
    if received < range.size {
        if range.first != received {
            return Err(Stop::OffsetMismatch(received));
        }
        if held.is_none() {
            store.begin_upload(sha256, &record).map_err(store_failed)?;
        }
        let mut out = tokio::fs::File::from_std(store.append_upload(sha256).map_err(store_failed)?);
        let written = receive_part(body, &mut out, &mut lock, range.len(), patience).await;
        // What was written stays, whatever ended the request.
        let synced = out.sync_all().await;
        let written = written?;
        synced.map_err(store_failed)?;
        if written != range.len() {
            return Err(BAD_REQUEST.into());
        }
        let received = received + written;
        if received < range.size {
            return Ok(Done::Due(received));
        }
    }
    let path = store.upload_file_of(sha256);
    // A failure to read the file or to keep the lock leaves the bytes where they are.
    let digest = hash_file(&path, &mut lock).await?;
    let size = digest.bytes();
    if digest.finish() != sha256 {
        forget(store, sha256);
        return Err(super::DIGEST_MISMATCH.into());
    }
    // A refused or failed commit leaves the bytes, which hash as the archive: the next request
    // commits them without being sent again.
    let stored = commit(coordinator, store, operator, wanted, size, &path)?;
    forget(store, sha256);
    Ok(Done::Stored(stored))
}

/// The answer to the request `request` for `GET /v1/xcode/<sha256>/upload` of `store`.
pub(super) fn query<B>(
    coordinator: &Coordinator,
    store: &XcodeStore,
    peer: &Peer,
    request: &Request<B>,
) -> Response<Full<Bytes>> {
    match asked(coordinator, store, peer, request) {
        Ok((state, received, size)) => json(
            StatusCode::OK,
            &serde_json::json!({ "state": state, "received": received, "size": size }),
        ),
        Err((status, code)) => error(status, code),
    }
}

/// What the store holds of the archive the request names: `held`, `partial` or `none`, the bytes
/// received and the size.
fn asked<B>(
    coordinator: &Coordinator,
    store: &XcodeStore,
    peer: &Peer,
    request: &Request<B>,
) -> Result<(&'static str, u64, u64), Refusal> {
    if peer.is_some() {
        return Err(FORBIDDEN);
    }
    coordinator
        .operator(bearer_in(request.headers()))
        .map_err(|_| UNAUTHORIZED)?;
    let path = request.uri().path();
    let sha256 = path
        .strip_prefix(PREFIX)
        .and_then(|rest| rest.strip_suffix("/upload"))
        .filter(|sha| hostkeeper_release::grammar::is_sha256(sha))
        .ok_or(BAD_REQUEST)?;
    if let Some(archive) = store.archive(sha256).map_err(store_failed)? {
        return Ok(("held", archive.size, archive.size));
    }
    Ok(match store.upload(sha256).map_err(store_failed)? {
        Some(upload) => ("partial", upload.received, upload.record.size),
        None => ("none", 0, 0),
    })
}
