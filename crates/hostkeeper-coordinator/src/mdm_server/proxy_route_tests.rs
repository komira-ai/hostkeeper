//! The package proxy's apt routes on the MDM listener ([`crate::proxy_apt`]), driven through
//! [`crate::grpc::route`] on the MDM surface (and, for the headers a connection decides, over an
//! in-memory TLS connection) against an origin served in process
//! ([`crate::repository_origin_support`]): what a client is answered, what the origin is asked,
//! and what is kept. The time is the rig's clock, which a test moves; no socket is opened.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::{Request, StatusCode};

use super::support::{Rig, Session};
use crate::grpc::route;
use crate::host_groups_support::coordinator_listed;
use crate::http::Surface;
use crate::marks_support::operator;
use crate::mdm_setup::MdmSetup;
use crate::proxy_cache::{Limits, ProxyCache};
use crate::proxy_origin::ProxyOrigin;
use crate::repositories_support::{credential_in, private, repository};
use crate::repository_origin_support::{FakeOrigin, Then, bare, reply};
use crate::service::Coordinator;
use crate::store::mdm_setup::{SecretRow, SetupError, SetupStore};
use crate::testutil::{Log, scratch};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;

const DATE: &str = "Mon, 02 Nov 2026 10:00:00 GMT";
const EARLIER: &str = "Sun, 01 Nov 2026 10:00:00 GMT";
const LATER: &str = "Tue, 03 Nov 2026 10:00:00 GMT";
const RELEASE: &str = "dists/stable/InRelease";

/// The bytes of the pool file: a pattern longer than the stream's buffers.
fn package() -> Vec<u8> {
    (0..300_000_u32)
        .map(|n| u8::try_from(n % 251).expect("a byte"))
        .collect()
}

/// The value of the header `name` in a request head.
fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

/// The origin: the index at version `version` (its entity tag follows it), a pool file, a file
/// that redirects, one that redirects to http, one that loops, one that is gone, one that breaks.
fn site(version: Arc<AtomicU64>) -> impl Fn(&str) -> Then + Send + Sync + 'static {
    move |head| {
        let mut words = head.split_whitespace();
        let (method, path) = (words.next().unwrap_or(""), words.next().unwrap_or(""));
        let v = version.load(Ordering::SeqCst);
        let etag = format!("\"v{v}\"");
        match path {
            "/apt/dists/stable/InRelease" => {
                if header(head, "if-none-match") == Some(etag.as_str()) {
                    return bare(
                        &["HTTP/1.1 304 Not Modified", &format!("ETag: {etag}")],
                        b"",
                    );
                }
                reply(
                    "200 OK",
                    &[("ETag", &etag), ("Last-Modified", DATE)],
                    format!("release v{v}").as_bytes(),
                )
            }
            "/apt/pool/main/a.deb" | "/cdn/a.deb" => pool(head, method),
            "/apt/pool/main/moved.deb" => bare(
                &[
                    "HTTP/1.1 302 Found",
                    "Location: https://origin.test/cdn/a.deb",
                ],
                b"",
            ),
            "/apt/pool/main/plain.deb" => bare(
                &[
                    "HTTP/1.1 302 Found",
                    "Location: http://origin.test/cdn/a.deb",
                ],
                b"",
            ),
            "/apt/pool/main/loop.deb" => bare(
                &["HTTP/1.1 302 Found", "Location: /apt/pool/main/loop.deb"],
                b"",
            ),
            "/apt/dists/stable/gone" => reply("404 Not Found", &[], b""),
            "/apt/pool/main/chunked.deb" => bare(
                &[
                    "HTTP/1.1 200 OK",
                    "Transfer-Encoding: chunked",
                    "ETag: W/\"c\"",
                ],
                b"5\r\nhello\r\n0\r\n\r\n",
            ),
            "/apt/pool/main/stall.deb" => {
                Then::Stall(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n0123456789".to_vec())
            }
            _ => reply("500 Oops", &[], b""),
        }
    }
}

