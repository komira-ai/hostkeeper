//! The upload that resumes, through the router on the private surface
//! ([`crate::xcode_put_range`]): the query of what the store holds, a part with `Content-Range`,
//! the partial upload it leaves, its lock and the end that stores the archive. The coordinator,
//! the requests and the bodies are in `xcode_put_support_tests.rs`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::Ordering;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Request, StatusCode};
use serde_json::{Value, json};

use super::support::{
    ARCHIVE, Chunks, Farm, OTHER, Piece, ROOMY, bearer, builder, data, digest_of, farm, farm_with,
    feed, files_of, good_uri, listing, lock_clock, push, release, request, updater, uri,
};
use crate::audit_actions::XCODE_PUT;
use crate::testutil::{OPERATOR, ci_token};
use crate::xcode_feed::Channel;
use crate::xcode_store::{LOCK_STALE, XcodeStore};

/// The bytes of [`ARCHIVE`] with one letter changed: the same size, other bytes.
pub(super) const TAMPERED: &[u8] = b"the bytes of an Xcode archivX";

/// The size of [`ARCHIVE`].
pub(super) const SIZE: usize = ARCHIVE.len();

/// A request for the part `first..end` of the archive named by `path`, the body `body` and a
/// `Content-Range` that says the archive is `size` bytes: `Content-Length` is the part's.
pub(super) fn ranged<B>(path: &str, first: usize, end: usize, size: usize, body: B) -> Request<B> {
    builder("PUT", path, bearer(OPERATOR))
        .header("content-range", format!("bytes {first}-{}/{size}", end - 1))
        .header("content-length", (end - first).to_string())
        .body(body)
        .expect("a request")
}

/// The operator's push of the bytes `first..end` of [`ARCHIVE`] as Xcode 27.2 (27B52).
pub(super) fn part(first: usize, end: usize) -> Request<Full<Bytes>> {
    ranged(
        &good_uri(ARCHIVE),
        first,
        end,
        SIZE,
        Full::new(Bytes::from_static(&ARCHIVE[first..end])),
    )
}

/// The operator's `GET` of what the store holds of the archive `sha256`.
pub(super) fn query(sha256: &str) -> Request<Full<Bytes>> {
    request(
        "GET",
        &format!("/v1/xcode/{sha256}/upload"),
        bearer(OPERATOR),
        Full::new(Bytes::new()),
    )
}

/// The names of the partial upload of [`ARCHIVE`]: its file and its record.
pub(super) fn partial_names() -> Vec<String> {
    let sha = digest_of(ARCHIVE);
    vec![format!(".partial-{sha}"), format!(".partial-{sha}.json")]
}

/// The name of the lock of the partial upload of [`ARCHIVE`].
pub(super) fn lock_name() -> String {
    format!(".partial-{}.lock", digest_of(ARCHIVE))
}

/// Another store over the directory of `farm`, as another coordinator of the deployment is, whose
/// locks are judged by the clock of `farm`'s.
pub(super) fn second_store(farm: &Farm) -> XcodeStore {
    XcodeStore::open(&farm.dir, ROOMY, std::time::UNIX_EPOCH)
        .expect("a second store over the directory")
        .with_clock(lock_clock(&farm.clock))
}

/// The bytes the partial upload of [`ARCHIVE`] holds.
fn held_bytes(farm: &Farm) -> Vec<u8> {
    fs::read(farm.dir.join(&partial_names()[0])).expect("the partial upload")
}

