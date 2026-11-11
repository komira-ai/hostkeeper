//! The pull-through of `GET /pkg/<sha256>.pkg` (`docs/design/package-proxy.md`, section 8): a pkg
//! an install names is read from the install's `url` once, checked against its digest before it is
//! kept, and a second ask is a hit; a digest no install names is `404` with no origin request. The
//! origin is served in process ([`crate::repository_origin_support`]); no socket is opened.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::StreamExt as _;
use hostkeeper_enrollment::validity::SystemClock;
use http_body_util::BodyExt as _;
use hyper::header::{HeaderMap, HeaderName, HeaderValue};
use hyper::{Method, StatusCode};
use sha2::{Digest, Sha256};

use super::super::pkg;
use super::super::support::Rig;
use super::verified;
use crate::pkg_origin::PkgOrigin;
use crate::proxy_cache::{FetchError, Limits, ProxyCache};
use crate::repository_origin_support::{Answer, FakeOrigin, Then, response};

const ORIGIN: &str = "https://origin.test/pkgs";

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// A pkg of `len` bytes that differ from one `seed` to the next.
fn bytes_of(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| u8::try_from(i % 251).unwrap_or(0).wrapping_add(seed))
        .collect()
}

/// A rig that fetches pkgs from an origin answering `answer` by path, with a cache.
struct World {
    rig: Rig,
    fake: FakeOrigin,
    cache: Arc<ProxyCache>,
}

fn world(
    name: &str,
    with_dir: bool,
    answer: impl Fn(&str) -> Then + Send + Sync + 'static,
) -> World {
    let mut rig = if with_dir {
        Rig::new(name)
    } else {
        Rig::without_pkg_dir(name)
    };
    let fake = FakeOrigin::new();
    let cache = Arc::new(
        ProxyCache::open(
            &rig.store_dir().join("proxy"),
            Arc::new(SystemClock),
            &Limits::from_settings(&std::collections::BTreeMap::new()),
        )
        .expect("a cache"),
    );
    let origin = PkgOrigin::new(fake.roots())
        .expect("a client")
        .with_dial(fake.dial(Arc::new(answer) as Answer));
    let coordinator = Arc::get_mut(&mut rig.coordinator).expect("a coordinator no one else holds");
    coordinator.proxy_cache = Some(Arc::clone(&cache));
    coordinator.pkg_origin = Some(Arc::new(origin));
    World { rig, fake, cache }
}

/// The path an origin that serves `bytes` under the name of its digest answers.
fn serving(bytes: &[u8]) -> impl Fn(&str) -> Then + Send + Sync + 'static + use<> {
    let (path, body) = (format!("/pkgs/{}.pkg", sha(bytes)), bytes.to_vec());
    move |asked| {
        if asked == path {
            Then::Write(response("200 OK", &body))
        } else {
            Then::Write(response("404 Not Found", b""))
        }
    }
}

