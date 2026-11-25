//! `PUT /v1/xcode/<sha256>` over a coordinator with an Xcode store, driven through the router
//! ([`crate::grpc::route`]) on the private surface: a good push, each refusal and what it leaves
//! in the store's directory, the audit line, the alert it clears or raises, and the log. The
//! coordinator, the requests and the bodies are in `xcode_put_support_tests.rs`.

use std::collections::VecDeque;
use std::fs;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use http_body_util::Full;
use hyper::StatusCode;
use hyper::header::CONTENT_LENGTH;
use serde_json::json;
use tokio::io::AsyncWrite;

use super::receive;
use crate::audit_actions::XCODE_REMOVE;
use crate::grpc::route;
use crate::http::{Peer, Surface};
use crate::marks_support::operator;
use crate::testutil::{OPERATOR, Rig, ci_token, ops_of};
use crate::wait::ConnectionWaits;
use crate::xcode_feed::Channel;
use crate::xcode_store::{Archive, Taken, XcodeStore};

#[path = "xcode_put_support_tests.rs"]
mod support;

#[path = "xcode_put_kept_tests.rs"]
mod kept;

#[path = "xcode_put_dropped_tests.rs"]
mod dropped;

#[path = "xcode_put_range_tests.rs"]
mod range;

#[path = "xcode_put_range_remove_tests.rs"]
mod range_remove;

use support::{
    ARCHIVE, Chunks, Farm, OTHER, PATIENCE, Piece, ROOMY, SIXTEEN, Unreadable, bearer, builder,
    data, digest_of, farm, farm_with, feed, files_of, good_uri, id, lines, listing, polled, push,
    refusing, release, request, updater, uri,
};

/// The alerts of the Xcode store that the coordinator of `farm` lists now, as the kind and the
/// version each names.
fn xcode_alerts(farm: &Farm) -> Vec<(proto::AlertKind, String)> {
    farm.coordinator
        .list_alerts(&operator(), proto::ListAlertsRequest {})
        .expect("the alerts are listed")
        .alerts
        .iter()
        .filter(|alert| {
            matches!(
                alert.kind(),
                proto::AlertKind::XcodeReleaseWaiting | proto::AlertKind::XcodeVersionTaken
            )
        })
        .map(|alert| (alert.kind(), alert.version.clone()))
        .collect()
}

/// Proves: a push of a release the list holds, by the operator's token, is stored (the archive
/// holds exactly the bytes sent, beside its record), listed by the store with the token's name,
/// answered 201 with its SHA-256, version, build and size, audited once as `xcode_put` by the
/// token's name with the release, the SHA-256 and the size as the reason, and the alert
/// `xcode_release_waiting` for that release is gone.
///
/// Catches: an archive stored under a name other than its SHA-256, one stored without its record,
/// a store that does not say the release is held, an upload with no audit line or two, and an
/// audit line that names no archive.
#[tokio::test]
async fn a_good_push_is_stored_listed_audited_and_clears_the_waiting_alert() {
    let farm = farm("xcode-put-good");
    let sha = digest_of(ARCHIVE);
    assert_eq!(
        farm.coordinator
            .xcode
            .release_waiting()
            .map(|release| release.id),
        Some(id("27.2", "27B52"))
    );
    assert_eq!(
        xcode_alerts(&farm),
        vec![(proto::AlertKind::XcodeReleaseWaiting, "27.2".to_owned())]
    );

    let (status, body) = farm.send(push(ARCHIVE)).await;

    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        body,
        json!({"sha256": sha, "version": "27.2", "build": "27B52", "size": ARCHIVE.len()})
    );
    assert_eq!(
        fs::read(farm.store.archive_path(&sha)).expect("the archive"),
        ARCHIVE
    );
    assert_eq!(listing(&farm.dir), files_of(&sha));
    let archives = farm.store.archives().expect("the archives");
    assert_eq!(archives.len(), 1, "{archives:?}");
    assert_eq!(archives[0].sha256, sha);
    assert_eq!(archives[0].id, id("27.2", "27B52"));
    assert_eq!(
        archives[0].size,
        u64::try_from(ARCHIVE.len()).expect("a size")
    );
    assert_eq!(archives[0].by, "operator");
    assert_eq!(farm.coordinator.xcode.release_waiting(), None);
    assert_eq!(xcode_alerts(&farm), vec![]);
    let listed = farm
        .coordinator
        .xcode_list(&operator(), &proto::ListXcodeRequest {})
        .expect("the store is listed");
    let archives: Vec<(&str, &str, &str, &str)> = listed
        .archives
        .iter()
        .map(|a| {
            (
                a.sha256.as_str(),
                a.version.as_str(),
                a.build.as_str(),
                a.uploaded_by.as_str(),
            )
        })
        .collect();
    assert_eq!(archives, vec![(sha.as_str(), "27.2", "27B52", "operator")]);
    assert_eq!(listed.waiting, None);
    let puts = farm.puts();
    assert_eq!(puts.len(), 1, "{puts:?}");
    assert_eq!(puts[0].by, "operator");
    assert_eq!(puts[0].api_token, None);
    assert_eq!(
        puts[0].reason.as_deref(),
        Some(format!("Xcode 27.2 (27B52), sha256 {sha}, {} bytes", ARCHIVE.len()).as_str())
    );
}

