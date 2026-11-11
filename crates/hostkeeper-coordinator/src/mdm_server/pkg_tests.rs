//! `GET /pkg/<sha256>.pkg` (`docs/design/coordinator-mdm.md` section 4.7): which digests are served,
//! the digest check of the bytes, the validators and the ranges.

use std::os::unix::fs::PermissionsExt as _;

use http_body_util::BodyExt as _;
use hyper::header::{HeaderMap, HeaderName, HeaderValue};
use hyper::{Method, StatusCode};
use sha2::{Digest, Sha256};

use std::sync::Arc;
use std::sync::atomic::Ordering;

use hostkeeper_mdm_gate::PkgDigest;

use super::super::pkg;
use super::super::support::{Rig, Session};
use crate::mdm_setup::MdmSetup;
use crate::operator_mdm_push_tests::Flaky;
use crate::store::mdm_setup::SetupStore;

/// The SHA-256 of `bytes` in hex.
fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// A pkg of `len` bytes that differ from one `seed` to the next.
fn bytes_of(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| u8::try_from(i % 251).unwrap_or(0).wrapping_add(seed))
        .collect()
}

/// Sets the setting `installs` to one install for each of `digests`.
fn install(rig: &Rig, digests: &[&str]) {
    let entries: Vec<String> = digests
        .iter()
        .enumerate()
        .map(|(i, digest)| {
            format!(
                r#"{{"name":"pkg-{i}","bundle_id":"dev.hostkeeper.hostkeeper","sha256":"{digest}","version":"0.1.66","path":"/var/lib/hostkeeper-mdm-pkg/{digest}.pkg"}}"#
            )
        })
        .collect();
    rig.setup
        .set_setting("installs", &format!("[{}]", entries.join(",")))
        .expect("set");
}

/// Writes `bytes` as the file named for `digest` in the pkg directory.
fn put(rig: &Rig, digest: &str, bytes: &[u8]) {
    std::fs::create_dir_all(rig.pkg_dir()).expect("a directory");
    std::fs::write(rig.pkg_dir().join(format!("{digest}.pkg")), bytes).expect("written");
}

/// A pkg of `len` bytes provided and named by an install, and its digest.
fn provided(rig: &Rig, seed: u8, len: usize) -> (String, Vec<u8>) {
    let bytes = bytes_of(seed, len);
    let digest = sha(&bytes);
    put(rig, &digest, &bytes);
    install(rig, &[&digest]);
    (digest, bytes)
}

/// What a request was answered: the status, the headers and the body, which is `Err` where the
/// server cut it.
struct Seen {
    status: StatusCode,
    headers: HeaderMap,
    body: Result<Vec<u8>, ()>,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }
}

/// `method path` with `headers`, answered by the route without a connection.
async fn ask(rig: &Rig, method: &str, path: &str, headers: &[(&str, &str)]) -> Seen {
    let mut map = HeaderMap::new();
    for (name, value) in headers {
        map.insert(
            HeaderName::try_from(*name).expect("a name"),
            HeaderValue::try_from(*value).expect("a value"),
        );
    }
    let method = Method::from_bytes(method.as_bytes()).expect("a method");
    let response = pkg::serve(&rig.coordinator, &rig.server, &method, path, &map).await;
    let (head, body) = response.into_parts();
    Seen {
        status: head.status,
        headers: head.headers,
        body: body
            .collect()
            .await
            .map(|collected| collected.to_bytes().to_vec())
            .map_err(|_| ()),
    }
}

/// `GET /pkg/<digest>.pkg`.
async fn get(rig: &Rig, digest: &str, headers: &[(&str, &str)]) -> Seen {
    ask(rig, "GET", &format!("/pkg/{digest}.pkg"), headers).await
}

const NOT_FOUND: &[u8] = br#"{"error":"not_found"}"#;