/// The pool file, whole or a range of it, or only its headers.
fn pool(head: &str, method: &str) -> Then {
    let all = package();
    let headers = [("ETag", "\"pkg\""), ("Last-Modified", DATE)];
    if method == "HEAD" {
        return bare(
            &[
                "HTTP/1.1 200 OK",
                "ETag: \"pkg\"",
                &format!("Last-Modified: {DATE}"),
                &format!("Content-Length: {}", all.len()),
            ],
            b"",
        );
    }
    let Some(span) = header(head, "range").and_then(|r| r.strip_prefix("bytes=")) else {
        return reply("200 OK", &headers, &all);
    };
    let (first, last) = span.split_once('-').expect("a span");
    let first: usize = first.parse().expect("a start");
    let last: usize = last.parse().map_or(all.len() - 1, |last: usize| last);
    let content = format!("bytes {first}-{last}/{}", all.len());
    reply(
        "206 Partial Content",
        &[("ETag", "\"pkg\""), ("Content-Range", &content)],
        &all[first..=last],
    )
}

/// What a test builds on: the rig, a coordinator that serves the proxy's routes over a cache and
/// an origin client, and the origin.
struct World {
    rig: Rig,
    coordinator: Arc<Coordinator>,
    fake: FakeOrigin,
    cache: ProxyCache,
    version: Arc<AtomicU64>,
    _dirs: (hostkeeper_testkit::Scratch, hostkeeper_testkit::Scratch),
}

/// What a world leaves out or changes.
#[derive(Default)]
struct Without {
    cache: bool,
    client: bool,
    /// The setup whose settings the coordinator reads, in place of the rig's.
    setup: Option<Arc<MdmSetup>>,
}

fn world_with(name: &str, without: &Without) -> World {
    let rig = Rig::bare(name);
    rig.store_tls();
    let version = Arc::new(AtomicU64::new(1));
    let fake = FakeOrigin::new();
    let dial = fake.dial_heads(Arc::new(site(Arc::clone(&version))));
    let cache_dir = scratch(&format!("{name}-cache"));
    let files = scratch(&format!("{name}-files"));
    let limits = Limits::from_settings(&BTreeMap::new());
    let cache = ProxyCache::open(cache_dir.path(), rig.time.clone(), &limits).expect("a cache");
    let mut coordinator = coordinator_listed(&rig.fake, &Arc::new(AtomicU64::new(1_000)), None, "")
        .with_shared_mdm_setup(
            without
                .setup
                .clone()
                .unwrap_or_else(|| Arc::clone(&rig.setup)),
        )
        .with_mdm_server(Some(Arc::clone(&rig.server)));
    if !without.cache {
        coordinator = coordinator.with_proxy_cache(Some(cache.clone()));
    }
    if !without.client {
        let client = ProxyOrigin::new(fake.roots())
            .expect("a client")
            .with_dial(dial);
        coordinator = coordinator.with_proxy_origin(client);
    }
    let credential = credential_in(files.path(), "credential", "Bearer token\n");
    for entry in [
        repository("pub", "https://origin.test/apt", &["stable"], 2),
        private(
            "priv",
            "https://origin.test/apt",
            &["stable"],
            1,
            &credential,
        ),
    ] {
        coordinator
            .repositories_put(
                &operator(),
                proto::PutRepositoryRequest {
                    repository: Some(entry),
                    reason: "for a test".to_owned(),
                },
            )
            .expect("put");
    }
    World {
        rig,
        coordinator: Arc::new(coordinator),
        fake,
        cache,
        version,
        _dirs: (cache_dir, files),
    }
}

impl World {
    fn new(name: &str) -> Self {
        world_with(name, &Without::default())
    }

    /// The answer of `method path` with `headers` on the MDM surface.
    async fn send(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
    ) -> (StatusCode, hyper::HeaderMap, Bytes) {
        let mut request = Request::builder().method(method).uri(path);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let request = request.body(Full::new(Bytes::new())).expect("a request");
        let response = route(
            &self.coordinator,
            Surface::Mdm,
            &None,
            request,
            std::time::Duration::from_secs(30),
        )
        .await;
        let (parts, body) = response.into_parts();
        let bytes = body.collect().await.expect("a body").to_bytes();
        (parts.status, parts.headers, bytes)
    }

    async fn get(&self, path: &str) -> (StatusCode, hyper::HeaderMap, Bytes) {
        self.send("GET", path, &[]).await
    }

    /// The requests the origin heard, as `METHOD /path`.
    fn heard(&self) -> Vec<String> {
        self.fake
            .heard()
            .iter()
            .map(|head| head.lines().next().unwrap_or("").replace(" HTTP/1.1", ""))
            .collect()
    }

    fn advance(&self, seconds: u64) {
        self.rig.time.advance(seconds);
    }
}