/// Proves: a push that arrives as several data frames with a trailers frame among them is stored
/// as the data frames' bytes in order, and the trailers are neither written nor hashed.
///
/// Catches: a copy that stops at the first frame, one that writes a trailers frame's nothing as
/// data or fails on it, and a digest taken over a different sequence than the file's.
#[tokio::test]
async fn a_body_in_frames_with_trailers_is_stored_whole() {
    let farm = farm("xcode-put-frames");
    let sha = digest_of(ARCHIVE);
    let body = Chunks {
        pieces: VecDeque::from([
            Piece::Data(b"the bytes of ".to_vec()),
            Piece::Trailers,
            Piece::Data(b"an Xcode archive".to_vec()),
        ]),
        ..Chunks::default()
    };

    let (status, _) = farm
        .send(request("PUT", &good_uri(ARCHIVE), bearer(OPERATOR), body))
        .await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        fs::read(farm.store.archive_path(&sha)).expect("the archive"),
        ARCHIVE
    );
}

/// Proves: only `PUT` is answered: any other method on the path, with the operator's token, is a
/// 405 `method_not_allowed` and stores nothing.
///
/// Catches: an upload taken from a `POST`, and a route that answers a read of the store to a
/// caller on the private listener.
#[tokio::test]
async fn only_put_is_answered() {
    let farm = farm("xcode-put-method");
    for method in ["GET", "HEAD", "POST", "DELETE", "PATCH"] {
        let asked = request(
            method,
            &good_uri(ARCHIVE),
            bearer(OPERATOR),
            Full::new(Bytes::from_static(ARCHIVE)),
        );
        farm.refused(
            method,
            asked,
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
        )
        .await;
    }
}

/// Proves: a request that presents a client certificate is a 403 `forbidden`, with the operator's
/// token and without one, and stores nothing.
///
/// Catches: a host certificate that may upload, and a token that is honoured beside a
/// certificate.
#[tokio::test]
async fn a_client_certificate_is_refused_even_with_a_valid_token() {
    let farm = farm("xcode-put-certificate");
    for (what, authorization) in [("a token", bearer(OPERATOR)), ("no token", None)] {
        let asked = request(
            "PUT",
            &good_uri(ARCHIVE),
            authorization,
            Full::new(Bytes::from_static(ARCHIVE)),
        );
        let answered = farm.send_within(&updater(), asked, PATIENCE).await;
        assert_eq!(
            answered,
            (StatusCode::FORBIDDEN, json!({"error": "forbidden"})),
            "{what}"
        );
        farm.assert_untouched(what);
    }
}

/// Proves: no token, a token the coordinator does not know, a header that is not a bearer, and
/// CI's token are each a 401 `unauthorized`, and the body is never asked for a frame.
///
/// Catches: an upload read before the caller is known, a token check that lets CI upload, and a
/// scheme other than `Bearer` that is accepted.
#[tokio::test]
async fn a_caller_without_the_operators_token_is_refused_before_the_body_is_read() {
    let farm = farm("xcode-put-token");
    let cases = [
        ("no token", None),
        ("a wrong token", bearer("wrong-token-0123456789abcdef")),
        ("CI's token", bearer(&ci_token())),
        ("no scheme", Some(OPERATOR.to_owned())),
    ];
    for (what, authorization) in cases {
        let body = data(&[ARCHIVE]);
        let polled = Arc::clone(&body.polled);
        let asked = request("PUT", &good_uri(ARCHIVE), authorization, body);
        farm.refused(what, asked, StatusCode::UNAUTHORIZED, "unauthorized")
            .await;
        assert!(!polled.load(Ordering::SeqCst), "{what}: the body was read");
    }
}

