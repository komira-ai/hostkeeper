//! The route that serves a Mac an Xcode archive ([`crate::xcode_get`]), called in process with the
//! certificate of an enrolled host, over a store in a scratch directory: what a host is answered
//! and refused, the ranges it may ask for to resume a copy, the grammar of the `Range` header, and
//! what the log says. No socket is opened.

use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use hostkeeper_enrollment::names::HostId;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_testkit::Scratch;
use http_body_util::{BodyExt, Full};
use hyper::header::{
    ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG, HeaderValue, IF_RANGE, RANGE,
};
use hyper::{HeaderMap, Method, Request, StatusCode};
use rcgen::{CertificateParams, ExtendedKeyUsagePurpose, KeyPair, SanType};

use super::{Asked, Spec, answer, parse, resolve, serve};
use crate::host_groups_support::{SELF, World};
use crate::http::Peer;
use crate::testutil::{Log, scratch};
use crate::wait::ConnectionWaits;
use crate::xcode_feed::{XcodeId, XcodeVersion};
use crate::xcode_store::{Archive, Digesting, PREFIX, XcodeStore};

const NOT_FOUND: &str = r#"{"error":"not_found"}"#;
const FORBIDDEN: &str = r#"{"error":"forbidden"}"#;
const DENIED: &str = r#"{"error":"denied"}"#;

/// How long a connection is kept after its last hold, in the tests of the hold.
const PATIENCE: Duration = Duration::from_secs(30);

/// The bytes of the archive: a pattern longer than two chunks of the body.
fn archive_bytes() -> Vec<u8> {
    (0..150_000_u32)
        .map(|n| u8::try_from(n % 251).expect("a byte"))
        .collect()
}

/// The certificate of the host `name`, as an updater's.
fn host(name: &str) -> Peer {
    peer(&[
        "urn:kbf:role:updater",
        "urn:kbf:host-role:mac",
        &format!("dns:{name}"),
    ])
}

/// A client certificate with the subjectAltNames `sans` (`dns:` names, else URIs).
fn peer(sans: &[&str]) -> Peer {
    let names = sans
        .iter()
        .map(|s| match s.strip_prefix("dns:") {
            Some(name) => SanType::DnsName(name.try_into().expect("a name")),
            None => SanType::URI((*s).try_into().expect("a uri")),
        })
        .collect();
    let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
    params.subject_alt_names = names;
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    let key = KeyPair::generate().expect("key");
    Some(Arc::from(
        params.self_signed(&key).expect("sign").der().as_ref(),
    ))
}

fn host_id(text: &str) -> HostId {
    HostId::parse(text).expect("a host id")
}

/// The lines of `log` without their time, which is the one part that changes.
fn lines(log: &Log) -> Vec<String> {
    log.text()
        .lines()
        .map(|line| line.split_once(' ').expect("a time").1.to_owned())
        .collect()
}

/// Stores `bytes` as the archive of Xcode `version` build `build`, as an upload and its commit
/// do: the SHA-256 it is named by.
fn put(store: &XcodeStore, version: &str, build: &str, bytes: &[u8]) -> String {
    let mut digest = Digesting::default();
    digest.feed(bytes);
    let sha256 = digest.finish();
    let (path, mut file) = store.upload_file(&sha256).expect("an upload file");
    file.write_all(bytes).expect("written");
    drop(file);
    let archive = Archive {
        sha256: sha256.clone(),
        id: XcodeId {
            version: XcodeVersion::parse(version).expect("a version"),
            build: build.to_owned(),
        },
        size: u64::try_from(bytes.len()).expect("a size"),
        by: "tests".to_owned(),
        at_unix: 1,
    };
    let _changing = store.changing();
    store.commit(&path, &archive).expect("committed");
    sha256
}

/// A coordinator whose group `keeps` keeps one Xcode and whose group `drops` keeps none, with the
/// enrolled hosts `m1` (in `keeps`), `m2` (in `drops`) and `m3` (in none), and a store holding one
/// archive.
struct Set {
    world: World,
    store: XcodeStore,
    dir: Scratch,
    sha256: String,
    bytes: Vec<u8>,
}