/// Proves: a Mac's `GET` over the listener's TLS of a pkg an install names is `200` with the file's
/// bytes, its `Content-Length`, the digest as its `ETag` and `Accept-Ranges: bytes`, whatever the
/// size (a file of several chunks), and a `HEAD` is the same head with no body.
///
/// Catches: the route not wired into the router (a `404` as before CM11c), a length that differs
/// from the file's, and a body that is cut or padded.
#[tokio::test]
async fn a_pkg_an_install_names_is_served_with_its_length_and_digest() {
    let rig = Rig::new("pkg-served");
    let (digest, bytes) = provided(&rig, 1, 150_000);
    let mut session = Session::dial(&rig, None).await.expect("connected");
    let (head, body) = session
        .fetch("GET", &format!("/pkg/{digest}.pkg"), &[])
        .await
        .expect("an answer");
    assert_eq!(head.status, StatusCode::OK);
    assert_eq!(body, bytes);
    assert_eq!(head.headers["content-length"], "150000");
    assert_eq!(head.headers["etag"], format!("\"{digest}\"").as_str());
    assert_eq!(head.headers["accept-ranges"], "bytes");
    assert_eq!(head.headers["content-type"], "application/octet-stream");
    let (head, body) = session
        .fetch("HEAD", &format!("/pkg/{digest}.pkg"), &[])
        .await
        .expect("an answer");
    assert_eq!(head.status, StatusCode::OK);
    assert!(body.is_empty());
    assert_eq!(head.headers["content-length"], "150000");
}

/// Proves: an empty file named by the digest of nothing is served as an empty `200`; a `Range` of
/// it is `416`.
///
/// Catches: a held last chunk that does not exist being sent or awaited, and a suffix range of an
/// empty file taken as one byte.
#[tokio::test]
async fn an_empty_pkg_is_an_empty_answer() {
    let rig = Rig::new("pkg-empty");
    let (digest, _) = provided(&rig, 0, 0);
    let seen = get(&rig, &digest, &[]).await;
    assert_eq!(seen.status, StatusCode::OK);
    assert_eq!(seen.body, Ok(Vec::new()));
    assert_eq!(seen.header("content-length"), Some("0"));
    for range in ["bytes=0-", "bytes=-5", "bytes=0-0"] {
        let seen = get(&rig, &digest, &[("range", range)]).await;
        assert_eq!(seen.status, StatusCode::RANGE_NOT_SATISFIABLE, "{range}");
        assert_eq!(seen.header("content-range"), Some("bytes */0"));
    }
}