/// Proves: a path that is not the prefix and 64 lowercase hex characters (too short, upper case,
/// a character outside hex, a further segment, nothing) is a 400 `bad_request` and stores
/// nothing.
///
/// Catches: an archive named by a path that is not its SHA-256, and a path with a segment below
/// it that reaches another file of the directory.
#[tokio::test]
async fn a_path_that_is_not_a_sha256_is_refused() {
    let farm = farm("xcode-put-path");
    let sha = digest_of(ARCHIVE);
    let query = "?version=27.2&build=27B52";
    let paths = [
        format!("/v1/xcode/{}{query}", &sha[..63]),
        format!("/v1/xcode/{}{query}", sha.to_uppercase()),
        format!("/v1/xcode/{}g{query}", &sha[..63]),
        format!("/v1/xcode/{sha}/x{query}"),
        format!("/v1/xcode/../{sha}{query}"),
        format!("/v1/xcode/{query}"),
    ];
    for path in paths {
        let asked = request(
            "PUT",
            &path,
            bearer(OPERATOR),
            Full::new(Bytes::from_static(ARCHIVE)),
        );
        farm.refused(&path, asked, StatusCode::BAD_REQUEST, "bad_request")
            .await;
    }
}

/// Proves: the query must be exactly `version` and `build`, each once, with a version of
/// dot-separated numbers and a build of one to 32 letters, digits and dots: no query, a missing
/// key, another key, a repeated key, a pair without a value, an empty pair, an empty value, a
/// version that is not numbers, a percent escape and a build with another character or past 32
/// bytes are each a 400 `bad_request` that stores nothing.
///
/// Catches: a release named by something other than the query, a duplicate key whose first or last
/// wins, a value decoded into something the grammar refused, and a build too long for a record.
#[tokio::test]
async fn the_query_names_exactly_a_version_and_a_build() {
    let farm = farm("xcode-put-query");
    let sha = digest_of(ARCHIVE);
    let long_build = "B".repeat(33);
    let queries = [
        String::new(),
        "?version=27.2".to_owned(),
        "?build=27B52".to_owned(),
        "?version=27.2&build=27B52&other=1".to_owned(),
        "?version=27.2&version=27.2&build=27B52".to_owned(),
        "?version=27.2&build=27B52&build=27B52".to_owned(),
        "?version=27.2&build".to_owned(),
        "?version=27.2&&build=27B52".to_owned(),
        "?version=&build=27B52".to_owned(),
        "?version=27.2&build=".to_owned(),
        "?version=27.x&build=27B52".to_owned(),
        "?version=27%2E2&build=27B52".to_owned(),
        "?version=27.2&build=27-B52".to_owned(),
        "?version=27.2&build=27%42%35%32".to_owned(),
        format!("?version=27.2&build={long_build}"),
    ];
    for query in queries {
        let asked = request(
            "PUT",
            &format!("/v1/xcode/{sha}{query}"),
            bearer(OPERATOR),
            Full::new(Bytes::from_static(ARCHIVE)),
        );
        farm.refused(&query, asked, StatusCode::BAD_REQUEST, "bad_request")
            .await;
    }
}

/// Proves: a `Content-Length` over the store's cap is a 413 `too_large` answered before the body
/// is asked for a frame; an archive of exactly the cap, with its length declared, is stored.
///
/// Catches: a cap read from the body instead of the header, a cap that refuses what it allows, and
/// a refusal that has already started to write.
#[tokio::test]
async fn a_declared_length_over_the_cap_is_refused_unread_and_one_at_the_cap_is_stored() {
    let farm = farm_with("xcode-put-declared", 16, |coordinator, _| coordinator);
    let body = data(&[SIXTEEN]);
    let polled = Arc::clone(&body.polled);
    let asked = builder("PUT", &good_uri(SIXTEEN), bearer(OPERATOR))
        .header(CONTENT_LENGTH, "17")
        .body(body)
        .expect("a request");
    farm.refused(
        "17 of 16",
        asked,
        StatusCode::PAYLOAD_TOO_LARGE,
        "too_large",
    )
    .await;
    assert!(!polled.load(Ordering::SeqCst), "the body was read");

    let asked = builder("PUT", &good_uri(SIXTEEN), bearer(OPERATOR))
        .header(CONTENT_LENGTH, "16")
        .body(Full::new(Bytes::from_static(SIXTEEN)))
        .expect("a request");
    let (status, _) = farm.send(asked).await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        fs::read(farm.store.archive_path(&digest_of(SIXTEEN))).expect("the archive"),
        SIXTEEN
    );
}