fn set_up(name: &str) -> Set {
    let world = World::new(name);
    for id in ["m1", "m2", "m3"] {
        world.enroll(id);
    }
    let keeps = proto::RolloutPolicy {
        maintenance: Some(proto::MaintenancePolicy {
            xcode_keep: 1,
            ..proto::MaintenancePolicy::default()
        }),
        ..proto::RolloutPolicy::default()
    };
    world
        .put_with("keeps", &[SELF], Some(keeps), None)
        .expect("put keeps");
    world.put("drops", &[SELF], None).expect("put drops");
    world.move_to("m1", "keeps").expect("moved");
    world.move_to("m2", "drops").expect("moved");
    let dir = scratch(&format!("{name}-store"));
    let store = XcodeStore::open(dir.path(), 1 << 30).expect("a store");
    let bytes = archive_bytes();
    let sha256 = put(&store, "27.2", "27B52", &bytes);
    Set {
        world,
        store,
        dir,
        sha256,
        bytes,
    }
}

impl Set {
    /// The path of the archive.
    fn path(&self) -> String {
        format!("{PREFIX}{}", self.sha256)
    }

    /// The answer to the request `method path` with `headers` from `who`.
    async fn send(
        &self,
        who: &Peer,
        method: Method,
        path: &str,
        headers: &[(&str, &str)],
    ) -> (StatusCode, HeaderMap, Bytes) {
        let mut builder = Request::builder().method(method).uri(path);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder.body(Full::new(Bytes::new())).expect("a request");
        let response = answer(self.world.coordinator(), &self.store, who, request).await;
        let (parts, body) = response.into_parts();
        let bytes = body.collect().await.expect("a body").to_bytes();
        (parts.status, parts.headers, bytes)
    }

    /// `GET` of the archive with `headers`, from the host `m1`.
    async fn get(&self, headers: &[(&str, &str)]) -> (StatusCode, HeaderMap, Bytes) {
        self.send(&host("m1"), Method::GET, &self.path(), headers)
            .await
    }

    /// `GET` of the archive with the `Range` header `range`.
    async fn ranged(&self, range: &str) -> (StatusCode, HeaderMap, Bytes) {
        self.get(&[("range", range)]).await
    }
}

/// Proves: a host of a group that keeps Xcode is answered the whole archive: 200, the exact bytes,
/// `Content-Length` of the file, `Accept-Ranges: bytes`, `ETag` the quoted SHA-256 and
/// `Content-Type: application/octet-stream`, and no `Content-Range`.
///
/// Catches: a body that is cut at a chunk, a missing length or validator, an archive served under
/// the wrong type.
#[tokio::test]
async fn a_host_is_served_the_whole_archive() {
    let set = set_up("xcode-get-whole");
    let (status, headers, body) = set.get(&[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.to_vec(), set.bytes);
    assert_eq!(headers[CONTENT_LENGTH], set.bytes.len().to_string());
    assert_eq!(headers[ACCEPT_RANGES], "bytes");
    assert_eq!(headers[ETAG], format!("\"{}\"", set.sha256));
    assert_eq!(headers[CONTENT_TYPE], "application/octet-stream");
    assert!(headers.get(CONTENT_RANGE).is_none());
}

/// Proves: `Range: bytes=<first>-<last>` is answered 206 with `Content-Range: bytes
/// <first>-<last>/<size>`, `Content-Length` of the part and exactly those bytes, for a range inside
/// one chunk and for ranges that cross the 64 KiB chunks; a last position past the end is taken as
/// the last byte; the first and the last byte alone are each served.
///
/// Catches: a range that starts at the wrong byte, an off-by-one at either end, a range cut at a
/// chunk boundary.
#[tokio::test]
async fn a_range_is_answered_with_exactly_those_bytes() {
    let set = set_up("xcode-get-range");
    let size = set.bytes.len();
    let cases = [
        (0, 0),
        (1000, 1999),
        (65_000, 66_999),
        (1, 140_000),
        (size - 1, size - 1),
        (149_000, size - 1),
    ];
    for (first, last) in cases {
        let (status, headers, body) = set.ranged(&format!("bytes={first}-{last}")).await;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT, "{first}-{last}");
        assert_eq!(
            headers[CONTENT_RANGE],
            format!("bytes {first}-{last}/{size}")
        );
        assert_eq!(headers[CONTENT_LENGTH], (last - first + 1).to_string());
        assert_eq!(headers[ACCEPT_RANGES], "bytes");
        assert_eq!(headers[ETAG], format!("\"{}\"", set.sha256));
        assert_eq!(body.to_vec(), set.bytes[first..=last], "{first}-{last}");
    }
    let (status, headers, body) = set.ranged("bytes=149000-999999999").await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        headers[CONTENT_RANGE],
        format!("bytes 149000-149999/{size}")
    );
    assert_eq!(body.to_vec(), set.bytes[149_000..]);
}

