//! `PUT /v1/xcode/<sha256>?version=<version>&build=<build>` on the private listener: an operator
//! pushes one Xcode archive into the coordinator's Xcode store ([`crate::xcode_store`]). The
//! route exists only on a coordinator that has the store (`--xcode-dir`); without it the path is a
//! 404 like any other.
//!
//! The request is checked, in this order, before anything is stored; each refusal is the JSON
//! `{"error":"<code>"}` of [`crate::http::error`], and every one of them leaves the store as it
//! was, whatever part of the body had been written:
//!
//! 1. any method but `PUT`: 405 `method_not_allowed`;
//! 2. a client certificate: 403 `forbidden`. No operator credential is a certificate, and a host's
//!    never uploads;
//! 3. the operator's bearer token ([`Coordinator::operator`]): 401 `unauthorized` for none, an
//!    unknown one, and CI's. The body is not read before this;
//! 4. the path and the query: after the prefix exactly 64 lowercase hex characters, the archive's
//!    SHA-256; the query is exactly the keys `version` (dot-separated decimal numbers) and `build`
//!    (1 to 32 ASCII letters, digits and dots, as the list of releases writes a build), each once,
//!    and nothing is percent-decoded: 400 `bad_request`;
//! 5. a `Content-Length` over the store's `--xcode-max-bytes`: 413 `too_large`, the body unread;
//! 6. the release must be one the list the coordinator read last holds, of the channel `release`
//!    (a beta or a release candidate is not stored), with this exact version and build: 409
//!    `not_listed`; a list that cannot be read: 500 `store_failed`;
//! 7. what the store holds ([`XcodeStore::holds`]): the release, or an archive of these bytes, is
//!    409 `already_held`; another build of the version is 409 `xcode_version_taken`, which stands
//!    as the alert of that name until the held archive is removed; a store that cannot be read is
//!    500 `store_failed`;
//! 8. an upload the store would remove at once: the store keeps the newest releases (as many as
//!    the largest `xcode_keep` of any group, at least one) and each installed one
//!    ([`crate::xcode_store::kept`]), so a release older than those it keeps is 409 `not_kept`,
//!    its body unread. This is what makes the release just stored always stay;
//! 9. another upload of the version in progress: 409 `upload_in_progress`.
//!
//! The body is then streamed into a file of the store, never held whole: each frame must arrive
//! within `patience` (a stall, or a body that fails, is 400 `bad_request`), the bytes are counted
//! and hashed as they arrive, and more than `--xcode-max-bytes` is 413 `too_large` as soon as the
//! count passes it. While it streams, the connection is held ([`ConnectionWaits`]), so an HTTP/2
//! connection is not cut a patience after its start. A body whose SHA-256 is not the path's is 400
//! `digest_mismatch`.
//!
//! The commit runs under the store's change lock: what the store holds is read again (a second
//! upload of another build of the version, or of these bytes, that was committed meanwhile is
//! refused as in 7, and a newer release committed meanwhile that makes this one older than those
//! the store keeps is refused as in 8); the upload is audited as `xcode_put`, by the token's name, with the release,
//! the SHA-256 and the size as its reason, and a line that cannot be appended is 503
//! `audit_unavailable`; only then is the archive committed ([`XcodeStore::commit`]), whose
//! failure is 500 `store_failed` (the audit line stands: it is written first, so that nothing is
//! stored that the audit does not say). The store is then pruned
//! ([`Coordinator::prune_xcode_store`]) and the answer is 201 with the JSON
//! `{"sha256":..,"version":..,"build":..,"size":..}`.
//!
//! A request with a `Content-Range` is one part of an upload that resumes, and
//! `GET /v1/xcode/<sha256>/upload` asks how much of an archive the store holds: both are
//! [`range`]'s. The checks above run for a part as for a whole upload, with the size the range
//! names for 5, and the commit is the same.
//!
//! The upload file is removed when the request ends by any way but the commit, including when the
//! client goes away and the request is dropped. The token is never in a log line: the one line a
//! stored archive logs names the release, the SHA-256, the size and who uploaded it.

use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bytes::{Buf, Bytes};
use hostkeeper_release::grammar::is_sha256;
use http_body_util::{BodyExt, Full};
use hyper::body::Body;
use hyper::header::CONTENT_LENGTH;
use hyper::{HeaderMap, Method, Request, Response, StatusCode, Uri};
use tokio::io::{AsyncWrite, AsyncWriteExt};