/// Proves: a body that passes the cap as it streams, with no `Content-Length` or one that cannot
/// be read, is a 413 `too_large`, and the partial upload is removed.
///
/// Catches: a cap enforced only by the header, a streamed body written whole before it is
/// counted, and a partial file left behind.
#[tokio::test]
async fn a_streamed_body_over_the_cap_is_refused_and_its_partial_removed() {
    let farm = farm_with("xcode-put-streamed", 16, |coordinator, _| coordinator);
    for length in [None, Some("many")] {
        let mut asked = builder("PUT", &good_uri(SIXTEEN), bearer(OPERATOR));
        if let Some(length) = length {
            asked = asked.header(CONTENT_LENGTH, length);
        }
        let asked = asked
            .body(data(&[b"01234567", b"89abcdef", b"x"]))
            .expect("a request");
        farm.refused(
            &format!("{length:?}"),
            asked,
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
        )
        .await;
    }
}

/// Proves: a release the list does not hold is a 409 `not_listed`, whether its version or its
/// build is not listed, whether it is a beta of the list, or whether the list is empty; nothing is
/// stored.
///
/// Catches: a store that takes whatever an operator names, one that takes a beta, and a match on
/// the version alone.
#[tokio::test]
async fn a_release_the_list_does_not_hold_is_refused_not_listed() {
    let farm = farm("xcode-put-listed");
    let sha = digest_of(ARCHIVE);
    for (version, build) in [("27.3", "27C1"), ("28.0", "28A5"), ("27.2", "27B53")] {
        let asked = request(
            "PUT",
            &uri(&sha, version, build),
            bearer(OPERATOR),
            Full::new(Bytes::from_static(ARCHIVE)),
        );
        farm.refused(
            &format!("{version} ({build})"),
            asked,
            StatusCode::CONFLICT,
            "not_listed",
        )
        .await;
    }
    farm.coordinator
        .xcode
        .known
        .keep(&[])
        .expect("an empty list is kept");
    farm.refused(
        "an empty list",
        push(ARCHIVE),
        StatusCode::CONFLICT,
        "not_listed",
    )
    .await;
}

/// Proves: a list the coordinator cannot read is a 500 `store_failed` and stores nothing.
///
/// Catches: an upload accepted because the list could not be checked.
#[tokio::test]
async fn an_unreadable_list_is_refused_store_failed() {
    let farm = farm_with("xcode-put-list-down", ROOMY, |coordinator, _| {
        coordinator.with_known_xcode_releases(Arc::new(Unreadable))
    });
    farm.refused(
        "the list",
        push(ARCHIVE),
        StatusCode::INTERNAL_SERVER_ERROR,
        "store_failed",
    )
    .await;
}

/// Proves: a store whose directory cannot be read is a 500 `store_failed`, and a name an upload
/// file is to take that is already in the directory is the same: neither stores nor audits.
///
/// Catches: an upload that goes on when the store cannot say what it holds, and one that
/// overwrites a file of the directory it did not make.
#[tokio::test]
async fn a_store_that_cannot_be_read_or_written_is_refused_store_failed() {
    let farm = farm("xcode-put-store-down");
    let sha = digest_of(ARCHIVE);
    let (first, file) = farm.store.upload_file(&sha).expect("an upload file");
    drop(file);
    fs::remove_file(&first).expect("the first upload file goes");
    let first = first.file_name().expect("a name").to_string_lossy();
    let next = first.strip_suffix("-0").expect("the count ends the name");
    let taken = format!("{next}-1");
    fs::write(farm.dir.join(&taken), b"").expect("a file in the way");
    let (status, body) = farm.send(push(ARCHIVE)).await;
    assert_eq!(
        (status, body),
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": "store_failed"})
        )
    );
    assert_eq!(listing(&farm.dir), vec![taken.clone()]);
    fs::remove_file(farm.dir.join(&taken)).expect("removed");

    fs::remove_dir(&farm.dir).expect("the empty directory removed");
    let (status, body) = farm.send(push(ARCHIVE)).await;
    assert_eq!(
        (status, body),
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": "store_failed"})
        )
    );
    assert_eq!(farm.puts(), Vec::new());
}