/// Proves: a request that is not a `GET` or `HEAD` of a file of a public repository of the
/// registry is refused with its status and no request is made to any origin: other methods 405, an
/// unknown repository, a private one, a name that is not a repository name and a path with nothing
/// after the repository 404, and an empty, `.` or `..` segment, a `%` and a path that ends in `/`
/// 400.
///
/// Catches: an open proxy, which is the route's shape and not its caller's identity being the
/// only guard (package-proxy.md section 3).
#[tokio::test]
async fn a_request_outside_the_route_shape_is_refused_before_any_origin_is_read() {
    let world = World::new("pp2-refuse");
    for (method, path, status, code) in [
        (
            "POST",
            "/apt/pub/dists/stable/InRelease",
            405,
            "method_not_allowed",
        ),
        (
            "PUT",
            "/apt/pub/dists/stable/InRelease",
            405,
            "method_not_allowed",
        ),
        (
            "DELETE",
            "/apt/pub/dists/stable/InRelease",
            405,
            "method_not_allowed",
        ),
        ("GET", "/apt/nope/dists/stable/InRelease", 404, "not_found"),
        ("GET", "/apt/priv/dists/stable/InRelease", 404, "not_found"),
        (
            "GET",
            "/apt/Pub_Repo/dists/stable/InRelease",
            404,
            "not_found",
        ),
        ("GET", "/apt/pub", 404, "not_found"),
        ("GET", "/apt/", 404, "not_found"),
        ("GET", "/apt/pub/", 400, "bad_request"),
        ("GET", "/apt/pub/dists/../InRelease", 400, "bad_request"),
        ("GET", "/apt/pub/dists/./InRelease", 400, "bad_request"),
        ("GET", "/apt/pub/dists//InRelease", 400, "bad_request"),
        ("GET", "/apt/pub/dists/%2e%2e/InRelease", 400, "bad_request"),
        ("GET", "/apt/pub/dists/stable/", 400, "bad_request"),
        ("HEAD", "/apt/pub/../priv/x", 400, "bad_request"),
    ] {
        let (got, _, body) = world.send(method, path, &[]).await;
        assert_eq!(got.as_u16(), status, "{method} {path}");
        assert_eq!(
            body,
            format!(r#"{{"error":"{code}"}}"#).as_bytes(),
            "{method} {path}"
        );
    }
    assert_eq!(world.heard(), Vec::<String>::new());
}

/// Proves: with no cache directory every route is 404, with no origin client a read is 502, and
/// neither makes a request to an origin.
///
/// Catches: a proxy that answers before a deployment opts in.
#[tokio::test]
async fn a_coordinator_without_a_cache_answers_404_and_without_a_client_502() {
    let bare_world = world_with(
        "pp2-nocache",
        &Without {
            cache: true,
            ..Without::default()
        },
    );
    let (status, _, _) = bare_world.get(&format!("/apt/pub/{RELEASE}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(bare_world.heard().is_empty());
    let clientless = world_with(
        "pp2-noclient",
        &Without {
            client: true,
            ..Without::default()
        },
    );
    let (status, _, body) = clientless.get(&format!("/apt/pub/{RELEASE}")).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body, br#"{"error":"origin_failed"}"#.as_slice());
    assert!(clientless.heard().is_empty());
}

/// Proves: the proxy's routes are served with the certificate of the name alone while the MDM
/// routes answer 503, and with no certificate they answer 503 too.
///
/// Catches: a Linux-only farm that never uploads an Apple credential losing its package proxy
/// (package-proxy.md section 3).
#[tokio::test]
async fn the_apt_routes_are_served_without_any_mdm_credential() {
    let world = World::new("pp2-family");
    let mut session = Session::dial_to(&world.rig, &world.coordinator, None)
        .await
        .expect("connected");
    let (status, _, body) = session
        .fetch("GET", &format!("/apt/pub/{RELEASE}"), &[])
        .await
        .expect("an answer");
    assert_eq!(
        (status, body.as_slice()),
        (StatusCode::OK, b"release v1".as_slice())
    );
    assert_eq!(
        session.put("/mdm", &[], b"x").await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    // A coordinator with no MDM server does not know the route: 404, as for any other path.
    let without = Arc::new(coordinator_listed(
        &world.rig.fake,
        &Arc::new(AtomicU64::new(1_000)),
        None,
        "",
    ));
    let request = Request::builder()
        .uri(format!("/apt/pub/{RELEASE}"))
        .body(Full::new(Bytes::new()))
        .expect("a request");
    let response = route(
        &without,
        Surface::Mdm,
        &None,
        request,
        std::time::Duration::from_secs(30),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let unserved = Rig::bare("pp2-nocert");
    let coordinator = Arc::new(
        coordinator_listed(&unserved.fake, &Arc::new(AtomicU64::new(1_000)), None, "")
            .with_mdm_server(Some(Arc::clone(&unserved.server))),
    );
    let request = Request::builder()
        .uri(format!("/apt/pub/{RELEASE}"))
        .body(Full::new(Bytes::new()))
        .expect("a request");
    let response = route(
        &coordinator,
        Surface::Mdm,
        &None,
        request,
        std::time::Duration::from_secs(30),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

/// Proves: an index is read from the origin once and then answered from the cache with the
/// origin's validators until its TTL passes; then it is one conditional read, whose 304 renews it
/// (no body read) and whose 200 replaces it with the new index.
///
/// Catches: an `apt update` that reads the whole index from the origin every time, and one that
/// never sees a release because the index is never revalidated.
#[tokio::test]
async fn an_index_is_served_inside_its_ttl_and_revalidated_after_it() {
    let world = World::new("pp2-ttl");
    let path = format!("/apt/pub/{RELEASE}");
    let (status, headers, body) = world.get(&path).await;
    assert_eq!(
        (status, body.as_ref()),
        (StatusCode::OK, b"release v1".as_slice())
    );
    assert_eq!(headers["etag"], "\"v1\"");
    assert_eq!(headers["last-modified"], DATE);
    assert_eq!(headers["content-length"], "10");
    assert_eq!(headers["accept-ranges"], "bytes");
    world.advance(299);
    let (_, headers, body) = world.get(&path).await;
    assert_eq!(body.as_ref(), b"release v1");
    assert_eq!(headers["etag"], "\"v1\"");
    assert_eq!(world.heard(), ["GET /apt/dists/stable/InRelease"]);
    // Past the TTL: one conditional read, answered 304, and the stored index is renewed.
    world.advance(2);
    let (_, _, body) = world.get(&path).await;
    assert_eq!(body.as_ref(), b"release v1");
    let heard = world.fake.heard();
    assert_eq!(heard.len(), 2);
    assert_eq!(header(&heard[1], "if-none-match"), Some("\"v1\""));
    assert_eq!(header(&heard[1], "if-modified-since"), Some(DATE));
    world.advance(100);
    world.get(&path).await;
    assert_eq!(world.fake.heard().len(), 2);
    // The origin publishes: the next revalidation after the TTL is a 200 with the new index.
    world.version.store(2, Ordering::SeqCst);
    world.advance(301);
    let (status, headers, body) = world.get(&path).await;
    assert_eq!(
        (status, body.as_ref()),
        (StatusCode::OK, b"release v2".as_slice())
    );
    assert_eq!(headers["etag"], "\"v2\"");
    let (_, _, again) = world.get(&path).await;
    assert_eq!(again.as_ref(), b"release v2");
    assert_eq!(world.fake.heard().len(), 3);
}

/// Proves: the TTL is the setting `proxy_index_ttl_s`: raised, an index older than the default is
/// still served without a read; with settings that cannot be read the defaults apply and the
/// proxy goes on.
///
/// Catches: a tunable that is not a setting, or a proxy that stops when its store does.
#[tokio::test]
async fn the_index_ttl_is_the_setting_and_an_unreadable_store_leaves_the_default() {
    let world = World::new("pp2-setting");
    let path = format!("/apt/pub/{RELEASE}");
    world
        .rig
        .setup
        .set_setting("proxy_index_ttl_s", "1000")
        .expect("set");
    world.get(&path).await;
    world.advance(900);
    world.get(&path).await;
    assert_eq!(world.heard().len(), 1);
    world.advance(101);
    world.get(&path).await;
    assert_eq!(world.heard().len(), 2);

    let log = Log::default();
    let _guard = log.install();
    let down = Arc::new(MdmSetup::new(
        Arc::new(Unreadable),
        None,
        crate::enrollment::os_random(),
    ));
    let world = world_with(
        "pp2-unreadable",
        &Without {
            setup: Some(down),
            ..Without::default()
        },
    );
    world.get(&path).await;
    world.advance(299);
    let (status, _, body) = world.get(&path).await;
    assert_eq!(
        (status, body.as_ref()),
        (StatusCode::OK, b"release v1".as_slice())
    );
    assert_eq!(world.heard().len(), 1);
    world.advance(2);
    world.get(&path).await;
    assert_eq!(world.heard().len(), 2);
    assert!(
        log.text()
            .contains("the package proxy could not read its settings; the defaults apply"),
        "{}",
        log.text()
    );
}

const POOL: &str = "/apt/pub/pool/main/a.deb";

/// Proves: a pool file is read from the origin once, streamed whole with its length and
/// validators, and then answered from the cache however much later, with no read; the cache
/// counts one fill and one hit.
///
/// Catches: a package pulled over the uplink by every host, or revalidated though it is immutable.
#[tokio::test]
async fn a_pool_file_is_fetched_once_and_never_revalidated() {
    let world = World::new("pp2-pool");
    let (status, headers, body) = world.get(POOL).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_ref(), package().as_slice());
    assert_eq!(headers["content-length"], "300000");
    assert_eq!(headers["etag"], "\"pkg\"");
    world.advance(86_400 * 30);
    let (status, _, again) = world.get(POOL).await;
    assert_eq!(
        (status, again.as_ref()),
        (StatusCode::OK, package().as_slice())
    );
    assert_eq!(world.heard(), ["GET /apt/pool/main/a.deb"]);
    let stats = world.cache.stats();
    assert_eq!((stats.fills, stats.hits, stats.passed), (1, 1, 0));
}

/// Proves: twenty hosts asking for one file at once cause one origin read, and every one of them
/// is given the whole file.
///
/// Catches: fifty hosts running the same update becoming fifty pulls over the uplink.
#[tokio::test]
async fn twenty_askers_of_one_file_cause_one_origin_read() {
    let world = World::new("pp2-crowd");
    let answers = futures::future::join_all((0..20).map(|_| world.get(POOL))).await;
    for (status, _, body) in answers {
        assert_eq!(
            (status, body.as_ref()),
            (StatusCode::OK, package().as_slice())
        );
    }
    assert_eq!(world.heard(), ["GET /apt/pool/main/a.deb"]);
}

/// Proves: a client that holds the file is answered 304 with no body and, for a file in the cache
/// and inside its TTL, no origin read: by `If-None-Match` (the tag, its weak form, a list holding
/// it, `*`) and by `If-Modified-Since` (the date and a later one); a tag or date that is not the
/// file's, a date that is not a date, and a date beside a tag that does not match are 200.
///
/// Catches: a host that downloads an index it already has.
#[tokio::test]
async fn a_client_that_holds_the_file_is_answered_304() {
    let world = World::new("pp2-conditional");
    let path = format!("/apt/pub/{RELEASE}");
    world.get(&path).await;
    for (name, value) in [
        ("if-none-match", "\"v1\""),
        ("if-none-match", "W/\"v1\""),
        ("if-none-match", "\"other\", \"v1\""),
        ("if-none-match", "*"),
        ("if-modified-since", DATE),
        ("if-modified-since", LATER),
    ] {
        let (status, headers, body) = world.send("GET", &path, &[(name, value)]).await;
        assert_eq!(status, StatusCode::NOT_MODIFIED, "{name}: {value}");
        assert!(body.is_empty());
        assert_eq!(headers["etag"], "\"v1\"");
        assert!(headers.get("content-length").is_none());
    }
    for (name, value) in [
        ("if-none-match", "\"v2\""),
        ("if-modified-since", EARLIER),
        ("if-modified-since", "not a date"),
    ] {
        let (status, _, body) = world.send("GET", &path, &[(name, value)]).await;
        assert_eq!(status, StatusCode::OK, "{name}: {value}");
        assert_eq!(body.as_ref(), b"release v1");
    }
    let (status, _, _) = world
        .send(
            "GET",
            &path,
            &[("if-none-match", "\"v2\""), ("if-modified-since", LATER)],
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(world.heard().len(), 1);
}

/// Proves: a conditional request of a file the cache does not hold fills it, and is 304 when the
/// origin's tag is the client's; the fill still commits, so the next ask is a hit.
///
/// Catches: a conditional request that leaves the cache cold for the hosts after it.
#[tokio::test]
async fn a_conditional_request_of_a_file_not_held_still_fills_it() {
    let world = World::new("pp2-conditional-miss");
    let path = format!("/apt/pub/{RELEASE}");
    let (status, _, body) = world
        .send("GET", &path, &[("if-none-match", "\"v1\"")])
        .await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);
    assert!(body.is_empty());
    let (status, _, body) = world.get(&path).await;
    assert_eq!(
        (status, body.as_ref()),
        (StatusCode::OK, b"release v1".as_slice())
    );
    assert_eq!(world.heard().len(), 1);
}

/// Proves: a range of a file the cache does not hold is passed to the origin and answered 206
/// with the origin's `Content-Range`, and nothing is stored; a whole request afterwards fills the
/// cache.
///
/// Catches: a partial body committed as the whole file.
#[tokio::test]
async fn a_range_of_a_file_not_held_is_passed_through_and_stores_nothing() {
    let world = World::new("pp2-range-miss");
    let (status, headers, body) = world.send("GET", POOL, &[("range", "bytes=100-199")]).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(headers["content-range"], "bytes 100-199/300000");
    assert_eq!(headers["content-length"], "100");
    assert_eq!(body.as_ref(), &package()[100..200]);
    assert_eq!(world.cache.bytes(), 0);
    let stats = world.cache.stats();
    assert_eq!((stats.fills, stats.passed), (0, 1));
    // A span the cache cannot resolve without the length is the whole file, and fills it.
    let (status, _, body) = world.send("GET", POOL, &[("range", "bytes=-10")]).await;
    assert_eq!((status, body.len()), (StatusCode::OK, 300_000));
    assert_eq!(world.cache.bytes(), 300_000);
    assert_eq!(world.heard().len(), 2);
}

/// Proves: a range of a file in the cache is read from the file with no origin read: a span, an
/// open span, the last bytes; one past the end is 416 with the length; a span that is invalid,
/// several, in another unit or empty is ignored and the whole file is the answer; an `If-Range`
/// that is the file's tag or date keeps the range, and one that is not, or is weak, drops it.
///
/// Catches: a resumed download that restarts, or that is given the wrong bytes.
#[tokio::test]
async fn a_range_of_a_held_file_is_read_from_the_file() {
    let world = World::new("pp2-range-hit");
    world.get(POOL).await;
    let all = package();
    for (range, first, last) in [
        ("bytes=10-19", 10, 19),
        ("bytes=299990-", 299_990, 299_999),
        ("bytes=-10", 299_990, 299_999),
        ("bytes=5-999999999", 5, 299_999),
    ] {
        let (status, headers, body) = world.send("GET", POOL, &[("range", range)]).await;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT, "{range}");
        assert_eq!(
            headers["content-range"],
            format!("bytes {first}-{last}/300000").as_str()
        );
        assert_eq!(body.as_ref(), &all[first..=last], "{range}");
    }
    let (status, headers, _) = world.send("GET", POOL, &[("range", "bytes=300000-")]).await;
    assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(headers["content-range"], "bytes */300000");
    for range in [
        "bytes=5-2",
        "items=1-2",
        "bytes=1-2,5-6",
        "bytes=a-b",
        "bytes=-0",
        "bytes=-",
        "bytes=-x",
        "bytes=99999999999999999999-",
        "bytes=1",
        "bytes=1-b",
    ] {
        let (status, _, body) = world.send("GET", POOL, &[("range", range)]).await;
        assert_eq!((status, body.len()), (StatusCode::OK, 300_000), "{range}");
    }
    for (condition, ranged) in [
        ("\"pkg\"", true),
        (DATE, true),
        ("\"other\"", false),
        (EARLIER, false),
        ("W/\"pkg\"", false),
    ] {
        let (status, _, body) = world
            .send(
                "GET",
                POOL,
                &[("range", "bytes=0-3"), ("if-range", condition)],
            )
            .await;
        let expected = if ranged {
            (StatusCode::PARTIAL_CONTENT, 4)
        } else {
            (StatusCode::OK, 300_000)
        };
        assert_eq!((status, body.len()), expected, "{condition}");
    }
    assert_eq!(world.heard().len(), 1);
}

/// Proves: an `If-Range` with no file in the cache to compare it with drops the range, and the
/// whole file is the answer.
///
/// Catches: a range honoured against a file nobody can say is the one the client holds.
#[tokio::test]
async fn an_if_range_without_a_stored_file_drops_the_range() {
    let world = World::new("pp2-if-range-miss");
    let (status, _, body) = world
        .send(
            "GET",
            POOL,
            &[("range", "bytes=0-3"), ("if-range", "\"pkg\"")],
        )
        .await;
    assert_eq!((status, body.len()), (StatusCode::OK, 300_000));
}

/// Proves: a HEAD of a file the cache does not hold is one HEAD of the origin and stores nothing;
/// of a pool file in the cache, or an index inside its TTL, it is the headers from the file with
/// no origin read; of an index past its TTL it is one HEAD of the origin; of a file the origin
/// does not have it is 404; and a HEAD with the client's tag is 304.
///
/// Catches: a HEAD that downloads the file, or one that reports a stale index's length.
#[tokio::test]
async fn a_head_is_the_headers_of_the_same_answer() {
    let world = World::new("pp2-head");
    let (status, headers, body) = world.send("HEAD", POOL, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_empty());
    assert_eq!(headers["content-length"], "300000");
    assert_eq!(headers["etag"], "\"pkg\"");
    assert_eq!(world.heard(), ["HEAD /apt/pool/main/a.deb"]);
    assert_eq!(world.cache.bytes(), 0);
    world.get(POOL).await;
    let (_, headers, _) = world.send("HEAD", POOL, &[]).await;
    assert_eq!(headers["content-length"], "300000");
    assert_eq!(world.heard().len(), 2);
    let index = format!("/apt/pub/{RELEASE}");
    world.get(&index).await;
    let (_, headers, _) = world.send("HEAD", &index, &[]).await;
    assert_eq!(headers["content-length"], "10");
    assert_eq!(world.heard().len(), 3);
    world.advance(301);
    world.version.store(2, Ordering::SeqCst);
    let (status, headers, _) = world.send("HEAD", &index, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["etag"], "\"v2\"");
    assert!(world.heard().last().expect("a head").starts_with("HEAD "));
    let (status, _, _) = world.send("HEAD", "/apt/pub/dists/stable/gone", &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = world
        .send("HEAD", POOL, &[("if-none-match", "\"pkg\"")])
        .await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);
}

/// Proves: over a connection a HEAD keeps the length of the body it stands for and has no body, and
/// a GET's body arrives whole with its length.
///
/// Catches: a HEAD whose `Content-Length` the server rewrote to 0.
#[tokio::test]
async fn over_a_connection_a_head_keeps_its_content_length() {
    let world = World::new("pp2-connection");
    let mut session = Session::dial_to(&world.rig, &world.coordinator, None)
        .await
        .expect("connected");
    let (status, headers, body) = session
        .fetch("HEAD", "/apt/pub/pool/main/a.deb", &[])
        .await
        .expect("an answer");
    assert_eq!(
        (status, headers["content-length"].to_str().expect("text")),
        (StatusCode::OK, "300000")
    );
    assert!(body.is_empty());
    let mut session = Session::dial_to(&world.rig, &world.coordinator, None)
        .await
        .expect("connected");
    let (status, headers, body) = session
        .fetch("GET", "/apt/pub/pool/main/a.deb", &[])
        .await
        .expect("an answer");
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-length"], "300000");
    assert_eq!(body, package());
}

/// Proves: a redirect of the origin to an https URL is followed and the file it leads to is
/// stored under the name that was asked; a redirect to http, and a loop past `proxy_redirects`
/// (the setting, raised or lowered), are 502 and store nothing.
///
/// Catches: a release asset the proxy cannot fetch, a downgrade to plain HTTP, a redirect loop.
#[tokio::test]
async fn the_origins_redirects_are_followed_within_the_limit_and_over_https_only() {
    let world = World::new("pp2-redirect");
    let (status, _, body) = world.get("/apt/pub/pool/main/moved.deb").await;
    assert_eq!(
        (status, body.as_ref()),
        (StatusCode::OK, package().as_slice())
    );
    world.get("/apt/pub/pool/main/moved.deb").await;
    assert_eq!(
        world.heard(),
        ["GET /apt/pool/main/moved.deb", "GET /cdn/a.deb"]
    );

    let (status, _, body) = world.get("/apt/pub/pool/main/plain.deb").await;
    assert_eq!(
        (status, body.as_ref()),
        (
            StatusCode::BAD_GATEWAY,
            br#"{"error":"origin_failed"}"#.as_slice()
        )
    );
    let before = world.heard().len();
    let (status, _, _) = world.get("/apt/pub/pool/main/loop.deb").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(world.heard().len() - before, 4);
    world
        .rig
        .setup
        .set_setting("proxy_redirects", "1")
        .expect("set");
    let before = world.heard().len();
    world.get("/apt/pub/pool/main/loop.deb").await;
    assert_eq!(world.heard().len() - before, 2);
    assert_eq!(world.cache.bytes(), 300_000);
}

/// Proves: the origin's 404 is 404, any other failure of it is 502 `origin_failed`, and neither
/// leaves anything in the cache.
///
/// Catches: a missing file reported as a broken origin, or a broken answer kept.
#[tokio::test]
async fn the_origins_404_is_404_and_its_other_failures_are_502() {
    let world = World::new("pp2-failures");
    let log = Log::default();
    let _guard = log.install();
    let (status, _, body) = world.get("/apt/pub/dists/stable/gone").await;
    assert_eq!(
        (status, body.as_ref()),
        (
            StatusCode::NOT_FOUND,
            br#"{"error":"not_found"}"#.as_slice()
        )
    );
    let (status, _, body) = world.get("/apt/pub/dists/stable/anything-else").await;
    assert_eq!(
        (status, body.as_ref()),
        (
            StatusCode::BAD_GATEWAY,
            br#"{"error":"origin_failed"}"#.as_slice()
        )
    );
    assert_eq!(world.cache.bytes(), 0);
    let text = log.text();
    assert!(
        text.contains("repository=pub path=dists/stable/anything-else why=the origin failed: the origin answered 500"),
        "{text}"
    );
    assert!(
        !text.contains("dists/stable/gone"),
        "a 404 is not a failure: {text}"
    );
}

/// Proves: an origin that answers in chunks, with no length, is streamed to the client with no
/// `Content-Length`, kept, and then answered with the length of the file and its weak tag, which
/// the client's weak tag matches.
///
/// Catches: a file of unknown length refused, or kept with a wrong length.
#[tokio::test]
async fn a_chunked_origin_is_streamed_without_a_length_and_then_kept_with_one() {
    let world = World::new("pp2-chunked");
    let path = "/apt/pub/pool/main/chunked.deb";
    let (status, headers, body) = world.get(path).await;
    assert_eq!(
        (status, body.as_ref()),
        (StatusCode::OK, b"hello".as_slice())
    );
    assert!(headers.get("content-length").is_none());
    assert_eq!(headers["etag"], "W/\"c\"");
    let (_, headers, body) = world.get(path).await;
    assert_eq!(headers["content-length"], "5");
    assert_eq!(body.as_ref(), b"hello");
    let (status, _, _) = world
        .send("GET", path, &[("if-none-match", "W/\"c\"")])
        .await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);
    assert_eq!(world.heard().len(), 1);
}

/// Proves: a body the origin stops sending ends the client's response with an error and not with
/// an end that looks whole, and the cache keeps nothing of it.
///
/// Catches: a truncated package handed to apt as complete.
#[tokio::test(start_paused = true)]
async fn a_body_that_breaks_off_ends_the_response_in_an_error() {
    let world = World::new("pp2-breaks");
    let request = Request::builder()
        .uri("/apt/pub/pool/main/stall.deb")
        .body(Full::new(Bytes::new()))
        .expect("a request");
    let response = route(
        &world.coordinator,
        Surface::Mdm,
        &None,
        request,
        std::time::Duration::from_secs(30),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-length"], "100");
    assert!(response.into_body().collect().await.is_err());
    assert_eq!(world.cache.bytes(), 0);
}

/// A setup store whose settings cannot be read.
struct Unreadable;

impl SetupStore for Unreadable {
    fn put_secret(&self, _: &SecretRow) -> Result<(), SetupError> {
        Ok(())
    }

    fn secrets(&self) -> Result<Vec<SecretRow>, SetupError> {
        Ok(Vec::new())
    }

    fn prune_superseded(&self, _: i64) -> Result<u64, SetupError> {
        Ok(0)
    }

    fn settings(&self) -> Result<BTreeMap<String, String>, SetupError> {
        Err(SetupError::Store("down".to_owned()))
    }

    fn set_setting(&self, _: &str, _: &str) -> Result<(), SetupError> {
        Ok(())
    }
}