use crate::audit_actions::XCODE_PUT;
use crate::http::{BoxError, Peer, bearer_in, error, json};
use crate::ops::{AuditEntry, Write};
use crate::service::Coordinator;
use crate::tokens::Operator;
use crate::wait::ConnectionWaits;
use crate::xcode_feed::{Channel, XcodeId, XcodeVersion};
use crate::xcode_store::{Archive, Digesting, Held, PREFIX, XcodeStore, kept, remove_quietly};

/// The upload that resumes: the query of what the store holds, and a part of an upload.
#[path = "xcode_put_range.rs"]
mod range;

/// A refusal: the status and the code of the JSON error.
type Refusal = (StatusCode, &'static str);

const NOT_PUT: Refusal = (StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed");
const FORBIDDEN: Refusal = (StatusCode::FORBIDDEN, "forbidden");
const UNAUTHORIZED: Refusal = (StatusCode::UNAUTHORIZED, "unauthorized");
const BAD_REQUEST: Refusal = (StatusCode::BAD_REQUEST, "bad_request");
const TOO_LARGE: Refusal = (StatusCode::PAYLOAD_TOO_LARGE, "too_large");
const NOT_LISTED: Refusal = (StatusCode::CONFLICT, "not_listed");
const ALREADY_HELD: Refusal = (StatusCode::CONFLICT, "already_held");
const VERSION_TAKEN: Refusal = (StatusCode::CONFLICT, "xcode_version_taken");
const NOT_KEPT: Refusal = (StatusCode::CONFLICT, "not_kept");
const IN_PROGRESS: Refusal = (StatusCode::CONFLICT, "upload_in_progress");
const PARTIAL_LOCKED: Refusal = (StatusCode::CONFLICT, "partial_locked");
const PARTIAL_DIFFERS: Refusal = (StatusCode::CONFLICT, "partial_differs");
const DIGEST_MISMATCH: Refusal = (StatusCode::BAD_REQUEST, "digest_mismatch");
const AUDIT_UNAVAILABLE: Refusal = (StatusCode::SERVICE_UNAVAILABLE, "audit_unavailable");
const STORE_FAILED: Refusal = (StatusCode::INTERNAL_SERVER_ERROR, "store_failed");

/// What ends a request that was not stored: a refusal, or the answer of a part that began at
/// another byte than the partial upload holds.
enum Stop {
    /// The status and code of the JSON error.
    Refused(Refusal),
    /// 409 `offset_mismatch` with the bytes the store holds.
    OffsetMismatch(u64),
}

impl From<Refusal> for Stop {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal)
    }
}

/// What a request that was accepted did.
enum Done {
    /// The archive was committed: 201.
    Stored(Archive),
    /// A part was kept and bytes are still due: 202 with the bytes held.
    Due(u64),
}

/// The end of the path of the query of what the store holds of an archive.
const UPLOAD_PATH: &str = "/upload";

/// The most bytes of a build the request may name: as the list of releases takes.
const MAX_BUILD_BYTES: usize = 32;

/// What the path and the query of a request name.
struct Wanted {
    /// The archive's SHA-256, as the path names it.
    sha256: String,
    /// The release the archive is.
    id: XcodeId,
}

/// The archive the path and the query of `uri` name: none when the path is not the prefix and a
/// SHA-256, or the query is not the keys `version` and `build` once each with values that fit.
fn wanted(uri: &Uri) -> Option<Wanted> {
    let path = uri.path();
    let sha256 = path.strip_prefix(PREFIX).unwrap_or(path);
    if !is_sha256(sha256) {
        return None;
    }
    let mut version: Option<&str> = None;
    let mut build: Option<&str> = None;
    for pair in uri.query()?.split('&') {
        let (key, value) = pair.split_once('=')?;
        let slot = match key {
            "version" => &mut version,
            "build" => &mut build,
            _ => return None,
        };
        if slot.replace(value).is_some() {
            return None;
        }
    }
    let build = build?;
    let fits = !build.is_empty()
        && build.len() <= MAX_BUILD_BYTES
        && build
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.');
    if !fits {
        return None;
    }
    Some(Wanted {
        sha256: sha256.to_owned(),
        id: XcodeId {
            version: XcodeVersion::parse(version?).ok()?,
            build: build.to_owned(),
        },
    })
}

/// The length the request's `Content-Length` declares, if it has a readable one.
fn declared(headers: &HeaderMap) -> Option<u64> {
    headers.get(CONTENT_LENGTH)?.to_str().ok()?.parse().ok()
}

/// A failure of the store or the list: logged, and answered `store_failed`.
fn store_failed(why: impl Display) -> Refusal {
    tracing::error!(%why, "an upload to the Xcode store failed");
    STORE_FAILED
}