/// A writer whose writes fail, or, when `on_write` is not set, whose flush does.
struct Failing {
    on_write: bool,
}

impl AsyncWrite for Failing {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(if self.on_write {
            Err(io::Error::other("the disk is full"))
        } else {
            Ok(buf.len())
        })
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(if self.on_write {
            Ok(())
        } else {
            Err(io::Error::other("the disk is full"))
        })
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// Proves: a body whose bytes cannot be written to the upload file, and one whose bytes cannot be
/// flushed to it, are each refused 500 `store_failed` rather than taken as stored.
///
/// Catches: a failed write or flush of the upload that is ignored, so that a short file is
/// committed under the digest of the whole.
#[tokio::test]
async fn a_write_or_a_flush_that_fails_is_refused_store_failed() {
    for (what, on_write) in [("a write", true), ("a flush", false)] {
        let mut out = Failing { on_write };

        let refused = receive(data(&[ARCHIVE]), &mut out, ROOMY, PATIENCE).await;

        assert_eq!(
            refused.err(),
            Some((StatusCode::INTERNAL_SERVER_ERROR, "store_failed")),
            "{what}"
        );
    }
}

/// Proves: a release the store already holds is a 409 `already_held`, whether it is pushed again
/// as the same bytes, as other bytes, or as other bytes of the release that has the same
/// SHA-256 as the held archive; the store's two files stay as they were and no second upload is
/// audited.
///
/// Catches: a second archive of a release, one archive stored under two releases, and a push that
/// replaces what is held.
#[tokio::test]
async fn a_release_the_store_holds_is_refused_already_held() {
    let farm = farm("xcode-put-held");
    let sha = digest_of(ARCHIVE);
    assert_eq!(farm.send(push(ARCHIVE)).await.0, StatusCode::CREATED);
    let kept = listing(&farm.dir);
    assert_eq!(kept, files_of(&sha));

    let pushes = [
        ("the same bytes", good_uri(ARCHIVE), ARCHIVE),
        ("other bytes", good_uri(OTHER), OTHER),
        (
            "the same bytes as another release",
            uri(&sha, "27.1", "27B40"),
            ARCHIVE,
        ),
    ];
    for (what, path, bytes) in pushes {
        let asked = request(
            "PUT",
            &path,
            bearer(OPERATOR),
            Full::new(Bytes::from_static(bytes)),
        );
        let answered = farm.send(asked).await;
        assert_eq!(
            answered,
            (StatusCode::CONFLICT, json!({"error": "already_held"})),
            "{what}"
        );
        assert_eq!(listing(&farm.dir), kept, "{what}");
    }
    assert_eq!(farm.puts().len(), 1);
    assert_eq!(farm.store.taken(), Vec::new());
}

/// Proves: a second build under a version the store holds is a 409 `xcode_version_taken`, stores
/// nothing, stands as the coordinator's record of a refused build (what the alert
/// `xcode_version_taken` is derived from), and is logged with the version and both builds.
///
/// Catches: two builds under one version, a refusal that leaves no alert, and an alert naming
/// the wrong build.
#[tokio::test]
async fn a_second_build_under_a_held_version_is_refused_and_recorded() {
    let farm = farm("xcode-put-taken");
    let mut list = feed();
    list.push(release("27.2", "27B53", Channel::Release));
    farm.coordinator
        .xcode
        .known
        .keep(&list)
        .expect("the list is kept");
    assert_eq!(farm.send(push(ARCHIVE)).await.0, StatusCode::CREATED);
    let kept = listing(&farm.dir);
    assert_eq!(farm.coordinator.xcode_taken(), Vec::new());

    let asked = request(
        "PUT",
        &uri(&digest_of(OTHER), "27.2", "27B53"),
        bearer(OPERATOR),
        Full::new(Bytes::from_static(OTHER)),
    );
    let answered = farm.send(asked).await;

    assert_eq!(
        answered,
        (
            StatusCode::CONFLICT,
            json!({"error": "xcode_version_taken"})
        )
    );
    assert_eq!(listing(&farm.dir), kept);
    assert!(
        xcode_alerts(&farm).contains(&(proto::AlertKind::XcodeVersionTaken, "27.2".to_owned())),
        "{:?}",
        xcode_alerts(&farm)
    );
    assert_eq!(
        farm.coordinator.xcode_taken(),
        vec![Taken {
            version: "27.2".to_owned(),
            held_build: "27B52".to_owned(),
            refused_build: "27B53".to_owned(),
        }]
    );
    let warned = " WARN hostkeeper_coordinator::xcode_put: an Xcode archive was refused: the store holds another build of that version version=27.2 held_build=27B52 refused_build=27B53";
    assert!(
        lines(&farm.log).iter().any(|line| line == warned),
        "{}",
        farm.log.text()
    );
    assert_eq!(farm.puts().len(), 1);
}

/// Proves: a push that leaves the store with more releases than it keeps (one, while no group
/// keeps Xcode) prunes the older one: only the newest release's two files stay, both pushes are
/// audited as `xcode_put`, and the older archive's removal is one `xcode_remove` entry by
/// `coordinator` that names the release and the SHA-256 as pruned.
///
/// Catches: a push that does not prune, a prune that removes the release just pushed, and a
/// removal that is not audited.
#[tokio::test]
async fn a_push_prunes_the_releases_the_store_no_longer_keeps() {
    let farm = farm("xcode-put-prune");
    let older = request(
        "PUT",
        &uri(&digest_of(OTHER), "27.1", "27B40"),
        bearer(OPERATOR),
        Full::new(Bytes::from_static(OTHER)),
    );
    assert_eq!(farm.send(older).await.0, StatusCode::CREATED);
    assert_eq!(listing(&farm.dir), files_of(&digest_of(OTHER)));

    assert_eq!(farm.send(push(ARCHIVE)).await.0, StatusCode::CREATED);

    assert_eq!(listing(&farm.dir), files_of(&digest_of(ARCHIVE)));
    assert_eq!(farm.puts().len(), 2);
    let removals: Vec<(String, Option<String>)> = farm
        .coordinator
        .ops
        .audit_after(0)
        .expect("the audit")
        .into_iter()
        .filter(|entry| entry.action == XCODE_REMOVE)
        .map(|entry| (entry.by, entry.reason))
        .collect();
    assert_eq!(
        removals,
        vec![(
            "coordinator".to_owned(),
            Some(format!(
                "pruned: Xcode 27.1 (27B40), sha256 {}",
                digest_of(OTHER)
            ))
        )]
    );
}

/// Proves: a body whose SHA-256 is not the path's is a 400 `digest_mismatch`, the partial upload
/// is removed, and the version is free for the push that follows, which is stored.
///
/// Catches: an archive stored under a name that is not its digest, a partial file left behind,
/// and a version reserved by an upload that failed.
#[tokio::test]
async fn a_body_that_is_not_its_paths_digest_is_refused_and_frees_the_version() {
    let farm = farm("xcode-put-digest");
    let asked = request(
        "PUT",
        &good_uri(OTHER),
        bearer(OPERATOR),
        Full::new(Bytes::from_static(ARCHIVE)),
    );
    farm.refused(
        "the digest of other bytes",
        asked,
        StatusCode::BAD_REQUEST,
        "digest_mismatch",
    )
    .await;

    assert_eq!(farm.send(push(ARCHIVE)).await.0, StatusCode::CREATED);
}

/// Proves: a body that fails after it has begun, and one that fails at once, are each a 400
/// `bad_request` that leaves nothing in the store's directory.
///
/// Catches: a partial upload left when the client goes away, and a failed body taken as complete.
#[tokio::test]
async fn a_broken_body_is_refused_and_its_partial_removed() {
    let farm = farm("xcode-put-broken");
    let after_data = Chunks {
        pieces: VecDeque::from([Piece::Data(ARCHIVE.to_vec()), Piece::Fail]),
        ..Chunks::default()
    };
    let at_once = Chunks {
        pieces: VecDeque::from([Piece::Fail]),
        ..Chunks::default()
    };
    for (what, body) in [("after data", after_data), ("at once", at_once)] {
        let asked = request("PUT", &good_uri(ARCHIVE), bearer(OPERATOR), body);
        farm.refused(what, asked, StatusCode::BAD_REQUEST, "bad_request")
            .await;
    }
}

/// Proves: a body that gives some bytes and then nothing for more than the patience is a 400
/// `bad_request` and its partial upload is removed.
///
/// Catches: an upload that waits for ever on a client that has stopped sending, and a partial
/// file left by it.
#[tokio::test(start_paused = true)]
async fn a_stalled_body_is_refused_and_its_partial_removed() {
    let farm = farm("xcode-put-stalled");
    let mut body = data(&[ARCHIVE]);
    body.stalls = true;
    let asked = request("PUT", &good_uri(ARCHIVE), bearer(OPERATOR), body);

    let answered = farm
        .send_within(&None, asked, Duration::from_millis(50))
        .await;

    assert_eq!(
        answered,
        (StatusCode::BAD_REQUEST, json!({"error": "bad_request"}))
    );
    farm.assert_untouched("a stalled body");
}

/// Proves: while an upload of a version is in progress another push of the version is a 409
/// `upload_in_progress` that stores nothing, and once the first is over the push is taken.
///
/// Catches: two uploads of one version that run at once, and a reservation that outlives its
/// upload.
#[tokio::test]
async fn a_second_upload_of_a_version_in_progress_is_refused() {
    let farm = farm("xcode-put-busy");
    let reserved = farm.store.reserve("27.2").expect("the version is free");

    farm.refused(
        "a busy version",
        push(ARCHIVE),
        StatusCode::CONFLICT,
        "upload_in_progress",
    )
    .await;

    drop(reserved);
    assert_eq!(farm.send(push(ARCHIVE)).await.0, StatusCode::CREATED);
}

/// An upload of `bytes` as Xcode `version` of `build` that lands in the store of `store` itself:
/// what another coordinator's push, or the same operator's second one, would have committed.
fn lands_meanwhile(
    store: &Arc<XcodeStore>,
    bytes: &'static [u8],
    version: &str,
    build: &str,
) -> Box<dyn FnOnce() + Send> {
    let store = Arc::clone(store);
    let (version, build) = (version.to_owned(), build.to_owned());
    Box::new(move || {
        let sha256 = digest_of(bytes);
        let (path, file) = store.upload_file(&sha256).expect("an upload file");
        drop(file);
        fs::write(&path, bytes).expect("the bytes");
        let archive = Archive {
            sha256,
            id: id(&version, &build),
            size: u64::try_from(bytes.len()).expect("a size"),
            by: "elsewhere".to_owned(),
            at_unix: 1,
        };
        let _changing = store.changing();
        store.commit(&path, &archive).expect("committed");
    })
}

/// Proves: what the store holds is read again when the upload is committed: another build of the
/// version committed while the body streamed is a 409 `xcode_version_taken`, and the same release
/// committed meanwhile is a 409 `already_held`; the push is neither stored nor audited, its partial
/// is removed, and the archive committed meanwhile stands untouched.
///
/// Catches: two archives of one release by two pushes that raced, and a commit that trusts the
/// check made before the body was read.
#[tokio::test]
async fn what_landed_while_the_body_streamed_is_checked_again_at_the_commit() {
    let cases = [("27B53", "xcode_version_taken"), ("27B52", "already_held")];
    for (n, (build, code)) in cases.into_iter().enumerate() {
        let farm = farm(&format!("xcode-put-race-{n}"));
        let mut body = data(&[ARCHIVE]);
        body.at_end = Some(lands_meanwhile(&farm.store, OTHER, "27.2", build));
        let asked = request("PUT", &good_uri(ARCHIVE), bearer(OPERATOR), body);

        let answered = farm.send(asked).await;

        assert_eq!(
            answered,
            (StatusCode::CONFLICT, json!({"error": code})),
            "{build}"
        );
        assert_eq!(listing(&farm.dir), files_of(&digest_of(OTHER)), "{build}");
        assert_eq!(farm.puts(), Vec::new(), "{build}");
    }
}

/// Proves: an upload whose audit line cannot be appended is a 503 `audit_unavailable` that stores
/// nothing and leaves the release waiting.
///
/// Catches: an archive stored that the audit does not say, and an audit failure answered as
/// success or as a bad request.
#[tokio::test]
async fn an_upload_that_cannot_be_audited_is_refused_and_stores_nothing() {
    let farm = farm_with("xcode-put-audit", ROOMY, |coordinator, dir| {
        coordinator.with_ops(ops_of(dir).with_append(refusing()))
    });

    farm.refused(
        "a full audit",
        push(ARCHIVE),
        StatusCode::SERVICE_UNAVAILABLE,
        "audit_unavailable",
    )
    .await;

    assert_eq!(farm.store.archives().expect("the archives"), Vec::new());
    assert!(farm.coordinator.xcode.release_waiting().is_some());
}

/// Proves: when the archive cannot be committed after its audit line is written, the answer is a
/// 500 `store_failed`, the upload file is removed, and no archive is held.
///
/// Catches: a failed commit answered as success, and an upload file left in the directory.
#[tokio::test]
async fn a_commit_that_fails_is_refused_store_failed_and_leaves_no_upload() {
    let farm = farm("xcode-put-commit");
    let sha = digest_of(ARCHIVE);
    fs::create_dir(farm.dir.join(format!("{sha}.json")))
        .expect("a directory in the record's place");

    let (status, body) = farm.send(push(ARCHIVE)).await;

    assert_eq!(
        (status, body),
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": "store_failed"})
        )
    );
    let left = listing(&farm.dir);
    assert!(
        left.iter()
            .all(|name| !name.starts_with(".upload-") && !name.ends_with(".xip")),
        "{left:?}"
    );
    assert_eq!(farm.store.archives().expect("the archives"), Vec::new());
}

