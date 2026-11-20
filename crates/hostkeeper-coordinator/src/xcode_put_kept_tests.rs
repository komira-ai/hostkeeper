//! `PUT /v1/xcode/<sha256>` of a release the store would remove at once: older than the releases
//! it keeps. The coordinator, the requests and the bodies are in `xcode_put_support_tests.rs`.

use bytes::Bytes;
use http_body_util::Full;
use hyper::StatusCode;
use serde_json::json;
use std::sync::atomic::Ordering;

use super::lands_meanwhile;
use super::support::{
    ARCHIVE, OTHER, bearer, data, digest_of, farm, files_of, listing, request, uri,
};
use crate::testutil::OPERATOR;

/// Proves: with the store holding 27.2 and no group keeping Xcode (the store keeps one release),
/// a push of 27.1, which the list holds as a release, is a 409 `not_kept` whose body is never
/// read; the store is as it was (27.2's two files alone) and no `xcode_put` entry is audited.
///
/// Catches: an upload of a release that the prune removes in the same request, after the body was
/// received, stored and audited.
#[tokio::test]
async fn an_upload_older_than_the_kept_releases_is_refused_and_stores_nothing() {
    let farm = farm("xcode-put-not-kept");
    lands_meanwhile(&farm.store, OTHER, "27.2", "27B52")();
    let body = data(&[ARCHIVE]);
    let polled = body.polled.clone();
    let asked = request(
        "PUT",
        &uri(&digest_of(ARCHIVE), "27.1", "27B40"),
        bearer(OPERATOR),
        body,
    );

    let answered = farm.send(asked).await;

    assert_eq!(
        answered,
        (StatusCode::CONFLICT, json!({"error": "not_kept"}))
    );
    assert!(!polled.load(Ordering::SeqCst), "the body was read");
    assert_eq!(listing(&farm.dir), files_of(&digest_of(OTHER)));
    assert_eq!(farm.puts(), Vec::new());
}

/// Proves: a newer release committed while the body of an older one streamed makes the older one
/// a 409 `not_kept` at the commit: it is neither stored nor audited and its partial is removed;
/// the newer archive stands.
///
/// Catches: a commit that trusts the check made before the body was read, and so stores a release
/// the prune removes at once.
#[tokio::test]
async fn a_newer_release_landed_while_the_body_streamed_makes_the_upload_not_kept() {
    let farm = farm("xcode-put-not-kept-race");
    let mut body = data(&[ARCHIVE]);
    body.at_end = Some(lands_meanwhile(&farm.store, OTHER, "27.2", "27B52"));
    let asked = request(
        "PUT",
        &uri(&digest_of(ARCHIVE), "27.1", "27B40"),
        bearer(OPERATOR),
        body,
    );

    let answered = farm.send(asked).await;

    assert_eq!(
        answered,
        (StatusCode::CONFLICT, json!({"error": "not_kept"}))
    );
    assert_eq!(listing(&farm.dir), files_of(&digest_of(OTHER)));
    assert_eq!(farm.puts(), Vec::new());
}

/// Proves: the same push of 27.1 is stored when the store holds nothing newer.
///
/// Catches: a refusal that applies to every older release rather than to those the store would
/// remove.
#[tokio::test]
async fn an_older_release_is_stored_when_nothing_newer_is_held() {
    let farm = farm("xcode-put-kept-alone");
    let asked = request(
        "PUT",
        &uri(&digest_of(ARCHIVE), "27.1", "27B40"),
        bearer(OPERATOR),
        Full::new(Bytes::from_static(ARCHIVE)),
    );

    assert_eq!(farm.send(asked).await.0, StatusCode::CREATED);
    assert_eq!(listing(&farm.dir), files_of(&digest_of(ARCHIVE)));
}