/// The upload file of a request: removed when the request ends without having committed it (a
/// committed upload is renamed away, so there is nothing to remove then).
struct Partial(PathBuf);

impl Drop for Partial {
    fn drop(&mut self) {
        remove_quietly(&self.0);
    }
}

/// Whether the list the coordinator read last holds `id` as a release.
fn listed(coordinator: &Coordinator, id: &XcodeId) -> Result<(), Refusal> {
    let known = coordinator.xcode.known.known().map_err(store_failed)?;
    if known
        .iter()
        .any(|release| release.channel == Channel::Release && release.id == *id)
    {
        Ok(())
    } else {
        Err(NOT_LISTED)
    }
}

/// What the store holds of the archive `wanted`: refused when it holds the release or the bytes,
/// or another build of the version (which is noted, for the alert).
fn unheld(store: &XcodeStore, wanted: &Wanted) -> Result<(), Refusal> {
    match store
        .holds(&wanted.id, &wanted.sha256)
        .map_err(store_failed)?
    {
        Held::Free => Ok(()),
        Held::Already => Err(ALREADY_HELD),
        Held::VersionTaken(held_build) => {
            let version = wanted.id.version.as_str();
            store.note_taken(version, &held_build, &wanted.id.build);
            tracing::warn!(
                %version,
                %held_build,
                refused_build = %wanted.id.build,
                "an Xcode archive was refused: the store holds another build of that version"
            );
            Err(VERSION_TAKEN)
        }
    }
}

/// Refuses the archive `wanted` when the store would remove it as soon as it was stored: of the
/// archives the store holds and this one, [`kept`] removes this one (an older release than those
/// the store keeps, that no host has installed).
fn stays(coordinator: &Coordinator, store: &XcodeStore, wanted: &Wanted) -> Result<(), Refusal> {
    let mut archives = store.archives().map_err(store_failed)?;
    archives.push(Archive {
        sha256: wanted.sha256.clone(),
        id: wanted.id.clone(),
        size: 0,
        by: String::new(),
        at_unix: 0,
    });
    let (_, goes) = kept(
        &archives,
        coordinator.xcode_keep_largest(),
        &coordinator.xcodes_installed(),
    );
    if goes.iter().any(|archive| archive.sha256 == wanted.sha256) {
        Err(NOT_KEPT)
    } else {
        Ok(())
    }
}

/// `body` written to `out` (and flushed) and hashed as it arrives: the SHA-256 and the number of
/// bytes. Each frame must arrive within `patience`; more than `max` bytes is refused as soon as
/// it is seen.
async fn receive<B, W>(
    body: B,
    out: &mut W,
    max: u64,
    patience: Duration,
) -> Result<Digesting, Refusal>
where
    B: Body,
    B::Error: Into<BoxError>,
    W: AsyncWrite + Unpin,
{
    let mut body = Box::pin(body);
    let mut digest = Digesting::default();
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
        digest.feed(&chunk);
        if digest.bytes() > max {
            return Err(TOO_LARGE);
        }
        out.write_all(&chunk).await.map_err(store_failed)?;
    }
    out.flush().await.map_err(store_failed)?;
    Ok(digest)
}

/// Commits the upload at `path`, whose SHA-256 is `wanted`'s and which has `size` bytes, as the
/// archive: under the store's change lock what the store holds is read again (a second upload of
/// another build of the version, or of these bytes, that was committed meanwhile is refused as in
/// 7, and a newer release committed meanwhile that makes this one older than those the store
/// keeps is refused as in 8), the upload is audited as `xcode_put`, then committed and the store
/// pruned.
fn commit(
    coordinator: &Coordinator,
    store: &XcodeStore,
    operator: &Operator,
    wanted: &Wanted,
    size: u64,
    path: &Path,
) -> Result<Archive, Refusal> {
    let archive = Archive {
        sha256: wanted.sha256.clone(),
        id: wanted.id.clone(),
        size,
        by: operator.name().to_owned(),
        at_unix: coordinator.ops.unix_now(),
    };
    {
        let _changing = store.changing();
        unheld(store, wanted)?;
        stays(coordinator, store, wanted)?;
        let mut entry = AuditEntry::new(operator, XCODE_PUT);
        entry.reason = Some(format!(
            "Xcode {} ({}), sha256 {}, {size} bytes",
            archive.id.version, archive.id.build, archive.sha256
        ));
        coordinator
            .ops
            .change(Write::BeforeMemory, entry, |_| {})
            .map_err(|why| {
                tracing::error!(%why, "an Xcode upload could not be audited");
                AUDIT_UNAVAILABLE
            })?;
        store.commit(path, &archive).map_err(store_failed)?;
    }
    coordinator.prune_xcode_store();
    Ok(archive)
}