/// Proves: while the body streams the connection the request came on is held, and it is
/// released when the answer is made: the connection's quiet period does not end during a stalled
/// upload and ends after it.
///
/// Catches: an HTTP/2 connection that is cut a patience after its start in the middle of a long
/// upload, and a hold that is never given back.
#[tokio::test(start_paused = true)]
async fn the_connection_is_held_while_the_body_streams() {
    let farm = farm("xcode-put-held-connection");
    let waits = ConnectionWaits::default();
    let body = Chunks {
        stalls: true,
        ..Chunks::default()
    };
    let mut asked = request("PUT", &good_uri(ARCHIVE), bearer(OPERATOR), body);
    asked.extensions_mut().insert(waits.clone());
    let mut quiet = Box::pin(waits.quiet_for(Duration::from_secs(1)));
    let nobody: Peer = None;

    let answered = tokio::select! {
        biased;
        () = &mut quiet => None,
        sent = farm.send_within(&nobody, asked, Duration::from_secs(10)) => Some(sent),
    };

    let answered = answered.expect("the connection was quiet while the body streamed");
    assert_eq!(
        answered,
        (StatusCode::BAD_REQUEST, json!({"error": "bad_request"}))
    );
    assert!(polled(&mut quiet).await.is_pending());
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(polled(&mut quiet).await.is_ready());
}