/// Proves: the query answers `none` with 0 and 0 for an archive the store knows nothing of,
/// `partial` with the bytes held and the size the upload was started with once a part is stored,
/// and `held` with the archive's size once it is stored.
///
/// Catches: a query that cannot tell a resumable upload from nothing, one that answers the bytes
/// of another upload, and one that does not say an archive is held already.
#[tokio::test]
async fn the_query_says_none_then_partial_then_held() {
    let farm = farm("xcode-range-query");
    let sha = digest_of(ARCHIVE);
    assert_eq!(
        farm.send(query(&sha)).await,
        (
            StatusCode::OK,
            json!({"state": "none", "received": 0, "size": 0})
        )
    );

    assert_eq!(farm.send(part(0, 10)).await.0, StatusCode::ACCEPTED);
    assert_eq!(
        farm.send(query(&sha)).await,
        (
            StatusCode::OK,
            json!({"state": "partial", "received": 10, "size": SIZE})
        )
    );
    assert_eq!(
        farm.send(query(&digest_of(OTHER))).await,
        (
            StatusCode::OK,
            json!({"state": "none", "received": 0, "size": 0})
        )
    );

    assert_eq!(farm.send(part(10, SIZE)).await.0, StatusCode::CREATED);
    assert_eq!(
        farm.send(query(&sha)).await,
        (
            StatusCode::OK,
            json!({"state": "held", "received": SIZE, "size": SIZE})
        )
    );
}

/// Proves: the query takes the operator's bearer alone, by the checks of the `PUT` in their
/// order: a client certificate is 403 `forbidden`, no token, an unknown one and a CI token are 401
/// `unauthorized`, and a path that is not a SHA-256 and `/upload` is 400 `bad_request`; any
/// other `GET` of the store's path is still 405.
///
/// Catches: a query that tells a host, an unauthenticated caller or CI what the store holds.
#[tokio::test]
async fn the_query_takes_the_operators_bearer_alone() {
    let farm = farm("xcode-range-query-refused");
    let sha = digest_of(ARCHIVE);
    let path = format!("/v1/xcode/{sha}/upload");
    let ask = |authorization: Option<String>, path: &str| {
        request("GET", path, authorization, Full::new(Bytes::new()))
    };

    let from_a_host = farm
        .send_within(
            &updater(),
            ask(bearer(OPERATOR), &path),
            Duration::from_secs(1),
        )
        .await;
    assert_eq!(
        from_a_host,
        (StatusCode::FORBIDDEN, json!({"error": "forbidden"}))
    );
    for (what, authorization) in [
        ("no token", None),
        ("an unknown token", bearer("not-a-token")),
        ("a CI token", bearer(&ci_token())),
    ] {
        assert_eq!(
            farm.send(ask(authorization, &path)).await,
            (StatusCode::UNAUTHORIZED, json!({"error": "unauthorized"})),
            "{what}"
        );
    }
    for path in [
        "/v1/xcode/upload".to_owned(),
        format!("/v1/xcode/{}/upload", &sha[1..]),
        format!("/v1/xcode/{}/upload", sha.to_uppercase()),
        format!("/v1/xcode/{sha}/uploads"),
    ] {
        let answered = farm.send(ask(bearer(OPERATOR), &path)).await;
        // The last path does not end in `/upload`: it is the `GET` of the store's path as before.
        let expected = if path.ends_with("/uploads") {
            (
                StatusCode::METHOD_NOT_ALLOWED,
                json!({"error": "method_not_allowed"}),
            )
        } else {
            (StatusCode::BAD_REQUEST, json!({"error": "bad_request"}))
        };
        assert_eq!(answered, expected, "{path}");
    }
    assert_eq!(listing(&farm.dir), Vec::<String>::new());
}