/// Stores the upload of `request`, or says why not.
async fn put<B>(
    coordinator: &Coordinator,
    store: &XcodeStore,
    peer: &Peer,
    request: Request<B>,
    patience: Duration,
) -> Result<Done, Stop>
where
    B: Body,
    B::Error: Into<BoxError>,
{
    let (parts, body) = request.into_parts();
    if parts.method != Method::PUT {
        return Err(NOT_PUT.into());
    }
    if peer.is_some() {
        return Err(FORBIDDEN.into());
    }
    let operator = coordinator
        .operator(bearer_in(&parts.headers))
        .map_err(|_| UNAUTHORIZED)?;
    let wanted = wanted(&parts.uri).ok_or(BAD_REQUEST)?;
    let part = range::content_range(&parts.headers)?;
    // A part is judged by the size of the archive it is a part of, and must be as long as its
    // range says.
    let size = part.map_or_else(|| declared(&parts.headers), |part| Some(part.size));
    if size.is_some_and(|length| length > store.max_bytes()) {
        return Err(TOO_LARGE.into());
    }
    if part.is_some_and(|part| declared(&parts.headers) != Some(part.len())) {
        return Err(BAD_REQUEST.into());
    }
    listed(coordinator, &wanted.id)?;
    unheld(store, &wanted)?;
    stays(coordinator, store, &wanted)?;
    let Some(_reserved) = store.reserve(wanted.id.version.as_str()) else {
        // A part of an archive that this coordinator is receiving a part of already is the
        // partial upload's lock, not another upload of the version.
        let refusal = if part.is_some() && store.is_writing(&wanted.sha256) {
            PARTIAL_LOCKED
        } else {
            IN_PROGRESS
        };
        return Err(refusal.into());
    };
    let _held = parts
        .extensions
        .get::<ConnectionWaits>()
        .map(ConnectionWaits::hold);

    if let Some(part) = part {
        return range::part(coordinator, store, &operator, &wanted, part, body, patience).await;
    }
    let (path, file) = store.upload_file(&wanted.sha256).map_err(store_failed)?;
    let partial = Partial(path);
    let mut out = tokio::fs::File::from_std(file);
    let hashed = receive(body, &mut out, store.max_bytes(), patience).await?;
    out.sync_all().await.map_err(store_failed)?;
    let size = hashed.bytes();
    if hashed.finish() != wanted.sha256 {
        return Err(DIGEST_MISMATCH.into());
    }
    Ok(Done::Stored(commit(
        coordinator,
        store,
        &operator,
        &wanted,
        size,
        &partial.0,
    )?))
}

/// Answers the request `request` of `peer` to upload an archive to `store`, or, for a `GET` of
/// `/v1/xcode/<sha256>/upload`, what the store holds of it.
pub(crate) async fn answer<B>(
    coordinator: &Coordinator,
    store: &XcodeStore,
    peer: &Peer,
    request: Request<B>,
    patience: Duration,
) -> Response<Full<Bytes>>
where
    B: Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<BoxError> + Send + 'static,
{
    if request.method() == Method::GET && request.uri().path().ends_with(UPLOAD_PATH) {
        return range::query(coordinator, store, peer, &request);
    }
    match put(coordinator, store, peer, request, patience).await {
        Ok(Done::Stored(archive)) => {
            tracing::info!(
                version = %archive.id.version,
                build = %archive.id.build,
                sha256 = %archive.sha256,
                size = archive.size,
                by = %archive.by,
                "an Xcode archive was stored"
            );
            json(
                StatusCode::CREATED,
                &serde_json::json!({
                    "sha256": archive.sha256,
                    "version": archive.id.version.as_str(),
                    "build": archive.id.build,
                    "size": archive.size,
                }),
            )
        }
        Ok(Done::Due(received)) => json(
            StatusCode::ACCEPTED,
            &serde_json::json!({ "received": received }),
        ),
        Err(Stop::Refused((status, code))) => error(status, code),
        Err(Stop::OffsetMismatch(received)) => json(
            StatusCode::CONFLICT,
            &serde_json::json!({ "error": "offset_mismatch", "received": received }),
        ),
    }
}

#[cfg(test)]
#[path = "xcode_put_tests.rs"]
mod tests;
