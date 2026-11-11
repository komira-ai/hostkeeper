//! The pull-through of `GET /pkg/<sha256>.pkg` (`docs/design/package-proxy.md`, sections 3 and 4):
//! a pkg that an entry of the setting `installs` names and that is not in `--mdm-pkg-dir` is read
//! once from the entry's `url`, checked against the digest in its path, kept in the proxy's cache
//! and served from there.
//!
//! **The digest decides before the first byte.** The route can only answer `502` while it has
//! sent nothing, so the pkg is read from the origin to its end, hashed as it goes, before the Mac
//! is answered at all: a body whose SHA-256 is not the digest ends the read in an error, so the
//! cache keeps nothing of it and the Mac is answered `502 origin_failed`. A body that is the
//! digest's is committed, and the Mac is answered from the committed file with the headers,
//! ranges and digest check of every other pkg ([`super::pkg`]). A second request is a hit, and a second Mac
//! asking while the first read goes on is one origin read in all (the cache's single flight).
//!
//! **What it asks of the origin.** Only the `url` of the entry, never anything of the request, so
//! the digests that are fetched at all are those the operator named. Without `--proxy-cache-dir`,
//! without a `url`, or for a digest no entry names, nothing is asked of any origin: `404`.
//!
//! **A second Mac while the first read goes on** waits for it, at most `proxy_wait_s`; when the
//! wait ends first it is answered `503 pkg_pending` with `Retry-After`; the read the cache began
//! for it is dropped unread, and no second copy of the file is pulled.
//!
//! **When the cache cannot keep the pkg** (the disk is full or fails, or the pkg is larger than
//! `proxy_cache_max_bytes`) the digest was still checked, but there is no file to answer from, so
//! the answer is `503 pkg_not_stored`: the pass-through that streams other files uncached does
//! not apply, since a pkg's status must come before its bytes.

use std::sync::Arc;

use futures::StreamExt as _;
use hyper::header::{HeaderValue, RETRY_AFTER};
use hyper::{Response, StatusCode};
use sha2::{Digest, Sha256};

use super::pkg::failure;
use crate::grpc::RouteBody;
use crate::pkg_origin::PkgOrigin;
use crate::proxy_cache::{
    BodyStream, FetchError, Fetched, Key, Limits, Origin, ProxyCache, ProxyError, Served,
};
use crate::service::Coordinator;

/// An entry of the setting `installs` that names a pkg, as the route needs it.
#[derive(Debug)]
pub(super) struct Install {
    /// Where the pkg is fetched from when it is not provided.
    pub(super) url: Option<String>,
    /// The proxy's limits at the time of the request.
    pub(super) limits: Limits,
}

/// The key of a pkg in the cache: the digest is its identity, whatever origin gave the bytes.
fn key_of(digest: &str) -> Key {
    Key::new("pkg", format!("{digest}.pkg"))
}

/// `body` as it comes, and at its end an error instead of the end when its SHA-256 is not
/// `digest`: the cache commits only a body that ended.
fn verified(body: BodyStream, digest: String) -> BodyStream {
    Box::pin(futures::stream::unfold(
        (body, Some(Sha256::new())),
        move |(mut body, hasher)| {
            let digest = digest.clone();
            async move {
                // No hasher: the body already ended in an error, which was its last frame.
                let mut hasher = hasher?;
                match body.next().await {
                    Some(Ok(bytes)) => {
                        hasher.update(&bytes);
                        Some((Ok(bytes), (body, Some(hasher))))
                    }
                    Some(Err(why)) => Some((Err(why), (body, None))),
                    None if hex::encode(hasher.finalize()) == digest => None,
                    None => Some((
                        Err(FetchError::Failed(
                            "the bytes are not the pkg's digest".to_owned(),
                        )),
                        (body, None),
                    )),
                }
            }
        },
    ))
}

/// The read of the origin the cache starts for a miss: `url` through `origin`, digest-checked.
fn reading(origin: Arc<PkgOrigin>, url: String, digest: String, redirects: u64) -> Origin {
    Arc::new(move |_| {
        let (origin, url, digest) = (Arc::clone(&origin), url.clone(), digest.clone());
        Box::pin(async move {
            let Fetched {
                validator,
                length,
                body,
            } = origin
                .get(&url, redirects)
                .await
                .map_err(|why| FetchError::Failed(why.to_string()))?;
            Ok(Fetched {
                validator,
                length,
                body: verified(body, digest),
            })
        })
    })
}

/// Reads the pkg into the cache: nothing is read when it is there; otherwise the origin is read
/// to its end, so that a body that is not the digest's is an error before anything is sent.
async fn fill(
    cache: &ProxyCache,
    key: &Key,
    origin: &Origin,
    limits: &Limits,
) -> Result<(), Response<RouteBody>> {
    let answer = cache.get(key, None, origin, limits).await.map_err(|why| {
        tracing::warn!(%why, "a pkg could not be read from its origin");
        match why {
            ProxyError::Busy => failure(StatusCode::SERVICE_UNAVAILABLE, "busy"),
            _ => failure(StatusCode::BAD_GATEWAY, "origin_failed"),
        }
    })?;
    match answer.served {
        Served::Hit => return Ok(()),
        // The wait for another asker's fill ended before it did. A pkg is checked whole before it
        // is answered, so reading it again for this asker alone would be the second pull of the
        // same file that the single flight exists to prevent: the Mac is asked to come back.
        Served::Passed => {
            drop(answer);
            let mut response = failure(StatusCode::SERVICE_UNAVAILABLE, "pkg_pending");
            response
                .headers_mut()
                .insert(RETRY_AFTER, HeaderValue::from(limits.wait.as_secs().max(1)));
            return Err(response);
        }
        Served::Filled => {}
    }
    let mut body = answer.body;
    while let Some(frame) = body.next().await {
        if let Err(why) = frame {
            tracing::warn!(%why, "a pkg was not read whole from its origin; nothing was kept");
            return Err(failure(StatusCode::BAD_GATEWAY, "origin_failed"));
        }
    }
    Ok(())
}

/// The file of the pkg `digest` in the cache, read from the `url` of `install` first when it is
/// not there; the answer to send instead when there is none.
pub(super) async fn pull(
    coordinator: &Coordinator,
    digest: &str,
    install: &Install,
) -> Result<tokio::fs::File, Response<RouteBody>> {
    let (Some(url), Some(cache), Some(origin)) = (
        &install.url,
        coordinator.proxy_cache(),
        coordinator.pkg_origin(),
    ) else {
        tracing::warn!(%digest, "an install names a pkg that is not in the pkg directory and cannot be fetched");
        return Err(failure(StatusCode::NOT_FOUND, "not_found"));
    };
    let key = key_of(digest);
    if let Some(file) = cache.open_hit(&key) {
        return Ok(tokio::fs::File::from_std(file));
    }
    let origin = reading(
        Arc::clone(origin),
        url.clone(),
        digest.to_owned(),
        install.limits.redirects,
    );
    fill(cache, &key, &origin, &install.limits).await?;
    cache
        .open_hit(&key)
        .map(tokio::fs::File::from_std)
        .ok_or_else(|| {
            tracing::warn!(%digest, "a pkg was read whole and checked, and the cache did not keep it");
            failure(StatusCode::SERVICE_UNAVAILABLE, "pkg_not_stored")
        })
}

#[cfg(test)]
#[path = "pkg_pull_tests.rs"]
mod tests;