/// Proves: three parts send an archive: the first two are 202 with the bytes held after each, the
/// last is 201 with the archive's SHA-256, version, build and size; the store then holds exactly
/// the bytes sent beside its record, with no partial upload, no record of one and no lock left,
/// and the audit holds one `xcode_put`.
///
/// Catches: a part that is not appended where the last one ended, a partial upload left after the
/// archive is stored, a lock left behind, and an upload in parts that is not audited like one in
/// a single request.
#[tokio::test]
async fn parts_are_appended_and_the_last_stores_the_archive() {
    let farm = farm("xcode-range-parts");
    let sha = digest_of(ARCHIVE);

    assert_eq!(
        farm.send(part(0, 10)).await,
        (StatusCode::ACCEPTED, json!({"received": 10}))
    );
    assert_eq!(held_bytes(&farm), &ARCHIVE[..10]);
    assert_eq!(listing(&farm.dir), partial_names());
    assert_eq!(
        farm.send(part(10, 20)).await,
        (StatusCode::ACCEPTED, json!({"received": 20}))
    );
    assert_eq!(held_bytes(&farm), &ARCHIVE[..20]);
    assert_eq!(farm.puts(), Vec::new());
    let (status, body) = farm.send(part(20, SIZE)).await;

    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        body,
        json!({"sha256": sha, "version": "27.2", "build": "27B52", "size": SIZE})
    );
    assert_eq!(
        fs::read(farm.store.archive_path(&sha)).expect("the archive"),
        ARCHIVE
    );
    assert_eq!(listing(&farm.dir), files_of(&sha));
    let puts = farm.puts();
    assert_eq!(puts.len(), 1, "{puts:?}");
    assert_eq!(puts[0].action, XCODE_PUT);
    assert_eq!(
        puts[0].reason.as_deref(),
        Some(format!("Xcode 27.2 (27B52), sha256 {sha}, {SIZE} bytes").as_str())
    );
    assert_eq!(farm.coordinator.xcode.release_waiting(), None);
}

/// Proves: a partial upload's file and record are made with mode 0600, and its record holds the
/// version, build and size of the first part and the name of the operator's token, exactly.
///
/// Catches: a partial upload readable by other users of the host, and a record that does not say
/// what the upload is or who started it.
#[tokio::test]
async fn the_partial_upload_is_private_and_its_record_says_what_it_is() {
    let farm = farm("xcode-range-record");
    farm.send(part(0, 10)).await;

    for name in partial_names() {
        let mode = fs::metadata(farm.dir.join(&name))
            .expect("a file")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "{name}");
    }
    let record: Value =
        serde_json::from_slice(&fs::read(farm.dir.join(&partial_names()[1])).expect("the record"))
            .expect("JSON");
    assert_eq!(
        record,
        json!({"version": "27.2", "build": "27B52", "size": SIZE, "by": "operator"})
    );
}

/// Proves: a part that starts at another byte than the partial upload holds is 409
/// `offset_mismatch` with the bytes held, whether it starts before them (a part sent twice) or
/// after them, and appends nothing; a first part that does not start at 0 is the same with 0 held,
/// and makes no partial upload.
///
/// Catches: bytes appended at the wrong place, so that a resumed archive has a hole or a repeat,
/// and an answer that does not say where to go on from.
#[tokio::test]
async fn a_part_at_the_wrong_offset_is_refused_with_the_bytes_held() {
    let farm = farm("xcode-range-offset");
    let mismatch = |received: u64| {
        (
            StatusCode::CONFLICT,
            json!({"error": "offset_mismatch", "received": received}),
        )
    };

    assert_eq!(farm.send(part(5, 15)).await, mismatch(0));
    assert_eq!(listing(&farm.dir), Vec::<String>::new());

    farm.send(part(0, 10)).await;
    assert_eq!(farm.send(part(0, 10)).await, mismatch(10));
    assert_eq!(farm.send(part(12, 20)).await, mismatch(10));
    assert_eq!(held_bytes(&farm), &ARCHIVE[..10]);
}