/// Proves: a copy that broke after 90 000 bytes is resumed with `Range: bytes=90000-`: 206, the
/// tail, `Content-Range: bytes 90000-149999/150000`, and the broken part joined to the tail is the
/// whole archive, whose SHA-256 is its name.
///
/// Catches: a resume that repeats or skips a byte, a copy that no longer matches its digest.
#[tokio::test]
async fn an_open_range_resumes_a_broken_copy() {
    let set = set_up("xcode-get-resume");
    let broken = &set.bytes[..90_000];
    let (status, headers, tail) = set.ranged("bytes=90000-").await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(headers[CONTENT_RANGE], "bytes 90000-149999/150000");
    assert_eq!(headers[CONTENT_LENGTH], "60000");
    assert_eq!(tail.to_vec(), set.bytes[90_000..]);
    let mut joined = broken.to_vec();
    joined.extend_from_slice(&tail);
    assert_eq!(joined, set.bytes);
    let mut digest = Digesting::default();
    digest.feed(&joined);
    assert_eq!(digest.finish(), set.sha256);
}

/// Proves: `Range: bytes=-<count>` is the last `count` bytes (206 with their `Content-Range`), a
/// suffix longer than the file is the whole file as a 206 from byte 0, and a suffix of one byte is
/// the last byte.
///
/// Catches: a suffix read from the start, a suffix past the file that is refused or underflows.
#[tokio::test]
async fn a_suffix_range_is_the_last_bytes() {
    let set = set_up("xcode-get-suffix");
    let size = set.bytes.len();
    let (status, headers, body) = set.ranged("bytes=-500").await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(headers[CONTENT_RANGE], "bytes 149500-149999/150000");
    assert_eq!(headers[CONTENT_LENGTH], "500");
    assert_eq!(body.to_vec(), set.bytes[size - 500..]);

    let (status, headers, body) = set.ranged("bytes=-1").await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(headers[CONTENT_RANGE], "bytes 149999-149999/150000");
    assert_eq!(body.to_vec(), set.bytes[size - 1..]);

    let (status, headers, body) = set.ranged("bytes=-99999999999").await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(headers[CONTENT_RANGE], "bytes 0-149999/150000");
    assert_eq!(body.to_vec(), set.bytes);
}

/// Proves: a range that starts at or past the end of the file, and a suffix of no bytes, are 416
/// with `Content-Range: bytes */150000`, `Content-Length` 0 and no body; the last byte is still
/// served.
///
/// Catches: a range past the end answered 200 or 206 with nothing, or no size for the client to
/// resume from.
#[tokio::test]
async fn a_range_past_the_end_is_unsatisfiable() {
    let set = set_up("xcode-get-416");
    for range in [
        "bytes=150000-",
        "bytes=150000-150010",
        "bytes=200000-300000",
        "bytes=-0",
    ] {
        let (status, headers, body) = set.ranged(range).await;
        assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE, "{range}");
        assert_eq!(headers[CONTENT_RANGE], "bytes */150000", "{range}");
        assert_eq!(headers[CONTENT_LENGTH], "0", "{range}");
        assert!(body.is_empty(), "{range}");
    }
    let (status, _, body) = set.ranged("bytes=149999-").await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body.to_vec(), set.bytes[149_999..]);
}

