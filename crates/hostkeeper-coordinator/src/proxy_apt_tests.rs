//! The pieces of the package proxy's apt routes ([`super`]) that a request through the listener
//! does not reach: the answer to a refusal of the cache, and the headers of a `HEAD` when no
//! place among the reads in flight is free.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use hostkeeper_enrollment::validity::UnixClock;
use hyper::StatusCode;

use super::{Head, Wanted, head_of, refused, respond};
use crate::proxy_cache::{FetchError, Key, Limits, ProxyCache, ProxyError};
use crate::testutil::scratch;

struct Time;

impl UnixClock for Time {
    fn now_unix(&self) -> u64 {
        1_800_000_000
    }
}

/// Proves: each way the cache refuses has its status and code: no place for another read is 503
/// `busy`, a file the origin lacks is 404, a failing origin is 502 `origin_failed`, and a range
/// past the end is 416 with the length in `Content-Range`.
///
/// Catches: a busy proxy reported as a broken origin, or a 416 without the length a client needs.
#[test]
fn each_refusal_of_the_cache_has_its_status() {
    let wanted = Wanted {
        name: "pub",
        under: "dists/stable/InRelease",
    };
    let check = |why: ProxyError, status: StatusCode| {
        assert_eq!(refused(&wanted, &why).status(), status, "{why}");
    };
    check(ProxyError::Busy, StatusCode::SERVICE_UNAVAILABLE);
    check(
        ProxyError::Origin(FetchError::NotFound),
        StatusCode::NOT_FOUND,
    );
    check(
        ProxyError::Origin(FetchError::Failed("reset".to_owned())),
        StatusCode::BAD_GATEWAY,
    );
    let past = refused(&wanted, &ProxyError::Unsatisfiable(7));
    assert_eq!(past.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(past.headers()["content-range"], "bytes */7");
}

/// Proves: a HEAD for a file the cache does not hold, with every place among the reads taken, is
/// `Busy` and the origin is not read.
///
/// Catches: HEADs that bypass `proxy_fetches`.
#[tokio::test]
async fn a_head_with_no_place_for_a_read_is_busy_and_reads_nothing() {
    let dir = scratch("pp2-head-busy");
    let none = Limits {
        fetches: 0,
        ..Limits::from_settings(&BTreeMap::new())
    };
    let cache = ProxyCache::open(dir.path(), Arc::new(Time), &none).expect("a cache");
    let asked = Arc::new(AtomicU64::new(0));
    let witness = Arc::clone(&asked);
    let origin = Box::pin(async move {
        witness.store(1, std::sync::atomic::Ordering::SeqCst);
        Err(FetchError::NotFound)
    });
    let head = head_of(
        &cache,
        &Key::new("https://origin.test/apt", "pool/a.deb"),
        &none,
        origin,
    )
    .await;
    assert_eq!(head.err(), Some(ProxyError::Busy));
    assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 0);
}

/// Proves: a 206 whose origin did not say the length of the whole file names it `*` in
/// `Content-Range`, and has the length of its span; a weak tag is sent as the `ETag`, and a date
/// validator is not.
///
/// Catches: a `Content-Range` that invents a total, or a date sent as a tag.
#[test]
fn a_range_of_an_unsized_file_has_a_star_for_its_total() {
    let head = |validator: &str| Head {
        validator: Some(validator.to_owned()),
        last_modified: None,
        length: None,
    };
    let weak = respond(
        &hyper::HeaderMap::new(),
        head("W/\"w\""),
        Some((5, 9)),
        None,
    );
    assert_eq!(weak.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(weak.headers()["content-range"], "bytes 5-9/*");
    assert_eq!(weak.headers()["content-length"], "5");
    assert_eq!(weak.headers()["etag"], "W/\"w\"");
    let dated = respond(
        &hyper::HeaderMap::new(),
        head("Mon, 02 Nov 2026 10:00:00 GMT"),
        None,
        None,
    );
    assert_eq!(dated.status(), StatusCode::OK);
    assert!(dated.headers().get("etag").is_none());
    assert!(dated.headers().get("content-length").is_none());
}