/// Proves: a `Content-Length` that is not the range's length, and none at all, are 400
/// `bad_request` that make no partial upload, and every `Content-Range` that is not one `bytes
/// <first>-<last>/<size>` of decimal numbers with `first <= last < size` is the same, as is a
/// request with two.
///
/// Catches: a part whose body is not the bytes its range names, which would be appended all the
/// same, and a malformed range read as another.
#[tokio::test]
async fn a_length_or_a_range_that_does_not_fit_is_bad_request() {
    let farm = farm("xcode-range-bad");
    let path = good_uri(ARCHIVE);
    let with = |range: &str, length: Option<&str>| {
        let mut builder = builder("PUT", &path, bearer(OPERATOR)).header("content-range", range);
        if let Some(length) = length {
            builder = builder.header("content-length", length);
        }
        builder
            .body(Full::new(Bytes::from_static(&ARCHIVE[..10])))
            .expect("a request")
    };
    farm.refused(
        "a length shorter than the range",
        with("bytes 0-9/29", Some("9")),
        StatusCode::BAD_REQUEST,
        "bad_request",
    )
    .await;
    farm.refused(
        "a length longer than the range",
        with("bytes 0-9/29", Some("11")),
        StatusCode::BAD_REQUEST,
        "bad_request",
    )
    .await;
    farm.refused(
        "no length",
        with("bytes 0-9/29", None),
        StatusCode::BAD_REQUEST,
        "bad_request",
    )
    .await;
    for range in [
        "bytes 0-9",
        "bytes 0-9/",
        "bytes 0/29",
        "bytes -9/29",
        "bytes 10-9/29",
        "bytes 0-29/29",
        "bytes 0-9/*",
        "bytes */29",
        "bytes 0-9/29x",
        "bytes 0-9 /29",
        "bytes 0-+9/29",
        "bytes 0-99999999999999999999/99999999999999999999",
        "items 0-9/29",
        "bytes  0-9/29",
        "",
    ] {
        farm.refused(
            range,
            with(range, Some("10")),
            StatusCode::BAD_REQUEST,
            "bad_request",
        )
        .await;
    }
    let twice = builder("PUT", &path, bearer(OPERATOR))
        .header("content-range", "bytes 0-9/29")
        .header("content-range", "bytes 0-9/29")
        .header("content-length", "10")
        .body(Full::new(Bytes::from_static(&ARCHIVE[..10])))
        .expect("a request");
    farm.refused("two ranges", twice, StatusCode::BAD_REQUEST, "bad_request")
        .await;
}

/// Proves: a range that is not valid UTF-8 is 400 `bad_request`.
///
/// Catches: a header that is not text read as text.
#[tokio::test]
async fn a_range_that_is_not_text_is_bad_request() {
    let farm = farm("xcode-range-binary");
    let asked = builder("PUT", &good_uri(ARCHIVE), bearer(OPERATOR))
        .header(
            "content-range",
            hyper::header::HeaderValue::from_bytes(b"bytes 0-9/\xff").expect("a value"),
        )
        .header("content-length", "10")
        .body(Full::new(Bytes::from_static(&ARCHIVE[..10])))
        .expect("a request");
    farm.refused("binary", asked, StatusCode::BAD_REQUEST, "bad_request")
        .await;
}

/// Proves: every check of the `PUT` runs before a part is taken, in its order, and leaves nothing
/// of a partial upload: a client certificate is `forbidden`, no token `unauthorized`, a path that
/// is not a SHA-256 and a query with a key too many `bad_request`, a release the list does not
/// hold `not_listed`, an archive the store holds `already_held`, another build of its version
/// `xcode_version_taken`, a release the store would remove at once `not_kept`.
///
/// Catches: a part that skips a check the whole upload makes, and so stores what a whole upload
/// would have been refused.
#[tokio::test]
async fn the_checks_of_the_put_refuse_a_part_before_any_byte_is_kept() {
    let farm = farm("xcode-range-checks");
    let sha = digest_of(ARCHIVE);
    let to = |path: &str| {
        ranged(
            path,
            0,
            10,
            SIZE,
            Full::new(Bytes::from_static(&ARCHIVE[..10])),
        )
    };
    let refuse = |status: StatusCode, code: &str| (status, json!({ "error": code }));

    let cert = farm
        .send_within(&updater(), part(0, 10), Duration::from_secs(1))
        .await;
    assert_eq!(cert, refuse(StatusCode::FORBIDDEN, "forbidden"));
    let anonymous = builder("PUT", &good_uri(ARCHIVE), None)
        .header("content-range", "bytes 0-9/29")
        .header("content-length", "10")
        .body(Full::new(Bytes::from_static(&ARCHIVE[..10])))
        .expect("a request");
    assert_eq!(
        farm.send(anonymous).await,
        refuse(StatusCode::UNAUTHORIZED, "unauthorized")
    );
    for path in [
        format!("/v1/xcode/{}?version=27.2&build=27B52", &sha[1..]),
        format!("{}&extra=1", good_uri(ARCHIVE)),
    ] {
        assert_eq!(
            farm.send(to(&path)).await,
            refuse(StatusCode::BAD_REQUEST, "bad_request"),
            "{path}"
        );
    }
    assert_eq!(
        farm.send(to(&uri(&sha, "27.2", "27B99"))).await,
        refuse(StatusCode::CONFLICT, "not_listed")
    );
    assert_eq!(listing(&farm.dir), Vec::<String>::new());

    // The store holds 27.2 (27B52): the same bytes, the release, and another build of it.
    assert_eq!(farm.send(push(ARCHIVE)).await.0, StatusCode::CREATED);
    let held = listing(&farm.dir);
    assert_eq!(
        farm.send(part(0, 10)).await,
        refuse(StatusCode::CONFLICT, "already_held")
    );
    farm.coordinator
        .xcode
        .known
        .keep(&{
            let mut known = feed();
            known.push(release("27.2", "27B53", Channel::Release));
            known
        })
        .expect("the list is kept");
    assert_eq!(
        farm.send(to(&uri(&digest_of(OTHER), "27.2", "27B53")))
            .await,
        refuse(StatusCode::CONFLICT, "xcode_version_taken")
    );
    // 27.1 is older than the one release the store keeps and no host has it installed.
    assert_eq!(
        farm.send(to(&uri(&digest_of(OTHER), "27.1", "27B40")))
            .await,
        refuse(StatusCode::CONFLICT, "not_kept")
    );
    assert_eq!(listing(&farm.dir), held);
}