/// Proves: an archive of no bytes is 200 with `Content-Length` 0 and no body when the whole is
/// asked, and 416 with `Content-Range: bytes */0` for every kind of range.
///
/// Catches: an empty file whose size makes the range arithmetic underflow.
#[tokio::test]
async fn an_empty_archive_has_no_satisfiable_range() {
    let set = set_up("xcode-get-empty");
    let sha256 = put(&set.store, "27.1", "27B1", b"");
    let path = format!("{PREFIX}{sha256}");
    let (status, headers, body) = set.send(&host("m1"), Method::GET, &path, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[CONTENT_LENGTH], "0");
    assert!(body.is_empty());
    for range in ["bytes=0-", "bytes=0-0", "bytes=-1"] {
        let (status, headers, body) = set
            .send(&host("m1"), Method::GET, &path, &[("range", range)])
            .await;
        assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE, "{range}");
        assert_eq!(headers[CONTENT_RANGE], "bytes */0", "{range}");
        assert!(body.is_empty(), "{range}");
    }
}

/// Proves: a `Range` the route does not understand is ignored and the whole file is answered 200
/// without a `Content-Range`: several ranges, a unit other than `bytes`, a first position past the
/// last, text that is no range, a space, a sign, a number that does not fit, a missing `bytes=`,
/// and two `Range` headers.
///
/// Catches: a malformed or unsupported range answered with a part, or refused instead of ignored.
#[tokio::test]
async fn a_range_that_is_not_understood_is_ignored() {
    let set = set_up("xcode-get-ignored");
    let ignored = [
        "bytes=0-1,5-9",
        "items=0-5",
        "bytes=9-3",
        "bytes=abc",
        "bytes=1-2-3",
        "bytes= 1-2",
        "bytes=1 -2",
        "bytes=+1-2",
        "bytes=-",
        "bytes=",
        "bytes",
        "0-5",
        "",
        "bytes=0-99999999999999999999999",
        "bytes=99999999999999999999999-",
    ];
    for range in ignored {
        let (status, headers, body) = set.ranged(range).await;
        assert_eq!(status, StatusCode::OK, "{range:?}");
        assert!(headers.get(CONTENT_RANGE).is_none(), "{range:?}");
        assert_eq!(body.to_vec(), set.bytes, "{range:?}");
    }
    let (status, _, body) = set
        .get(&[("range", "bytes=0-1"), ("range", "bytes=5-9")])
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.to_vec(), set.bytes);
}

/// Proves: a `Range` with an `If-Range` equal to the `ETag` is answered as a range (206), and one
/// with an `If-Range` that is another tag, a weak tag, an unquoted digest or a date is ignored: the
/// whole file, 200.
///
/// Catches: a resume of a file that is no longer the one the client started, joined without notice.
#[tokio::test]
async fn an_if_range_that_is_not_the_tag_makes_the_range_ignored() {
    let set = set_up("xcode-get-if-range");
    let etag = format!("\"{}\"", set.sha256);
    let (status, headers, body) = set
        .get(&[("range", "bytes=10-19"), ("if-range", etag.as_str())])
        .await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(headers[CONTENT_RANGE], "bytes 10-19/150000");
    assert_eq!(body.to_vec(), set.bytes[10..20]);

    let other = format!("\"{}\"", "0".repeat(64));
    let weak = format!("W/{etag}");
    for tag in [
        other.as_str(),
        weak.as_str(),
        set.sha256.as_str(),
        "Wed, 21 Oct 2026 07:28:00 GMT",
    ] {
        let (status, headers, body) = set
            .get(&[("range", "bytes=10-19"), ("if-range", tag)])
            .await;
        assert_eq!(status, StatusCode::OK, "{tag}");
        assert!(headers.get(CONTENT_RANGE).is_none(), "{tag}");
        assert_eq!(body.to_vec(), set.bytes, "{tag}");
    }
}