/// Proves: a file whose bytes differ from the digest in its name never reaches a client whole: the
/// full `GET` ends in an error before the last chunk (fewer bytes than the length it promised, and
/// the same size as the right file), and a `HEAD` and a `Range` of it are `500 pkg_digest_mismatch`
/// with none of the file's bytes. A file shorter than the right one is cut the same way.
///
/// Catches: a digest compared after the last byte was sent, a check skipped for a `Range` or a
/// `HEAD`, and a mismatch taken for a file to serve.
#[tokio::test]
async fn bytes_that_differ_from_the_digest_are_refused() {
    let rig = Rig::new("pkg-mismatch");
    let right = bytes_of(1, 150_000);
    let digest = sha(&right);
    let mut wrong = right.clone();
    wrong[149_999] ^= 1;
    put(&rig, &digest, &wrong);
    install(&rig, &[&digest]);
    let seen = get(&rig, &digest, &[]).await;
    assert_eq!(seen.status, StatusCode::OK);
    assert_eq!(seen.header("content-length"), Some("150000"));
    assert_eq!(seen.body, Err(()));

    let mut session = Session::dial(&rig, None).await.expect("connected");
    assert!(
        session
            .fetch("GET", &format!("/pkg/{digest}.pkg"), &[])
            .await
            .is_err(),
        "the connection is cut before the body is whole"
    );
    let mut session = Session::dial(&rig, None).await.expect("connected");
    let (head, body) = session
        .fetch("HEAD", &format!("/pkg/{digest}.pkg"), &[])
        .await
        .expect("an answer");
    assert_eq!(head.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(body.is_empty(), "a HEAD has no body");
    let seen = get(&rig, &digest, &[("range", "bytes=0-9")]).await;
    assert_eq!(seen.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        seen.body,
        Ok(br#"{"error":"pkg_digest_mismatch"}"#.to_vec())
    );

    put(&rig, &digest, &right[..100_000]);
    assert_eq!(get(&rig, &digest, &[]).await.body, Err(()));
}

/// Proves: a file that is cut short after the answer began (a truncation between the head and the
/// body) ends the body in an error and is not recorded as checked, so a `Range` of it is refused.
///
/// Catches: a file shorter than its `Content-Length` ending a body as if it were whole.
#[tokio::test]
async fn a_file_cut_short_while_it_is_served_is_not_a_whole_answer() {
    let rig = Rig::new("pkg-truncated");
    let (digest, bytes) = provided(&rig, 2, 150_000);
    let path = rig.pkg_dir().join(format!("{digest}.pkg"));
    let method = Method::GET;
    let uri = format!("/pkg/{digest}.pkg");
    let response = pkg::serve(
        &rig.coordinator,
        &rig.server,
        &method,
        &uri,
        &HeaderMap::new(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    std::fs::write(&path, &bytes[..70_000]).expect("truncated");
    assert!(response.into_body().collect().await.is_err());
    let seen = get(&rig, &digest, &[("range", "bytes=0-9")]).await;
    assert_eq!(seen.status, StatusCode::INTERNAL_SERVER_ERROR);

    // A checked file that is cut after a `Range` answer began ends that body in an error too.
    put(&rig, &digest, &bytes);
    assert_eq!(get(&rig, &digest, &[]).await.body, Ok(bytes.clone()));
    let ranged = HeaderMap::from_iter([(
        HeaderName::from_static("range"),
        HeaderValue::from_static("bytes=100-60000"),
    )]);
    let response = pkg::serve(&rig.coordinator, &rig.server, &method, &uri, &ranged).await;
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    std::fs::write(&path, &bytes[..50]).expect("truncated");
    assert!(response.into_body().collect().await.is_err());
}

/// Proves: a digest no install names is `404 not_found` and the directory is not read for it: the
/// file is there and unreadable (mode 0), and the answer is the same `404` as for a digest with no
/// file; editing the setting takes effect at the next request, both ways; an `installs` setting
/// that was never set names nothing.
///
/// Catches: the directory serving whatever file is in it (a pkg dropped by hand), a setting read
/// once at start, and a read attempted before the setting was consulted.
#[tokio::test]
async fn a_digest_no_install_names_is_not_found_and_nothing_is_read() {
    let rig = Rig::new("pkg-unknown");
    let listed = bytes_of(3, 1000);
    let unlisted = bytes_of(4, 1000);
    let (listed, unlisted) = (sha(&listed), sha(&unlisted));
    put(&rig, &unlisted, &bytes_of(4, 1000));
    put(&rig, &listed, &bytes_of(3, 1000));
    let seen = get(&rig, &unlisted, &[]).await;
    assert_eq!(
        (seen.status, seen.body),
        (StatusCode::NOT_FOUND, Ok(NOT_FOUND.to_vec())),
        "never set"
    );
    install(&rig, &[&listed]);
    let hidden = rig.pkg_dir().join(format!("{unlisted}.pkg"));
    std::fs::set_permissions(&hidden, std::fs::Permissions::from_mode(0o000)).expect("mode");
    let seen = get(&rig, &unlisted, &[]).await;
    assert_eq!(
        (seen.status, seen.body),
        (StatusCode::NOT_FOUND, Ok(NOT_FOUND.to_vec()))
    );
    let seen = ask(&rig, "HEAD", &format!("/pkg/{unlisted}.pkg"), &[]).await;
    assert_eq!(seen.status, StatusCode::NOT_FOUND);
    assert_eq!(get(&rig, &listed, &[]).await.status, StatusCode::OK);
    install(&rig, &[&unlisted]);
    assert_eq!(get(&rig, &listed, &[]).await.status, StatusCode::NOT_FOUND);
    std::fs::set_permissions(&hidden, std::fs::Permissions::from_mode(0o600)).expect("mode");
    assert_eq!(get(&rig, &unlisted, &[]).await.status, StatusCode::OK);
}

/// Proves: a digest an install names but with no file, a directory in its place, a coordinator
/// started without `--mdm-pkg-dir`, and a setting that does not read are each answered: `404` for the
/// first three, `500` for the last.
///
/// Catches: a missing file taken for an empty one, and a corrupt setting served as "everything".
#[tokio::test]
async fn a_named_pkg_that_is_not_there_is_not_found() {
    let rig = Rig::new("pkg-missing");
    let digest = sha(b"nothing");
    install(&rig, &[&digest]);
    assert_eq!(get(&rig, &digest, &[]).await.status, StatusCode::NOT_FOUND);
    std::fs::create_dir_all(rig.pkg_dir().join(format!("{digest}.pkg"))).expect("a directory");
    assert_eq!(get(&rig, &digest, &[]).await.status, StatusCode::NOT_FOUND);

    let bare = Rig::without_pkg_dir("pkg-no-dir");
    install(&bare, &[&digest]);
    assert_eq!(get(&bare, &digest, &[]).await.status, StatusCode::NOT_FOUND);

    rig.setup.set_setting("installs", "not json").expect("set");
    let seen = get(&rig, &digest, &[]).await;
    assert_eq!(seen.status, StatusCode::INTERNAL_SERVER_ERROR);

    // The pkg directory is not a directory: the file cannot be opened, which is no `404`.
    let blocked = Rig::new("pkg-not-a-dir");
    install(&blocked, &[&digest]);
    std::fs::write(blocked.pkg_dir(), b"x").expect("written");
    let seen = get(&blocked, &digest, &[]).await;
    assert_eq!(seen.status, StatusCode::INTERNAL_SERVER_ERROR);
}

/// Proves: a setting store that cannot be read is an error and not "no installs": `is_installed`
/// fails, and a digest is not served by guessing.
///
/// Catches: an unreadable setting taken for an empty one (a `404` that hides the fault) or for a
/// permissive one.
#[test]
fn an_unreadable_setting_is_an_error() {
    let store = Flaky::new();
    let setup = MdmSetup::new(
        Arc::clone(&store) as Arc<dyn SetupStore>,
        None,
        crate::enrollment::os_random(),
    );
    let digest = PkgDigest::of(b"x");
    assert_eq!(pkg::is_installed(&setup, &digest), Ok(false));
    store.reads_fail.store(true, Ordering::SeqCst);
    assert_eq!(pkg::is_installed(&setup, &digest), Err(()));
}

/// Proves: only `GET` and `HEAD` are served: any other method on a pkg an install names is `405`
/// with `Allow: GET, HEAD`, whether or not the file is there.
///
/// Catches: a `PUT` or `POST` reaching the file route.
#[tokio::test]
async fn a_method_other_than_get_and_head_is_refused() {
    let rig = Rig::new("pkg-methods");
    let (digest, _) = provided(&rig, 5, 100);
    let mut session = Session::dial(&rig, None).await.expect("connected");
    for method in ["POST", "PUT", "DELETE", "PATCH", "OPTIONS"] {
        for named in [digest.clone(), sha(b"other")] {
            let (head, body) = session
                .fetch(method, &format!("/pkg/{named}.pkg"), &[])
                .await
                .expect("an answer");
            assert_eq!(head.status, StatusCode::METHOD_NOT_ALLOWED, "{method}");
            assert_eq!(head.headers["allow"], "GET, HEAD");
            assert_eq!(body, br#"{"error":"method_not_allowed"}"#);
        }
    }
}

/// Proves: a path that is not exactly `/pkg/` and 64 lower-case hexadecimal digits and `.pkg` is
/// `404`: `..`, a separator, an encoded dot or slash, an upper-case digest, a missing or doubled
/// suffix, a short digest; and a file one level up from the directory is not reachable by them.
///
/// Catches: a path walked out of `--mdm-pkg-dir`.
#[tokio::test]
async fn a_path_that_is_not_a_digest_is_not_found() {
    let rig = Rig::new("pkg-paths");
    let (digest, _) = provided(&rig, 6, 100);
    let outside = rig.pkg_dir().with_file_name("secret.pkg");
    std::fs::write(&outside, b"secret").expect("written");
    let upper = digest.to_uppercase();
    for path in [
        "/pkg/../secret.pkg".to_owned(),
        format!("/pkg/../pkgs/{digest}.pkg"),
        format!("/pkg/./{digest}.pkg"),
        format!("/pkg//{digest}.pkg"),
        format!("/pkg/x/{digest}.pkg"),
        format!("/pkg/{digest}.pkg/"),
        format!("/pkg/{digest}.pkg/.."),
        format!("/pkg/{digest}"),
        format!("/pkg/{digest}.pkg.pkg"),
        format!("/pkg/{upper}.pkg"),
        format!("/pkg/%2e%2e%2f{digest}.pkg"),
        format!("/pkg/{}%2f{digest}.pkg", ".."),
        format!("/pkg/{}.pkg", &digest[..63]),
        "/pkg/.pkg".to_owned(),
        "/pkg/".to_owned(),
        "/pkg/ab.pkg".to_owned(),
    ] {
        for method in ["GET", "HEAD"] {
            let seen = ask(&rig, method, &path, &[]).await;
            assert_eq!(seen.status, StatusCode::NOT_FOUND, "{method} {path}");
        }
    }
}

/// Proves: `If-None-Match` with the digest's `ETag` (alone, weak, in a list or as `*`) is `304` with
/// no body and the `ETag`, and without a match, or with `If-Modified-Since` alone, the file is
/// served; a `304` for a digest no install names does not happen.
///
/// Catches: a client that holds the pkg downloading it again, and a validator answered for a digest
/// that is not served.
#[tokio::test]
async fn a_client_holding_the_pkg_is_told_it_is_current() {
    let rig = Rig::new("pkg-conditional");
    let (digest, bytes) = provided(&rig, 7, 5000);
    let etag = format!("\"{digest}\"");
    let weak = format!("W/{etag}");
    let list = format!("\"x\", {etag}");
    for value in [&etag, &weak, &list, &"*".to_owned()] {
        let seen = get(&rig, &digest, &[("if-none-match", value)]).await;
        assert_eq!(seen.status, StatusCode::NOT_MODIFIED, "{value}");
        assert_eq!(seen.body, Ok(Vec::new()));
        assert_eq!(seen.header("etag"), Some(etag.as_str()));
    }
    let seen = get(&rig, &digest, &[("if-none-match", "\"other\"")]).await;
    assert_eq!(
        (seen.status, seen.body),
        (StatusCode::OK, Ok(bytes.clone()))
    );
    let since = [("if-modified-since", "Wed, 21 Oct 2037 07:28:00 GMT")];
    let seen = get(&rig, &digest, &since).await;
    assert_eq!((seen.status, seen.body), (StatusCode::OK, Ok(bytes)));
    let other = sha(b"not installed");
    let seen = get(&rig, &other, &[("if-none-match", "*")]).await;
    assert_eq!(seen.status, StatusCode::NOT_FOUND);
}

/// Proves: a `Range` of a pkg is `206` with the bytes and a `Content-Range` (`a-b`, `a-`, `-n`, an
/// end past the file clipped, a suffix longer than the file the whole file), a range that starts
/// past the end is `416` with `bytes */len`, and what is not one range (several, a malformed one,
/// `b-a` backwards, another unit, an `If-Range` that is not the `ETag`) is answered as a whole
/// `200`; an `If-Range` with the `ETag` keeps the range; a `Range` on a `HEAD` is the head of the
/// `206`.
///
/// Catches: an interrupted download restarting from zero, an off-by-one at the end of a range, and
/// a range answered with the wrong length.
#[tokio::test]
async fn a_range_of_a_pkg_is_served_from_the_file() {
    let rig = Rig::new("pkg-ranges");
    let (digest, bytes) = provided(&rig, 8, 1000);
    let etag = format!("\"{digest}\"");
    for (range, from, to) in [
        ("bytes=0-9", 0, 9),
        ("bytes=990-", 990, 999),
        ("bytes=-10", 990, 999),
        ("bytes=995-5000", 995, 999),
        ("bytes=-5000", 0, 999),
        ("bytes=0-0", 0, 0),
        ("bytes=999-999", 999, 999),
        (" bytes= 10 - 19 ", 10, 19),
    ] {
        let seen = get(&rig, &digest, &[("range", range)]).await;
        assert_eq!(seen.status, StatusCode::PARTIAL_CONTENT, "{range}");
        assert_eq!(seen.body, Ok(bytes[from..=to].to_vec()), "{range}");
        let length = to - from + 1;
        assert_eq!(
            seen.header("content-length"),
            Some(length.to_string().as_str())
        );
        assert_eq!(
            seen.header("content-range"),
            Some(format!("bytes {from}-{to}/1000").as_str()),
            "{range}"
        );
    }
    for range in ["bytes=1000-", "bytes=5000-6000", "bytes=-0"] {
        let seen = get(&rig, &digest, &[("range", range)]).await;
        assert_eq!(seen.status, StatusCode::RANGE_NOT_SATISFIABLE, "{range}");
        assert_eq!(seen.header("content-range"), Some("bytes */1000"));
    }
    for range in [
        "bytes=0-1,5-9",
        "bytes=9-3",
        "bytes=a-b",
        "bytes=a-5",
        "bytes=",
        "bytes=5",
        "items=0-9",
        "bytes=--5",
    ] {
        let seen = get(&rig, &digest, &[("range", range)]).await;
        assert_eq!(seen.status, StatusCode::OK, "{range}");
        assert_eq!(seen.body, Ok(bytes.clone()), "{range}");
    }
    let stale = get(
        &rig,
        &digest,
        &[("range", "bytes=0-9"), ("if-range", "\"old\"")],
    )
    .await;
    assert_eq!(
        (stale.status, stale.body),
        (StatusCode::OK, Ok(bytes.clone()))
    );
    let kept = get(
        &rig,
        &digest,
        &[("range", "bytes=0-9"), ("if-range", &etag)],
    )
    .await;
    assert_eq!(kept.status, StatusCode::PARTIAL_CONTENT);
    let head = ask(
        &rig,
        "HEAD",
        &format!("/pkg/{digest}.pkg"),
        &[("range", "bytes=0-9")],
    )
    .await;
    assert_eq!(head.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(head.body, Ok(Vec::new()));
    assert_eq!(head.header("content-length"), Some("10"));
}

/// Proves: a file whole-checked by a full `GET` is on record under its size, modification time,
/// device, inode and change time, and a `Range` is answered from the record; a file whose bytes
/// are replaced with the old size and the old modification time restored (what `cp -p` and `rsync
/// -t` do) has another change time, so it is hashed again and refused as a mismatch; so is one
/// whose size or modification time changed.
///
/// Catches: a check recorded for the digest and not for the file that was checked, a record that a
/// restored modification time defeats, and a `Range` that hashes the whole pkg every time.
#[tokio::test]
async fn a_pkg_changed_since_it_was_checked_is_checked_again() {
    let rig = Rig::new("pkg-recheck");
    let (digest, bytes) = provided(&rig, 9, 1000);
    let path = rig.pkg_dir().join(format!("{digest}.pkg"));
    let stamp = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .expect("a time");
    let on_record = |path: &std::path::Path| {
        let meta = std::fs::metadata(path).expect("metadata");
        rig.server.pkgs.is_checked(&digest, pkg::Stamp::of(&meta))
    };
    assert!(!on_record(&path), "nothing was checked yet");
    assert_eq!(get(&rig, &digest, &[]).await.body, Ok(bytes.clone()));
    assert!(on_record(&path), "a full GET that ended well is recorded");
    let seen = get(&rig, &digest, &[("range", "bytes=0-9")]).await;
    assert_eq!(seen.status, StatusCode::PARTIAL_CONTENT);

    let mut wrong = bytes.clone();
    wrong[0] ^= 1;
    std::fs::write(&path, &wrong).expect("written");
    let file = std::fs::File::options()
        .write(true)
        .open(&path)
        .expect("opened");
    file.set_modified(stamp).expect("a time");
    assert!(!on_record(&path), "the size and time are as they were");
    let seen = get(&rig, &digest, &[("range", "bytes=0-9")]).await;
    assert_eq!(seen.status, StatusCode::INTERNAL_SERVER_ERROR);
    file.set_modified(stamp + std::time::Duration::from_secs(5))
        .expect("a time");
    let seen = get(&rig, &digest, &[("range", "bytes=0-9")]).await;
    assert_eq!(seen.status, StatusCode::INTERNAL_SERVER_ERROR);

    let mut longer = bytes;
    longer.push(0);
    put(&rig, &digest, &longer);
    let seen = get(&rig, &digest, &[("range", "bytes=0-9")]).await;
    assert_eq!(seen.status, StatusCode::INTERNAL_SERVER_ERROR);
}

/// Proves: a `200`, `206` and `304` are cacheable for a year (the digest names the bytes), and a
/// `416` carries the `ETag` and `Accept-Ranges` but no `Cache-Control`, so no cache keeps an
/// unsatisfiable answer for the URL.
///
/// Catches: a cached `416` that outlives the file that was too short.
#[tokio::test]
async fn only_an_answer_with_the_file_is_cacheable() {
    let rig = Rig::new("pkg-cache");
    let (digest, _) = provided(&rig, 11, 100);
    let etag = format!("\"{digest}\"");
    let cacheable = Some("public, max-age=31536000, immutable");
    let full = get(&rig, &digest, &[]).await;
    let part = get(&rig, &digest, &[("range", "bytes=0-9")]).await;
    let current = get(&rig, &digest, &[("if-none-match", &etag)]).await;
    for (seen, status) in [
        (&full, StatusCode::OK),
        (&part, StatusCode::PARTIAL_CONTENT),
        (&current, StatusCode::NOT_MODIFIED),
    ] {
        assert_eq!(seen.status, status);
        assert_eq!(seen.header("cache-control"), cacheable, "{status}");
    }
    let past = get(&rig, &digest, &[("range", "bytes=100-")]).await;
    assert_eq!(past.status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(past.header("cache-control"), None);
    assert_eq!(past.header("etag"), Some(etag.as_str()));
    assert_eq!(past.header("accept-ranges"), Some("bytes"));
    assert_eq!(past.header("content-range"), Some("bytes */100"));
}

/// A file as `AsyncRead` and `AsyncSeek` that fails where it is told to.
struct Faulty {
    data: Vec<u8>,
    at: usize,
    /// The read that fails once this many bytes were read.
    fail_read_after: Option<usize>,
    fail_seek: bool,
}

impl Faulty {
    fn new(data: Vec<u8>) -> Self {
        Self {
            data,
            at: 0,
            fail_read_after: None,
            fail_seek: false,
        }
    }
}

impl tokio::io::AsyncRead for Faulty {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if self.fail_read_after.is_some_and(|n| self.at >= n) {
            return std::task::Poll::Ready(Err(std::io::Error::other("a read failed")));
        }
        let mut end = (self.at + buf.remaining()).min(self.data.len());
        if let Some(n) = self.fail_read_after {
            end = end.min(n);
        }
        buf.put_slice(&self.data[self.at..end]);
        self.at = end;
        std::task::Poll::Ready(Ok(()))
    }
}

impl tokio::io::AsyncSeek for Faulty {
    fn start_seek(
        mut self: std::pin::Pin<&mut Self>,
        position: std::io::SeekFrom,
    ) -> std::io::Result<()> {
        if self.fail_seek {
            return Err(std::io::Error::other("a seek failed"));
        }
        if let std::io::SeekFrom::Start(to) = position {
            self.at = usize::try_from(to).expect("a position");
        }
        Ok(())
    }

    fn poll_complete(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<u64>> {
        std::task::Poll::Ready(Ok(self.at as u64))
    }
}

/// The frames of `body` until it ends: the bytes it gave and whether it ended in an error.
async fn drained(body: crate::grpc::RouteBody) -> (Vec<u8>, bool) {
    let mut body = std::pin::pin!(body);
    let mut bytes = Vec::new();
    while let Some(frame) =
        std::future::poll_fn(|cx| hyper::body::Body::poll_frame(body.as_mut(), cx)).await
    {
        match frame {
            Ok(frame) => bytes.extend(frame.into_data().unwrap_or_default()),
            Err(_) => return (bytes, true),
        }
    }
    (bytes, false)
}

/// Proves: the body of a range reads from the start it was given, and ends in an error (never as
/// a shorter whole answer) when the seek fails, when a read fails after some bytes went out, and
/// when the file ends before the length that was promised.
///
/// Catches: a range cut short that the client takes for complete, and a failed seek answered with
/// bytes from the wrong place.
#[tokio::test]
async fn a_range_body_that_cannot_be_read_ends_in_an_error() {
    let data = bytes_of(3, 200_000);
    let (bytes, failed) =
        drained(pkg::part(Box::new(Faulty::new(data.clone())), 10, 150_000)).await;
    assert_eq!((bytes, failed), (data[10..150_010].to_vec(), false));

    let mut no_seek = Faulty::new(data.clone());
    no_seek.fail_seek = true;
    assert_eq!(
        drained(pkg::part(Box::new(no_seek), 10, 100)).await,
        (Vec::new(), true)
    );

    let mut breaks = Faulty::new(data.clone());
    breaks.fail_read_after = Some(70_000);
    let (bytes, failed) = drained(pkg::part(Box::new(breaks), 0, 150_000)).await;
    assert!(failed);
    assert!(bytes.len() < 150_000);
    assert_eq!(bytes, data[..bytes.len()].to_vec());

    let (bytes, failed) = drained(pkg::part(
        Box::new(Faulty::new(data[..50].to_vec())),
        0,
        100,
    ))
    .await;
    assert_eq!((bytes, failed), (data[..50].to_vec(), true));
}

/// Proves: a full `GET` whose file cannot be read part-way ends the body in an error without its
/// last chunk and does not record the file as checked, and hashing a file that cannot be read is
/// an error, not a digest of what was read.
///
/// Catches: a read error taken for the end of the file, so a truncated pkg is sent as whole.
#[tokio::test]
async fn a_read_error_while_streaming_or_hashing_is_not_a_whole_answer() {
    let rig = Rig::new("pkg-read-error");
    provided(&rig, 5, 10);
    let data = bytes_of(4, 200_000);
    let digest = sha(&data);
    let mut breaks = Faulty::new(data.clone());
    breaks.fail_read_after = Some(100_000);
    let mut streaming = pkg::Streaming {
        file: Box::new(breaks),
        hasher: Sha256::new(),
        held: None,
        left: data.len() as u64,
        done: false,
        digest: digest.clone(),
        stamp: pkg::Stamp::of(&std::fs::metadata(rig.pkg_dir()).expect("metadata")),
        server: Arc::clone(&rig.server),
    };
    let mut sent = 0;
    let mut ended = None;
    while let Some(frame) = streaming.next().await {
        match frame {
            Ok(frame) => sent += frame.into_data().map_or(0, |d| d.len()),
            Err(why) => ended = Some(why),
        }
    }
    assert!(ended.is_some(), "the body ended in an error");
    assert!(sent < data.len(), "the last chunk was held back");
    assert!(!rig.server.pkgs.is_checked(&digest, streaming.stamp));

    let mut breaks = Faulty::new(data.clone());
    breaks.fail_read_after = Some(1000);
    assert!(pkg::hash_of(&mut breaks, 200_000).await.is_err());
    let short = pkg::hash_of(&mut Faulty::new(data[..50].to_vec()), 100).await;
    assert_eq!(
        short.ok(),
        Some(sha(&data[..50])),
        "a file that ends early hashes what it has"
    );
    assert_eq!(
        pkg::hash_of(&mut Faulty::new(data.clone()), 200_000)
            .await
            .ok(),
        Some(digest)
    );
}

/// Proves: the package family answers `503 unavailable` while the name has no certificate, and a
/// pkg is not looked for.
///
/// Catches: a pkg served before the listener is configured.
#[tokio::test]
async fn the_route_is_unavailable_until_the_name_has_a_certificate() {
    let rig = Rig::bare("pkg-unavailable");
    let (digest, _) = provided(&rig, 10, 10);
    let seen = get(&rig, &digest, &[]).await;
    assert_eq!(seen.status, StatusCode::SERVICE_UNAVAILABLE);
}