/// Proves: the size a part's `Content-Range` names is what `too_large` judges against
/// `--xcode-max-bytes`, not the length of the part: a 10 byte part of a 29 byte archive is 413
/// `too_large` under a cap of 16, and a part of an archive of exactly the cap is taken.
///
/// Catches: a cap that is applied to each part, so that an archive of any size is taken in
/// parts, and one that refuses an archive of the cap's size.
#[tokio::test]
async fn too_large_is_judged_by_the_size_of_the_archive() {
    let farm = farm_with("xcode-range-large", 29, |coordinator, _| coordinator);
    let capped = ranged(
        &good_uri(ARCHIVE),
        0,
        10,
        30,
        Full::new(Bytes::from_static(&ARCHIVE[..10])),
    );
    farm.refused(
        "an archive over the cap",
        capped,
        StatusCode::PAYLOAD_TOO_LARGE,
        "too_large",
    )
    .await;

    assert_eq!(farm.send(part(0, 10)).await.0, StatusCode::ACCEPTED);
}

/// Proves: a partial upload is continued only by a request that says the same size, version and
/// build: another size, another version and another build are each 409 `partial_differs`, and
/// the partial upload still holds what it held and goes on from it with the right request.
///
/// Catches: bytes of one release appended to a partial upload of another, and a partial upload
/// that a different request replaces.
#[tokio::test]
async fn a_partial_upload_of_another_size_version_or_build_is_not_continued() {
    let farm = farm("xcode-range-differs");
    farm.coordinator
        .xcode
        .known
        .keep(&{
            let mut known = feed();
            known.push(release("27.2", "27B53", Channel::Release));
            known
        })
        .expect("the list is kept");
    farm.send(part(0, 10)).await;
    let sha = digest_of(ARCHIVE);
    let differs = (StatusCode::CONFLICT, json!({"error": "partial_differs"}));
    let body = || Full::new(Bytes::from_static(&ARCHIVE[10..20]));

    let size = ranged(&good_uri(ARCHIVE), 10, 20, 30, body());
    assert_eq!(farm.send(size).await, differs);
    let version = ranged(&uri(&sha, "27.1", "27B40"), 10, 20, SIZE, body());
    assert_eq!(farm.send(version).await, differs);
    let build = ranged(&uri(&sha, "27.2", "27B53"), 10, 20, SIZE, body());
    assert_eq!(farm.send(build).await, differs);

    assert_eq!(held_bytes(&farm), &ARCHIVE[..10]);
    assert_eq!(
        farm.send(part(10, 20)).await,
        (StatusCode::ACCEPTED, json!({"received": 20}))
    );
}