/// Proves: a method other than `GET` is 405 `method_not_allowed` with the exact body, from a host
/// that may read, and nothing is served.
///
/// Catches: an archive read, or a body left half answered, by a `HEAD`, `POST`, `PUT` or `DELETE`.
#[tokio::test]
async fn a_method_other_than_get_is_refused() {
    let set = set_up("xcode-get-method");
    for method in [Method::POST, Method::HEAD, Method::PUT, Method::DELETE] {
        let (status, headers, body) = set.send(&host("m1"), method, &set.path(), &[]).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(&body[..], br#"{"error":"method_not_allowed"}"#);
        assert!(headers.get(ETAG).is_none());
    }
}

/// Proves: a request with no certificate, and one whose certificate names no host, is 403
/// `forbidden` with the exact body and one warn line each (exact equality), and no byte of the
/// archive is in the answer.
///
/// Catches: the store read by a caller that is no host.
#[tokio::test]
async fn a_caller_that_is_no_host_is_forbidden() {
    let set = set_up("xcode-get-no-host");
    let log = Log::default();
    let _guard = log.install();
    for who in [None, peer(&["dns:m1"])] {
        let (status, headers, body) = set.send(&who, Method::GET, &set.path(), &[]).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(&body[..], FORBIDDEN.as_bytes());
        assert!(headers.get(ETAG).is_none());
    }
    let line = " WARN hostkeeper_coordinator::xcode_get: refused a read of the Xcode store: no certificate that names a host";
    assert_eq!(lines(&log), vec![line.to_owned(); 2]);
}

/// Proves: a host an operator denied is 403 `denied` while it retires and once it retired; the
/// first refusal is one warn line naming the node id.
///
/// Catches: a denied host that keeps downloading Xcode.
#[tokio::test]
async fn a_denied_host_is_refused() {
    let set = set_up("xcode-get-denied");
    let log = Log::default();
    let _guard = log.install();
    let store = set.world.enrollment.enrollment.store();
    store.deny(&host_id("m1")).expect("denied");
    let (status, _, body) = set.get(&[]).await;
    assert_eq!(
        (status, &body[..]),
        (StatusCode::FORBIDDEN, DENIED.as_bytes())
    );
    store.retire(&host_id("m1")).expect("retired");
    let (status, _, body) = set.get(&[]).await;
    assert_eq!(
        (status, &body[..]),
        (StatusCode::FORBIDDEN, DENIED.as_bytes())
    );
    assert_eq!(
        lines(&log)[0],
        " WARN hostkeeper_coordinator::xcode_get: refused a read of the Xcode store: the host is denied node_id=m1"
    );
}

/// Proves: a host of a group whose `xcode_keep` is 0, and a host in no group, are each 403
/// `forbidden` with the exact body and one warn line that names the node id, and the archive is
/// not served to either, with or without a range.
///
/// Catches: the store read by a Mac of a group that keeps no Xcode.
#[tokio::test]
async fn a_host_of_a_group_that_keeps_no_xcode_is_forbidden() {
    let set = set_up("xcode-get-keep-zero");
    let log = Log::default();
    let _guard = log.install();
    for name in ["m2", "m3"] {
        let asks: [&[(&str, &str)]; 2] = [&[], &[("range", "bytes=0-9")]];
        for headers in asks {
            let (status, answered, body) = set
                .send(&host(name), Method::GET, &set.path(), headers)
                .await;
            assert_eq!(
                (status, &body[..]),
                (StatusCode::FORBIDDEN, FORBIDDEN.as_bytes()),
                "{name}"
            );
            assert!(answered.get(ETAG).is_none());
        }
    }
    let expected: Vec<String> = ["m2", "m2", "m3", "m3"]
        .iter()
        .map(|name| {
            format!(
                " WARN hostkeeper_coordinator::xcode_get: refused a read of the Xcode store: the host's group keeps no Xcode node_id={name}"
            )
        })
        .collect();
    assert_eq!(lines(&log), expected);
}

/// Proves: a sha256 the store does not hold, one whose archive was removed, and paths that are no
/// sha256 (upper case, 63 or 65 characters, a further segment, a trailing slash, no name, `..`,
/// an escape) are each 404 `not_found` with the exact body, and nothing is logged.
///
/// Catches: a path that reaches another file of the store directory, or a missing archive that is
/// answered as a server failure.
#[tokio::test]
async fn an_unknown_or_malformed_name_is_a_404() {
    let set = set_up("xcode-get-404");
    let log = Log::default();
    let _guard = log.install();
    let unknown = "b".repeat(64);
    let upper = set.sha256.to_uppercase();
    let short = &set.sha256[..63];
    let long = format!("{}0", set.sha256);
    let paths = [
        format!("{PREFIX}{unknown}"),
        format!("{PREFIX}{upper}"),
        format!("{PREFIX}{short}"),
        format!("{PREFIX}{long}"),
        format!("{PREFIX}{}/extra", set.sha256),
        format!("{PREFIX}{}/", set.sha256),
        format!("{PREFIX}{}.xip", set.sha256),
        format!("{PREFIX}{}.json", set.sha256),
        PREFIX.to_owned(),
        format!("{PREFIX}..%2f{}", set.sha256),
        format!("{PREFIX}../{}", set.sha256),
        "/v1/xcode".to_owned(),
    ];
    for path in paths {
        let (status, _, body) = set.send(&host("m1"), Method::GET, &path, &[]).await;
        assert_eq!(
            (status, &body[..]),
            (StatusCode::NOT_FOUND, NOT_FOUND.as_bytes()),
            "{path}"
        );
    }
    let archive = set.store.archive(&set.sha256).expect("read").expect("held");
    {
        let _changing = set.store.changing();
        set.store.remove(&archive).expect("removed");
    }
    let (status, _, body) = set.get(&[]).await;
    assert_eq!(
        (status, &body[..]),
        (StatusCode::NOT_FOUND, NOT_FOUND.as_bytes())
    );
    assert_eq!(lines(&log), Vec::<String>::new());
}

/// Proves: a record of the store that cannot be read is 500 `store_failed` with the exact body
/// and one error line, and the same host is still served the archive whose record is whole.
///
/// Catches: a damaged record answered as a missing archive, or taking the other archives down.
#[tokio::test]
async fn a_record_that_cannot_be_read_is_a_server_error() {
    let set = set_up("xcode-get-bad-record");
    let broken = "a".repeat(64);
    std::fs::write(set.dir.path().join(format!("{broken}.json")), b"not json").expect("written");
    let log = Log::default();
    let _guard = log.install();
    let (status, _, body) = set
        .send(&host("m1"), Method::GET, &format!("{PREFIX}{broken}"), &[])
        .await;
    assert_eq!(
        (status, &body[..]),
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            br#"{"error":"store_failed"}"#.as_slice()
        )
    );
    let text = log.text();
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(
        text.contains(
            " ERROR hostkeeper_coordinator::xcode_get: the Xcode store could not be read"
        ),
        "{text}"
    );
    assert_eq!(set.get(&[]).await.0, StatusCode::OK);
}