/// Proves: a stored upload logs one line that names the release, the SHA-256, the size and who
/// uploaded it, exactly; the refusals of a missing token, a wrong one, an unlisted release and a bad
/// path log nothing of this route; and neither the operator's token nor the wrong one is in any
/// line.
///
/// Catches: a token written to a log by the route or by the call it makes, and a refusal that logs
/// what the caller sent.
#[tokio::test]
async fn the_token_is_in_no_log_line() {
    let farm = farm("xcode-put-log");
    let sha = digest_of(ARCHIVE);
    let wrong = "wrong-token-0123456789abcdef";
    let body = || Full::new(Bytes::from_static(ARCHIVE));

    assert_eq!(farm.send(push(ARCHIVE)).await.0, StatusCode::CREATED);
    for asked in [
        request("PUT", &good_uri(ARCHIVE), bearer(wrong), body()),
        request("PUT", &good_uri(ARCHIVE), None, body()),
        request("PUT", &uri(&sha, "27.3", "27C1"), bearer(OPERATOR), body()),
        request(
            "PUT",
            "/v1/xcode/not-a-digest?version=27.2&build=27B52",
            bearer(OPERATOR),
            body(),
        ),
    ] {
        assert_ne!(farm.send(asked).await.0, StatusCode::CREATED);
    }

    let stored = format!(
        " INFO hostkeeper_coordinator::xcode_put: an Xcode archive was stored version=27.2 build=27B52 sha256={sha} size={} by=operator",
        ARCHIVE.len()
    );
    let lines = lines(&farm.log);
    let own: Vec<&String> = lines
        .iter()
        .filter(|line| line.contains("hostkeeper_coordinator::xcode_put:"))
        .collect();
    assert_eq!(own, vec![&stored], "{lines:?}");
    for secret in [OPERATOR, wrong] {
        assert!(!farm.log.text().contains(secret), "{}", farm.log.text());
    }
}

/// Proves: a coordinator without `--xcode-dir` has no such route: the push is a 404.
///
/// Catches: a route that answers without a store to put the archive in.
#[tokio::test]
async fn without_a_store_the_path_is_not_found() {
    let rig = Rig::new("xcode-put-no-store");
    let coordinator = Arc::new(Rig::start(&rig.dir, &rig.clock));

    let response = route(
        &coordinator,
        Surface::Control,
        &None,
        push(ARCHIVE),
        PATIENCE,
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