/// Proves: the last part's whole file is hashed: bytes that are not the path's SHA-256 are 400
/// `digest_mismatch`, and the partial upload, its record and its lock are gone, nothing is
/// stored and nothing is audited, so that the next try starts from 0.
///
/// Catches: an archive stored under a SHA-256 its bytes do not have because the parts were
/// written by an earlier request, and a corrupt partial upload that every later try continues.
#[tokio::test]
async fn a_whole_file_that_is_not_the_named_digest_is_refused_and_removed() {
    let farm = farm("xcode-range-digest");
    let sha = digest_of(ARCHIVE);
    let sent = |first: usize, end: usize| {
        ranged(
            &good_uri(ARCHIVE),
            first,
            end,
            SIZE,
            Full::new(Bytes::from_static(&TAMPERED[first..end])),
        )
    };
    assert_eq!(farm.send(sent(0, 10)).await.0, StatusCode::ACCEPTED);

    let answered = farm.send(sent(10, SIZE)).await;

    assert_eq!(
        answered,
        (StatusCode::BAD_REQUEST, json!({"error": "digest_mismatch"}))
    );
    farm.assert_untouched("a file of other bytes");
    assert_eq!(
        farm.send(query(&sha)).await.1,
        json!({"state": "none", "received": 0, "size": 0})
    );
    assert_eq!(
        farm.send(part(0, 10)).await,
        (StatusCode::ACCEPTED, json!({"received": 10}))
    );
}

/// Proves: a request that is cut off keeps what it wrote, synced: a body that fails after 4 bytes,
/// one that ends after 5 of 10, and one that has 2 bytes more than its range are each 400
/// `bad_request` with the bytes before the fault held, and a part from there goes on and stores
/// the archive.
///
/// Catches: a partial upload that is removed when its request fails, which is every resume's
/// reason to be, and bytes written that the next request does not start after.
#[tokio::test]
async fn a_cut_off_part_keeps_the_bytes_it_wrote() {
    let farm = farm("xcode-range-cut");
    let sha = digest_of(ARCHIVE);
    let bad = (StatusCode::BAD_REQUEST, json!({"error": "bad_request"}));
    let pieces = |pieces: Vec<Piece>| Chunks {
        pieces: pieces.into(),
        ..Chunks::default()
    };

    let failing = ranged(
        &good_uri(ARCHIVE),
        0,
        10,
        SIZE,
        pieces(vec![Piece::Data(ARCHIVE[..4].to_vec()), Piece::Fail]),
    );
    assert_eq!(farm.send(failing).await, bad);
    assert_eq!(held_bytes(&farm), &ARCHIVE[..4]);
    assert_eq!(
        farm.send(query(&sha)).await.1,
        json!({"state": "partial", "received": 4, "size": SIZE})
    );

    let short = ranged(
        &good_uri(ARCHIVE),
        4,
        14,
        SIZE,
        pieces(vec![Piece::Trailers, Piece::Data(ARCHIVE[4..9].to_vec())]),
    );
    assert_eq!(farm.send(short).await, bad);
    assert_eq!(held_bytes(&farm), &ARCHIVE[..9]);

    let long = ranged(
        &good_uri(ARCHIVE),
        9,
        19,
        SIZE,
        data(&[&ARCHIVE[9..19], b"xy"]),
    );
    assert_eq!(farm.send(long).await, bad);
    assert_eq!(held_bytes(&farm), &ARCHIVE[..19]);

    assert_eq!(farm.send(part(19, SIZE)).await.0, StatusCode::CREATED);
    assert_eq!(
        fs::read(farm.store.archive_path(&sha)).expect("the archive"),
        ARCHIVE
    );
}

/// Proves: a body that stalls for more than the patience after some bytes is 400 `bad_request`,
/// and the bytes it gave are held.
///
/// Catches: a stalled part that holds the lock for ever, and one whose bytes are lost.
#[tokio::test(start_paused = true)]
async fn a_stalled_part_keeps_its_bytes_and_frees_its_lock() {
    let farm = farm("xcode-range-stalled");
    let mut body = data(&[&ARCHIVE[..4]]);
    body.stalls = true;
    let asked = ranged(&good_uri(ARCHIVE), 0, 10, SIZE, body);

    let answered = farm
        .send_within(&None, asked, Duration::from_millis(50))
        .await;

    assert_eq!(
        answered,
        (StatusCode::BAD_REQUEST, json!({"error": "bad_request"}))
    );
    assert_eq!(held_bytes(&farm), &ARCHIVE[..4]);
    assert_eq!(listing(&farm.dir), partial_names());
}