/// Proves: an archive whose file is gone when it is opened is 404, and one whose directory is gone
/// (so it cannot be opened for another reason) is 500 `store_failed` with one error line.
///
/// Catches: a file that vanished between the record check and the open answered as a failure of
/// the host, or a failed open left unanswered.
#[tokio::test]
async fn a_file_that_cannot_be_opened_is_answered_by_why() {
    let set = set_up("xcode-get-open");
    let missing = "c".repeat(64);
    let answered = serve(&set.store, &missing, &HeaderMap::new(), None).await;
    assert_eq!(answered.status(), StatusCode::NOT_FOUND);

    let parent = scratch("xcode-get-open-store");
    let inner = parent.path().join("store");
    std::fs::create_dir(&inner).expect("a directory");
    let store = XcodeStore::open(&inner, 1 << 20).expect("a store");
    std::fs::remove_dir(&inner).expect("removed");
    std::fs::write(&inner, b"a file where the directory was").expect("a file");
    let log = Log::default();
    let _guard = log.install();
    let answered = serve(&store, &set.sha256, &HeaderMap::new(), None).await;
    assert_eq!(answered.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let text = log.text();
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(
        text.contains(
            " ERROR hostkeeper_coordinator::xcode_get: an Xcode archive could not be opened"
        ),
        "{text}"
    );
}

/// Proves: a file that ends before the length the route gave (it was cut after it was opened)
/// ends the body with an error after the bytes that were there, and one warn line; a file that
/// cannot be read at all ends the body with an error and no byte.
///
/// Catches: a short file answered as a complete archive, which a Mac would install as it is.
#[tokio::test]
async fn a_file_that_ends_early_ends_the_body_with_an_error() {
    let set = set_up("xcode-get-short");
    let log = Log::default();
    let _guard = log.install();
    let answered = serve(&set.store, &set.sha256, &HeaderMap::new(), None).await;
    assert_eq!(answered.status(), StatusCode::OK);
    std::fs::OpenOptions::new()
        .write(true)
        .open(set.store.archive_path(&set.sha256))
        .expect("opened")
        .set_len(100_000)
        .expect("cut");
    let (_, body) = answered.into_parts();
    let mut body = Box::pin(body);
    let mut got = Vec::new();
    let failure = loop {
        match body.frame().await {
            Some(Ok(frame)) => got.extend_from_slice(&frame.into_data().expect("data")),
            Some(Err(why)) => break why,
            None => panic!("the body ended as complete"),
        }
    };
    assert_eq!(got, set.bytes[..100_000]);
    assert!(
        failure.to_string().contains("unexpected end of file"),
        "{failure}"
    );
    let text = log.text();
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(
        text.contains(
            " WARN hostkeeper_coordinator::xcode_get: an Xcode archive could not be read to its end"
        ),
        "{text}"
    );

    // A directory where the archive's file should be opens, and cannot be read.
    let sha256 = "d".repeat(64);
    std::fs::create_dir(set.store.archive_path(&sha256)).expect("a directory");
    let answered = serve(&set.store, &sha256, &HeaderMap::new(), None).await;
    let (_, body) = answered.into_parts();
    assert!(body.collect().await.is_err());
}

/// Proves: the body holds the connection's wait until it is read to its end or dropped, and a
/// refusal holds none: with the response unread the connection is not quiet, and it is quiet once
/// the body was read, once the response was dropped, and after a 404.
///
/// Catches: an HTTP/2 connection cut one patience after its start in the middle of a copy that
/// takes longer, or a hold that is never released.
#[tokio::test(start_paused = true)]
async fn the_body_holds_the_connection_until_it_ends() {
    let set = set_up("xcode-get-hold");
    let waits = ConnectionWaits::default();
    let request = |path: &str| {
        let mut request = Request::builder()
            .uri(path)
            .body(Full::new(Bytes::new()))
            .expect("a request");
        request.extensions_mut().insert(waits.clone());
        request
    };
    let quiet = || async { tokio::time::timeout(PATIENCE * 3, waits.quiet_for(PATIENCE)).await };
    let m1 = host("m1");

    let response = answer(
        set.world.coordinator(),
        &set.store,
        &m1,
        request(&set.path()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(quiet().await.is_err(), "held while the body is unread");
    let (_, body) = response.into_parts();
    body.collect().await.expect("read to its end");
    assert!(quiet().await.is_ok(), "released at the end");

    let response = answer(
        set.world.coordinator(),
        &set.store,
        &m1,
        request(&set.path()),
    )
    .await;
    assert!(quiet().await.is_err());
    drop(response);
    assert!(quiet().await.is_ok(), "released when dropped");

    let missing = format!("{PREFIX}{}", "b".repeat(64));
    let response = answer(set.world.coordinator(), &set.store, &m1, request(&missing)).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(quiet().await.is_ok(), "a refusal holds nothing");
}

/// Proves: the grammar of the `Range` header: `bytes=<a>-<b>` with `a <= b`, `bytes=<a>-` and
/// `bytes=-<n>` are understood, each as the range it names; everything else, including a first
/// position past the last, is not understood.
///
/// Catches: a grammar that takes more than one range, signs, spaces or a unit other than `bytes`.
#[test]
fn the_range_grammar_takes_one_decimal_range() {
    let understood = [
        ("bytes=0-0", Spec::Between(0, 0)),
        ("bytes=5-9", Spec::Between(5, 9)),
        ("bytes=7-7", Spec::Between(7, 7)),
        ("bytes=00012-0013", Spec::Between(12, 13)),
        ("bytes=5-", Spec::From(5)),
        ("bytes=0-", Spec::From(0)),
        ("bytes=-5", Spec::Suffix(5)),
        ("bytes=-0", Spec::Suffix(0)),
        ("bytes=18446744073709551615-", Spec::From(u64::MAX)),
    ];
    for (text, spec) in understood {
        assert_eq!(parse(text), Some(spec), "{text}");
    }
    for text in [
        "bytes=9-3",
        "bytes=-",
        "bytes=",
        "bytes=a-b",
        "bytes=1-b",
        "bytes=a-1",
        "bytes=1-2,3-4",
        "bytes=1-2-3",
        "bytes=--5",
        "bytes=-5-",
        "bytes= 5-9",
        "bytes=5-9 ",
        "bytes=+5-9",
        "Bytes=5-9",
        "bytes 5-9",
        "units=5-9",
        "5-9",
        "",
        "bytes=18446744073709551616-",
        "bytes=-18446744073709551616",
    ] {
        assert_eq!(parse(text), None, "{text:?}");
    }
}

/// Proves: what a range asks of a file of 100 bytes, at its edges: the last position past the end
/// is clamped to the last byte; a first position at the size is unsatisfiable, one before it is
/// the last byte; a suffix longer than the file is the whole file, a suffix of 0 is unsatisfiable;
/// a file of no bytes has no satisfiable range.
///
/// Catches: an off-by-one at the end of the file, a clamp that is missing, an underflow on an empty
/// file.
#[test]
fn a_range_is_resolved_against_the_size_at_its_edges() {
    let part = |first, last| Asked::Part { first, last };
    let cases = [
        (Spec::Between(0, 99), 100, part(0, 99)),
        (Spec::Between(0, 100), 100, part(0, 99)),
        (Spec::Between(10, u64::MAX), 100, part(10, 99)),
        (Spec::Between(99, 99), 100, part(99, 99)),
        (Spec::Between(100, 100), 100, Asked::Unsatisfiable),
        (Spec::Between(100, 200), 100, Asked::Unsatisfiable),
        (Spec::From(99), 100, part(99, 99)),
        (Spec::From(100), 100, Asked::Unsatisfiable),
        (Spec::From(u64::MAX), 100, Asked::Unsatisfiable),
        (Spec::Suffix(1), 100, part(99, 99)),
        (Spec::Suffix(100), 100, part(0, 99)),
        (Spec::Suffix(101), 100, part(0, 99)),
        (Spec::Suffix(u64::MAX), 100, part(0, 99)),
        (Spec::Suffix(0), 100, Asked::Unsatisfiable),
        (Spec::Between(0, 0), 0, Asked::Unsatisfiable),
        (Spec::From(0), 0, Asked::Unsatisfiable),
        (Spec::Suffix(1), 0, Asked::Unsatisfiable),
        (Spec::Between(0, 0), 1, part(0, 0)),
    ];
    for (spec, size, expected) in cases {
        assert_eq!(resolve(spec, size), expected, "{spec:?} of {size}");
    }
}

/// Proves: headers with no `Range` ask for the whole file, whatever `If-Range` says; a header that
/// is not text is not understood and asks for the whole file.
///
/// Catches: an `If-Range` alone turned into a part, a header of raw bytes that is a failure.
#[test]
fn headers_with_no_understood_range_ask_for_the_whole_file() {
    let etag = "\"abc\"";
    let mut headers = HeaderMap::new();
    assert_eq!(super::asked(&headers, etag, 10), Asked::Whole);
    headers.insert(IF_RANGE, HeaderValue::from_static("\"abc\""));
    assert_eq!(super::asked(&headers, etag, 10), Asked::Whole);
    headers.insert(
        RANGE,
        HeaderValue::from_bytes(b"bytes=0-\xff").expect("a value"),
    );
    assert_eq!(super::asked(&headers, etag, 10), Asked::Whole);
    headers.insert(RANGE, HeaderValue::from_static("bytes=2-3"));
    assert_eq!(
        super::asked(&headers, etag, 10),
        Asked::Part { first: 2, last: 3 }
    );
}