/// Sets the setting `installs` to one install of `digest`, with `url` when given.
fn install(rig: &Rig, entries: &[(&str, Option<String>)]) {
    let rows: Vec<String> = entries
        .iter()
        .enumerate()
        .map(|(i, (digest, url))| {
            let url = url
                .as_ref()
                .map_or(String::new(), |url| format!(r#","url":"{url}""#));
            format!(
                r#"{{"name":"pkg-{i}","bundle_id":"dev.hostkeeper.hostkeeper","sha256":"{digest}","version":"0.1.66","path":"/var/lib/hostkeeper-mdm-pkg/{digest}.pkg"{url}}}"#
            )
        })
        .collect();
    rig.setup
        .set_setting("installs", &format!("[{}]", rows.join(",")))
        .expect("set");
}

fn named(digest: &str) -> (&str, Option<String>) {
    (digest, Some(format!("{ORIGIN}/{digest}.pkg")))
}

struct Seen {
    status: StatusCode,
    headers: HeaderMap,
    body: Result<Vec<u8>, ()>,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(self.body.as_ref().expect("a body")).into_owned()
    }
}

async fn ask(rig: &Rig, method: &str, digest: &str, headers: &[(&str, &str)]) -> Seen {
    let mut map = HeaderMap::new();
    for (name, value) in headers {
        map.insert(
            HeaderName::try_from(*name).expect("a name"),
            HeaderValue::try_from(*value).expect("a value"),
        );
    }
    let method = Method::from_bytes(method.as_bytes()).expect("a method");
    let path = format!("/pkg/{digest}.pkg");
    let response = pkg::serve(&rig.coordinator, &rig.server, &method, &path, &map).await;
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

async fn get(rig: &Rig, digest: &str) -> Seen {
    ask(rig, "GET", digest, &[]).await
}

const BAD_GATEWAY: &str = r#"{"error":"origin_failed"}"#;
const NOT_FOUND: &str = r#"{"error":"not_found"}"#;

fn objects(world: &World) -> usize {
    std::fs::read_dir(world.rig.store_dir().join("proxy").join("objects"))
        .expect("the objects")
        .count()
}

/// Proves: a pkg an install names, with a `url` and not in any directory, is read from that `url`
/// once, answered `200` with its bytes, length, digest `ETag` and `Accept-Ranges`, and kept: a
/// second `GET`, a `HEAD`, a `Range` and a conditional request are all answered from the cache,
/// and the origin is read once in all.
///
/// Catches: a pkg that is fetched again for every Mac, and one whose cached copy is answered
/// without the validators and ranges of a provided pkg.
#[tokio::test]
async fn a_pkg_the_origin_gives_is_stored_and_a_second_ask_is_a_hit() {
    let bytes = bytes_of(1, 150_000);
    let digest = sha(&bytes);
    let world = world("pp3-hit", false, serving(&bytes));
    install(&world.rig, &[named(&digest)]);
    let first = get(&world.rig, &digest).await;
    assert_eq!(first.status, StatusCode::OK);
    assert_eq!(first.body.as_ref().expect("a body"), &bytes);
    assert_eq!(first.header("content-length"), Some("150000"));
    assert_eq!(first.header("etag"), Some(format!("\"{digest}\"").as_str()));
    assert_eq!(first.header("accept-ranges"), Some("bytes"));
    assert_eq!(world.fake.dials(), 1);
    assert_eq!(world.cache.bytes(), 150_000);
    let heard = world.fake.heard();
    assert!(
        heard[0].starts_with(&format!("GET /pkgs/{digest}.pkg HTTP/1.1\r\n")),
        "{heard:?}"
    );
    let second = get(&world.rig, &digest).await;
    assert_eq!(second.body.as_ref().expect("a body"), &bytes);
    let head = ask(&world.rig, "HEAD", &digest, &[]).await;
    assert_eq!(head.header("content-length"), Some("150000"));
    assert_eq!(head.body, Ok(Vec::new()));
    let part = ask(&world.rig, "GET", &digest, &[("range", "bytes=10-19")]).await;
    assert_eq!(part.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(part.body.as_ref().expect("a body"), &bytes[10..20]);
    let tag = format!("\"{digest}\"");
    let same = ask(&world.rig, "GET", &digest, &[("if-none-match", &tag)]).await;
    assert_eq!(same.status, StatusCode::NOT_MODIFIED);
    assert_eq!(world.fake.dials(), 1);
    assert!(world.cache.stats().hits >= 4);
}

/// Proves: bytes that are not the digest's (the same length, so only the hash tells) are answered
/// `502 origin_failed`, nothing of them is kept (no counted bytes, no object, no temp file), and
/// the next ask reads the origin again and, now that it gives the right bytes, stores and serves
/// them.
///
/// Catches: a poisoned or swapped pkg that is stored and then handed to every Mac, and a bad read
/// that leaves the key stuck.
#[tokio::test]
async fn bytes_that_are_not_the_digest_are_refused_and_not_stored() {
    let right = bytes_of(2, 70_000);
    let digest = sha(&right);
    let wrong = bytes_of(3, 70_000);
    let reads = Arc::new(AtomicUsize::new(0));
    let world = {
        let (right, wrong, reads) = (right.clone(), wrong, Arc::clone(&reads));
        self::world("pp3-mismatch", false, move |_| {
            let body = if reads.fetch_add(1, Ordering::SeqCst) == 0 {
                &wrong
            } else {
                &right
            };
            Then::Write(response("200 OK", body))
        })
    };
    install(&world.rig, &[named(&digest)]);
    let refused = get(&world.rig, &digest).await;
    assert_eq!(refused.status, StatusCode::BAD_GATEWAY);
    assert_eq!(refused.text(), BAD_GATEWAY);
    assert_eq!(world.cache.bytes(), 0);
    assert_eq!(objects(&world), 0);
    let tmp = world.rig.store_dir().join("proxy").join("tmp");
    assert_eq!(std::fs::read_dir(tmp).expect("tmp").count(), 0);
    assert!(
        world
            .cache
            .open_hit(&crate::proxy_cache::Key::new(
                "pkg",
                format!("{digest}.pkg")
            ))
            .is_none()
    );
    let served = get(&world.rig, &digest).await;
    assert_eq!(served.status, StatusCode::OK);
    assert_eq!(served.body.as_ref().expect("a body"), &right);
    assert_eq!(world.fake.dials(), 2);
    assert_eq!(world.cache.bytes(), 70_000);
}

/// Proves: a body cut short of its declared length, and one that ends in an error, are `502` and
/// stored nowhere.
///
/// Catches: a truncated pkg kept as if whole.
#[tokio::test]
async fn a_body_that_is_cut_short_is_refused_and_not_stored() {
    let bytes = bytes_of(4, 5_000);
    let digest = sha(&bytes);
    let cut = bytes[..1_000].to_vec();
    let world = world("pp3-cut", false, move |_| {
        let mut head = b"HTTP/1.1 200 OK\r\nContent-Length: 5000\r\n\r\n".to_vec();
        head.extend_from_slice(&cut);
        Then::Write(head)
    });
    install(&world.rig, &[named(&digest)]);
    let refused = get(&world.rig, &digest).await;
    assert_eq!(refused.status, StatusCode::BAD_GATEWAY);
    assert_eq!(refused.text(), BAD_GATEWAY);
    assert_eq!((world.cache.bytes(), objects(&world)), (0, 0));
}

/// Proves: a digest no install names is `404` with no read of the origin, even when the origin has
/// a file of that name; so is a digest whose install has no `url`, and so is one with a `url` on a
/// coordinator that has no cache, or a cache and no reader of origins; none of them reads any origin.
///
/// Catches: an open proxy that fetches whatever a client names, and a pull on a coordinator that
/// was not given a cache directory.
#[tokio::test]
async fn a_digest_no_install_can_fetch_is_not_found_and_no_origin_is_read() {
    let bytes = bytes_of(5, 3_000);
    let digest = sha(&bytes);
    let other = bytes_of(6, 3_000);
    let other_digest = sha(&other);
    let both = {
        let (a, b) = (serving(&bytes), serving(&other));
        move |path: &str| match a(path) {
            Then::Write(first) if first.starts_with(b"HTTP/1.1 200") => Then::Write(first),
            _ => b(path),
        }
    };
    let world = world("pp3-unnamed", false, both);
    install(&world.rig, &[named(&digest), (&other_digest, None)]);
    for asked in [sha(b"nothing names this"), other_digest.clone()] {
        let seen = get(&world.rig, &asked).await;
        assert_eq!(
            (seen.status, seen.text().as_str()),
            (StatusCode::NOT_FOUND, NOT_FOUND)
        );
    }
    assert_eq!(world.fake.dials(), 0);
    // A coordinator with no cache, and one with a cache and no reader of origins.
    let mut bare = world;
    let coordinator = Arc::get_mut(&mut bare.rig.coordinator).expect("a coordinator");
    coordinator.pkg_origin = None;
    assert_eq!(get(&bare.rig, &digest).await.status, StatusCode::NOT_FOUND);
    let coordinator = Arc::get_mut(&mut bare.rig.coordinator).expect("a coordinator");
    coordinator.proxy_cache = None;
    assert_eq!(get(&bare.rig, &digest).await.status, StatusCode::NOT_FOUND);
    assert_eq!(bare.fake.dials(), 0);
}

/// Proves: a pkg in the pkg directory is served from it with no origin read though its install has
/// a `url`; a pkg that directory lacks is pulled.
///
/// Catches: a provided file that is fetched anyway, and a directory that stops the pull.
#[tokio::test]
async fn a_provided_pkg_is_served_from_its_directory_and_a_missing_one_is_pulled() {
    let (here, there) = (bytes_of(7, 2_000), bytes_of(8, 2_000));
    let (here_digest, there_digest) = (sha(&here), sha(&there));
    let world = world("pp3-provided", true, serving(&there));
    install(&world.rig, &[named(&here_digest), named(&there_digest)]);
    std::fs::create_dir_all(world.rig.pkg_dir()).expect("a directory");
    std::fs::write(
        world.rig.pkg_dir().join(format!("{here_digest}.pkg")),
        &here,
    )
    .expect("written");
    let provided = get(&world.rig, &here_digest).await;
    assert_eq!(provided.body.as_ref().expect("a body"), &here);
    assert_eq!(world.fake.dials(), 0);
    let pulled = get(&world.rig, &there_digest).await;
    assert_eq!(pulled.body.as_ref().expect("a body"), &there);
    assert_eq!(world.fake.dials(), 1);
}

/// Proves: an origin that answers a status but 200 is `502`; a redirect is followed to the pkg
/// (and the pkg stored), and one past `proxy_redirects` (a setting read at each request) is `502`,
/// with no URL in either answer.
///
/// Catches: a GitHub release asset that cannot be fetched, a redirect loop that never ends, and a
/// tunable that is read from nowhere.
#[tokio::test]
async fn an_origin_that_fails_or_redirects_is_a_bad_gateway_within_the_limit() {
    let bytes = bytes_of(9, 4_000);
    let digest = sha(&bytes);
    let start = format!("/pkgs/{digest}.pkg");
    let body = bytes.clone();
    let world = world("pp3-redirect", false, move |path| {
        if path == start {
            Then::Write(
                b"HTTP/1.1 302 Found\r\nLocation: /asset?sig=secret\r\nContent-Length: 0\r\n\r\n"
                    .to_vec(),
            )
        } else if path.starts_with("/asset") {
            Then::Write(response("200 OK", &body))
        } else {
            Then::Write(response("500 Oops", b""))
        }
    });
    install(&world.rig, &[named(&digest)]);
    world
        .rig
        .setup
        .set_setting("proxy_redirects", "0")
        .expect("set");
    let refused = get(&world.rig, &digest).await;
    assert_eq!(
        (refused.status, refused.text().as_str()),
        (StatusCode::BAD_GATEWAY, BAD_GATEWAY)
    );
    assert_eq!(world.cache.bytes(), 0);
    world
        .rig
        .setup
        .set_setting("proxy_redirects", "1")
        .expect("set");
    let followed = get(&world.rig, &digest).await;
    assert_eq!(followed.body.as_ref().expect("a body"), &bytes);
    assert_eq!(world.cache.bytes(), 4_000);
    let failing = self::world("pp3-failing", false, |_| {
        Then::Write(response("500 Oops", b""))
    });
    install(&failing.rig, &[named(&digest)]);
    let seen = get(&failing.rig, &digest).await;
    assert_eq!(
        (seen.status, seen.text().as_str()),
        (StatusCode::BAD_GATEWAY, BAD_GATEWAY)
    );
}

/// Proves: askers that arrive together are one read of the origin and all get the pkg.
///
/// Catches: a rack of Macs that pull one file once each over the uplink.
#[tokio::test]
async fn askers_that_arrive_together_cost_one_read_of_the_origin() {
    let bytes = bytes_of(10, 90_000);
    let digest = sha(&bytes);
    let world = world("pp3-together", false, serving(&bytes));
    install(&world.rig, &[named(&digest)]);
    let (a, b, c) = tokio::join!(
        get(&world.rig, &digest),
        get(&world.rig, &digest),
        get(&world.rig, &digest)
    );
    for seen in [a, b, c] {
        assert_eq!(seen.status, StatusCode::OK);
        assert_eq!(seen.body.as_ref().expect("a body"), &bytes);
    }
    assert_eq!(world.fake.dials(), 1);
}

/// Proves: a pkg the cache cannot keep (larger than `proxy_cache_max_bytes`) is checked and then
/// answered `503 pkg_not_stored`, with nothing kept.
///
/// Catches: an answer sent before the digest was known, and a pkg kept beyond the cache's limit.
#[tokio::test]
async fn a_pkg_the_cache_cannot_keep_is_not_answered() {
    let bytes = bytes_of(11, 10_000);
    let digest = sha(&bytes);
    let world = world("pp3-too-big", false, serving(&bytes));
    install(&world.rig, &[named(&digest)]);
    world
        .rig
        .setup
        .set_setting("proxy_cache_max_bytes", "1000")
        .expect("set");
    let seen = get(&world.rig, &digest).await;
    assert_eq!(seen.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(seen.text(), r#"{"error":"pkg_not_stored"}"#);
    assert_eq!((world.cache.bytes(), objects(&world)), (0, 0));
}

/// Proves: with every place of `proxy_fetches` taken, another pkg is `503 busy`.
///
/// Catches: a proxy that opens as many connections to the origin as Macs ask.
#[tokio::test]
async fn a_pkg_asked_while_every_fetch_is_busy_is_refused() {
    let (slow, quick) = (bytes_of(12, 4_000), bytes_of(13, 4_000));
    let (slow_digest, quick_digest) = (sha(&slow), sha(&quick));
    let stalled = format!("/pkgs/{slow_digest}.pkg");
    let world = world("pp3-busy", false, move |path| {
        if path == stalled {
            Then::Stall(b"HTTP/1.1 200 OK\r\nContent-Length: 4000\r\n\r\nabc".to_vec())
        } else {
            Then::Write(response("200 OK", b""))
        }
    });
    install(&world.rig, &[named(&slow_digest), named(&quick_digest)]);
    world
        .rig
        .setup
        .set_setting("proxy_fetches", "1")
        .expect("set");
    let mut first = std::pin::pin!(get(&world.rig, &slow_digest));
    assert!(futures::poll!(first.as_mut()).is_pending());
    let busy = get(&world.rig, &quick_digest).await;
    assert_eq!(busy.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(busy.text(), r#"{"error":"busy"}"#);
}

/// Proves: an asker whose wait for another's read ends (`proxy_wait_s`, on the paused clock) is
/// `503 pkg_pending` with `Retry-After` of that wait, and is not given a second read of the pkg.
///
/// Catches: fifty Macs that each pull the whole pkg because one read is slow.
#[tokio::test(start_paused = true)]
async fn a_waiter_whose_wait_ends_is_told_to_come_back() {
    let bytes = bytes_of(14, 4_000);
    let digest = sha(&bytes);
    let world = world("pp3-pending", false, |_| {
        Then::Stall(b"HTTP/1.1 200 OK\r\nContent-Length: 4000\r\n\r\nabc".to_vec())
    });
    install(&world.rig, &[named(&digest)]);
    world
        .rig
        .setup
        .set_setting("proxy_wait_s", "2")
        .expect("set");
    let mut leader = std::pin::pin!(get(&world.rig, &digest));
    assert!(futures::poll!(leader.as_mut()).is_pending());
    let waiter = get(&world.rig, &digest).await;
    assert_eq!(waiter.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(waiter.text(), r#"{"error":"pkg_pending"}"#);
    assert_eq!(waiter.header("retry-after"), Some("2"));
}

/// Proves: `verified` passes the frames of a body whose digest is right, ends a body whose digest is
/// not in an error and then stops, and passes an error of the body on and stops.
///
/// Catches: a check that ends the body as whole when it is not, or one that goes on after an error.
#[tokio::test]
async fn the_check_ends_a_wrong_body_in_one_error() {
    let frames =
        |items: Vec<Result<&'static [u8], &'static str>>| -> crate::proxy_cache::BodyStream {
            Box::pin(futures::stream::iter(items.into_iter().map(|item| {
                item.map(bytes::Bytes::from_static)
                    .map_err(|why| FetchError::Failed(why.to_owned()))
            })))
        };
    let digest = sha(b"abcdef");
    let whole: Vec<_> = verified(frames(vec![Ok(b"abc"), Ok(b"def")]), digest.clone())
        .collect()
        .await;
    assert_eq!(whole.len(), 2);
    assert!(whole.iter().all(Result::is_ok));
    let wrong: Vec<_> = verified(frames(vec![Ok(b"abc"), Ok(b"xyz")]), digest.clone())
        .collect()
        .await;
    assert_eq!(wrong.len(), 3);
    assert!(matches!(wrong[2], Err(FetchError::Failed(_))));
    let broken: Vec<_> = verified(frames(vec![Ok(b"abc"), Err("cut"), Ok(b"def")]), digest)
        .collect()
        .await;
    assert_eq!(broken.len(), 2);
    assert_eq!(broken[1], Err(FetchError::Failed("cut".to_owned())));
}