/// Proves: while another store over the directory (another coordinator) holds the partial upload's
/// lock, a part is 409 `partial_locked` and keeps nothing, and a request of the same coordinator
/// for an archive it is already writing is the same, where a part of another archive of the
/// version in progress is `upload_in_progress`; once the lock is released the part is taken.
///
/// Catches: two writers appending to one partial upload, and a refusal for a lock that the
/// client cannot tell from an upload of the version.
#[tokio::test]
async fn a_second_writer_of_a_partial_upload_is_refused_partial_locked() {
    let farm = farm("xcode-range-locked");
    let sha = digest_of(ARCHIVE);
    let locked = (StatusCode::CONFLICT, json!({"error": "partial_locked"}));
    let other = second_store(&farm);

    let held = other.lock_upload(&sha).expect("the first writer's lock");
    assert_eq!(farm.send(part(0, 10)).await, locked);
    assert_eq!(listing(&farm.dir), vec![lock_name()]);
    drop(held);
    assert_eq!(farm.send(part(0, 10)).await.0, StatusCode::ACCEPTED);

    // A request of this coordinator, while another of its own writes the same archive.
    let writing = farm.store.lock_upload(&sha).expect("this store's writer");
    let busy = farm.store.reserve("27.2").expect("the version is free");
    assert_eq!(farm.send(part(10, 20)).await, locked);
    drop(writing);
    assert_eq!(
        farm.send(part(10, 20)).await,
        (StatusCode::CONFLICT, json!({"error": "upload_in_progress"}))
    );
    drop(busy);
    assert_eq!(held_bytes(&farm), &ARCHIVE[..10]);
    assert_eq!(farm.send(part(10, 20)).await.0, StatusCode::ACCEPTED);
}

/// Proves: a lock another store made is taken over once it has gone [`LOCK_STALE`] seconds of the
/// clock without a refresh and not before: at one second short of it the part is `partial_locked`,
/// at it the part is taken, and the lock is gone after it.
///
/// Catches: a writer that died with its lock and blocks the upload for ever, and a takeover of a
/// lock whose writer is alive.
#[tokio::test]
async fn a_lock_not_refreshed_for_thirty_seconds_is_taken_over() {
    let farm = farm("xcode-range-stale");
    let sha = digest_of(ARCHIVE);
    let other = second_store(&farm);
    let _dead = other.lock_upload(&sha).expect("the first writer's lock");
    let made = farm.clock.load(Ordering::SeqCst);

    farm.clock.store(made + LOCK_STALE - 1, Ordering::SeqCst);
    assert_eq!(
        farm.send(part(0, 10)).await,
        (StatusCode::CONFLICT, json!({"error": "partial_locked"}))
    );
    farm.clock.store(made + LOCK_STALE, Ordering::SeqCst);
    assert_eq!(
        farm.send(part(0, 10)).await,
        (StatusCode::ACCEPTED, json!({"received": 10}))
    );
    assert_eq!(listing(&farm.dir), partial_names());
}

/// Proves: a part is 500 `store_failed`, not `partial_locked`, when the lock cannot be examined
/// (the lock's path is a directory), and keeps nothing.
///
/// Catches: a failure of the disk told to the client as another writer's lock, which it would
/// retry for ever.
#[tokio::test]
async fn a_part_is_store_failed_when_the_lock_cannot_be_examined() {
    let farm = farm("xcode-range-lock-disk");
    fs::create_dir(farm.dir.join(lock_name())).expect("a directory in the lock's place");

    assert_eq!(
        farm.send(part(0, 10)).await,
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": "store_failed"})
        )
    );

    assert_eq!(listing(&farm.dir), vec![lock_name()]);
}
