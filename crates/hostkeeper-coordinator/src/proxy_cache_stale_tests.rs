//! The index half of the package proxy's cache ([`super`]): what an index inside its TTL, one past
//! it, a changed one, a removed one, a failing origin and a crowd of askers each do. The time is
//! a clock the test moves, the origin is a script whose replies are given in order, and the
//! bodies that wait wait for an explicit release (no sleeps).

use std::collections::VecDeque;
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use futures::stream;
use hostkeeper_enrollment::validity::UnixClock;
use tokio::sync::Semaphore;

use super::{
    Answer, ByteRange, Condition, FetchError, Fetched, Key, Limits, Origin, ProxyCache, ProxyError,
    Request,
};
use crate::proxy_cache::tests::{limits, read, scratch};
use crate::proxy_cache::{Meta, Served};

const START: u64 = 1_800_000_000;
const DATE: &str = "Mon, 02 Nov 2026 10:00:00 GMT";

/// A clock the test moves.
struct Time(AtomicU64);

impl UnixClock for Time {
    fn now_unix(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

impl Time {
    fn advance(&self, seconds: u64) {
        self.0.fetch_add(seconds, Ordering::SeqCst);
    }
}

/// One answer of the scripted origin.
#[derive(Clone)]
enum Reply {
    /// A 200 with this body and entity tag.
    Body(&'static [u8], &'static str),
    /// A 304.
    Unchanged,
    /// A 404.
    NotFound,
    /// The headers fail.
    Fail,
    /// The reply waits for a release of the gate first.
    Gated(Box<Reply>),
    /// The reply never comes.
    Hang,
}

/// The origin's side of a test: its replies in order, and what it was asked.
struct Script {
    replies: Mutex<VecDeque<Reply>>,
    asked: Mutex<Vec<Request>>,
    gate: Arc<Semaphore>,
}

impl Script {
    fn new(replies: impl IntoIterator<Item = Reply>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into_iter().collect()),
            asked: Mutex::new(Vec::new()),
            gate: Arc::new(Semaphore::new(0)),
        })
    }

    fn calls(&self) -> usize {
        self.asked.lock().expect("the log").len()
    }

    fn asked(&self, n: usize) -> Request {
        self.asked.lock().expect("the log")[n].clone()
    }

    fn release(&self, n: usize) {
        self.gate.add_permits(n);
    }
}

fn fetched(reply: Reply) -> Result<Fetched, FetchError> {
    match reply {
        Reply::Body(bytes, etag) => Ok(Fetched {
            validator: Some(etag.to_owned()),
            last_modified: Some(DATE.to_owned()),
            unchanged: false,
            slice: None,
            length: Some(bytes.len() as u64),
            body: Box::pin(stream::iter([Ok(Bytes::from_static(bytes))])),
        }),
        Reply::Unchanged => Ok(Fetched {
            validator: None,
            last_modified: None,
            unchanged: true,
            slice: None,
            length: None,
            body: Box::pin(stream::empty()),
        }),
        Reply::NotFound => Err(FetchError::NotFound),
        Reply::Fail | Reply::Hang | Reply::Gated(_) => Err(FetchError::Failed("reset".to_owned())),
    }
}

fn origin(script: &Arc<Script>) -> Origin {
    let script = Arc::clone(script);
    Arc::new(move |request| {
        let script = Arc::clone(&script);
        script.asked.lock().expect("the log").push(request);
        let reply = script
            .replies
            .lock()
            .expect("the script")
            .pop_front()
            .expect("an unexpected read of the origin");
        Box::pin(async move {
            let mut reply = reply;
            if let Reply::Gated(inner) = reply {
                script.gate.acquire().await.expect("the gate").forget();
                reply = *inner;
            }
            if matches!(reply, Reply::Hang) {
                std::future::pending::<()>().await;
            }
            fetched(reply)
        })
    })
}

fn key() -> Key {
    Key::new("https://example.test/apt", "dists/stable/InRelease")
}

/// A cache over a scratch directory and a clock at [`START`].
fn open(name: &str) -> (hostkeeper_testkit::Scratch, ProxyCache, Arc<Time>) {
    let dir = scratch(name);
    let time = Arc::new(Time(AtomicU64::new(START)));
    let cache = ProxyCache::open(dir.path(), time.clone(), &limits()).expect("a cache");
    (dir, cache, time)
}

async fn index(cache: &ProxyCache, origin: &Origin) -> Result<Answer, ProxyError> {
    cache.get_index(&key(), None, origin, &limits()).await
}

/// Proves: an index inside `proxy_index_ttl_s` is answered from its file with no origin read, up to
/// the last second of it.
///
/// Catches: a revalidation at every `apt update`, which is what the TTL is for.
#[tokio::test]
async fn an_index_inside_its_ttl_is_not_asked_of_the_origin_again() {
    let (_dir, cache, time) = open("pp2-fresh");
    let script = Script::new([Reply::Body(b"release v1", "\"v1\"")]);
    let origin = origin(&script);
    let first = index(&cache, &origin).await.expect("an answer");
    assert_eq!(first.served, Served::Filled);
    assert_eq!(read(first).await.expect("body"), b"release v1");
    time.advance(299);
    let second = index(&cache, &origin).await.expect("an answer");
    assert_eq!(second.served, Served::Hit);
    assert_eq!(read(second).await.expect("body"), b"release v1");
    assert_eq!(script.calls(), 1);
}

/// Proves: past the TTL the next ask is one conditional read with the validators the file was
/// stored with, and the origin's 304 renews the file (in memory and in its sidecar, which a new
/// cache over the directory reads), answers from the file, and is the last read for another TTL.
///
/// Catches: a 304 that is not remembered, so every ask after the TTL asks the origin again.
#[tokio::test]
async fn a_304_after_the_ttl_renews_the_file_and_is_answered_from_it() {
    let (dir, cache, time) = open("pp2-renew");
    let script = Script::new([Reply::Body(b"release v1", "\"v1\""), Reply::Unchanged]);
    let origin = origin(&script);
    read(index(&cache, &origin).await.expect("an answer"))
        .await
        .expect("body");
    time.advance(301);
    let renewed = index(&cache, &origin).await.expect("an answer");
    assert_eq!(renewed.served, Served::Hit);
    assert_eq!(renewed.fetched_at, START + 301);
    assert_eq!(read(renewed).await.expect("body"), b"release v1");
    assert_eq!(script.calls(), 2);
    let asked = script.asked(1);
    assert_eq!(
        asked.condition,
        Some(Condition {
            etag: Some("\"v1\"".to_owned()),
            last_modified: Some(DATE.to_owned()),
        })
    );
    assert_eq!(asked.range, None);
    time.advance(100);
    read(index(&cache, &origin).await.expect("an answer"))
        .await
        .expect("body");
    assert_eq!(script.calls(), 2);
    let reopened = ProxyCache::open(dir.path(), time.clone(), &limits()).expect("a cache");
    assert_eq!(
        reopened.peek(&key()).map(|m| m.fetched_at),
        Some(START + 301)
    );
    assert!(crate::proxy_cache::tests::names(dir.path(), "tmp").is_empty());
}

/// Proves: past the TTL an origin that has a newer index has it forwarded to the asker and
/// stored in place of the old one, which is gone from the books and the disk; the next ask is a
/// hit of the new one.
///
/// Catches: a stale index served after the origin changed, or both kept on the disk.
#[tokio::test]
async fn a_changed_index_replaces_the_old_one() {
    let (dir, cache, time) = open("pp2-changed");
    let script = Script::new([
        Reply::Body(b"release v1", "\"v1\""),
        Reply::Body(b"release v2 is longer", "\"v2\""),
    ]);
    let origin = origin(&script);
    read(index(&cache, &origin).await.expect("an answer"))
        .await
        .expect("body");
    time.advance(301);
    let changed = index(&cache, &origin).await.expect("an answer");
    assert_eq!(changed.served, Served::Filled);
    assert_eq!(changed.validator.as_deref(), Some("\"v2\""));
    assert_eq!(read(changed).await.expect("body"), b"release v2 is longer");
    // The fill commits in a task; the next ask finds the file or waits for it.
    let after = index(&cache, &origin).await.expect("an answer");
    assert_eq!(after.served, Served::Hit);
    assert_eq!(read(after).await.expect("body"), b"release v2 is longer");
    assert_eq!(script.calls(), 2);
    assert_eq!(cache.bytes(), 20);
    let d = key().digest();
    assert_eq!(
        crate::proxy_cache::tests::names(dir.path(), "objects"),
        [d.clone(), format!("{d}.meta")]
    );
}

/// Proves: past the TTL an origin that no longer has the file answers `NotFound` to the asker and
/// the stored index is forgotten, with its files.
///
/// Catches: an index that was withdrawn upstream still being handed to hosts.
#[tokio::test]
async fn a_withdrawn_index_is_forgotten() {
    let (dir, cache, time) = open("pp2-withdrawn");
    let script = Script::new([Reply::Body(b"release v1", "\"v1\""), Reply::NotFound]);
    let origin = origin(&script);
    read(index(&cache, &origin).await.expect("an answer"))
        .await
        .expect("body");
    time.advance(301);
    let gone = index(&cache, &origin).await.err();
    assert_eq!(gone, Some(ProxyError::Origin(FetchError::NotFound)));
    assert!(cache.peek(&key()).is_none());
    assert_eq!(cache.bytes(), 0);
    assert!(crate::proxy_cache::tests::names(dir.path(), "objects").is_empty());
}

/// Proves: past the TTL a failing origin fails the ask (a stale index is not served) and keeps the
/// stored file, so that the next ask revalidates and a 304 then serves it.
///
/// Catches: a stale index served while the origin is down, which could hide a release, or a stored
/// index dropped for one failed read.
#[tokio::test]
async fn a_failing_origin_fails_the_ask_and_keeps_the_stored_index() {
    let (_dir, cache, time) = open("pp2-failing");
    let script = Script::new([
        Reply::Body(b"release v1", "\"v1\""),
        Reply::Fail,
        Reply::Unchanged,
    ]);
    let origin = origin(&script);
    read(index(&cache, &origin).await.expect("an answer"))
        .await
        .expect("body");
    time.advance(301);
    assert!(matches!(
        index(&cache, &origin).await.err(),
        Some(ProxyError::Origin(FetchError::Failed(_)))
    ));
    assert!(cache.peek(&key()).is_some());
    let healed = index(&cache, &origin).await.expect("an answer");
    assert_eq!(healed.served, Served::Hit);
    assert_eq!(read(healed).await.expect("body"), b"release v1");
}

/// Proves: with every place among the reads taken, a stale ask is `Busy` and the stored index is
/// kept.
///
/// Catches: a revalidation that ignores `proxy_fetches`.
#[tokio::test]
async fn a_stale_ask_with_no_place_for_a_read_is_busy() {
    let (_dir, cache, time) = open("pp2-busy");
    let script = Script::new([Reply::Body(b"release v1", "\"v1\"")]);
    let origin = origin(&script);
    read(index(&cache, &origin).await.expect("an answer"))
        .await
        .expect("body");
    time.advance(301);
    let none = Limits {
        fetches: 0,
        ..limits()
    };
    let busy = cache.get_index(&key(), None, &origin, &none).await.err();
    assert_eq!(busy, Some(ProxyError::Busy));
    assert!(cache.peek(&key()).is_some());
    assert_eq!(script.calls(), 1);
}

/// Proves: a range of a stale index whose origin says it is unchanged is answered as a range of
/// the file.
///
/// Catches: a range request that loses its range when it revalidates.
#[tokio::test]
async fn a_range_of_a_renewed_index_is_a_range_of_the_file() {
    let (_dir, cache, time) = open("pp2-range");
    let script = Script::new([Reply::Body(b"0123456789", "\"v1\""), Reply::Unchanged]);
    let origin = origin(&script);
    read(index(&cache, &origin).await.expect("an answer"))
        .await
        .expect("body");
    time.advance(301);
    let range = Some(ByteRange {
        start: 2,
        end: Some(4),
    });
    let answer = cache
        .get_index(&key(), range, &origin, &limits())
        .await
        .expect("an answer");
    assert_eq!(answer.range, Some((2, 4)));
    assert_eq!(read(answer).await.expect("body"), b"234");
}

/// Proves: ten askers of a stale index make one conditional read between them, and all of them are
/// answered from the renewed file.
///
/// Catches: a rack of hosts running `apt update` at once asking the origin once each.
#[tokio::test]
async fn ten_askers_of_a_stale_index_cause_one_revalidation() {
    let (_dir, cache, time) = open("pp2-crowd");
    let script = Script::new([
        Reply::Body(b"release v1", "\"v1\""),
        Reply::Gated(Box::new(Reply::Unchanged)),
    ]);
    let origin = origin(&script);
    read(index(&cache, &origin).await.expect("an answer"))
        .await
        .expect("body");
    time.advance(301);
    let askers: Vec<_> = (0..10)
        .map(|_| {
            let (cache, origin) = (cache.clone(), origin.clone());
            tokio::spawn(async move {
                let answer = index(&cache, &origin).await.expect("an answer");
                let served = answer.served;
                (served, read(answer).await.expect("body"))
            })
        })
        .collect();
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert_eq!(script.calls(), 2);
    script.release(1);
    for asker in askers {
        let (served, body) = asker.await.expect("an asker");
        assert_eq!(served, Served::Hit);
        assert_eq!(body, b"release v1");
    }
    assert_eq!(script.calls(), 2);
}

/// Proves: a waiter for a revalidation that does not finish in `proxy_wait_s` is passed through to
/// the origin on its own, and the revalidation it did not wait for is not its concern.
///
/// Catches: fifty hosts stalled for the whole of one slow revalidation.
#[tokio::test(start_paused = true)]
async fn a_waiter_for_a_revalidation_that_hangs_is_passed_through() {
    let (_dir, cache, time) = open("pp2-hang");
    let script = Script::new([
        Reply::Body(b"release v1", "\"v1\""),
        Reply::Hang,
        Reply::Body(b"release v2", "\"v2\""),
    ]);
    let origin = origin(&script);
    read(index(&cache, &origin).await.expect("an answer"))
        .await
        .expect("body");
    time.advance(301);
    let leader = {
        let (cache, origin) = (cache.clone(), origin.clone());
        tokio::spawn(async move { index(&cache, &origin).await.map(|_| ()) })
    };
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert_eq!(script.calls(), 2);
    let waiter = tokio::spawn({
        let (cache, origin) = (cache.clone(), origin.clone());
        async move { index(&cache, &origin).await }
    });
    tokio::time::advance(Duration::from_secs(11)).await;
    let passed = waiter.await.expect("a waiter").expect("an answer");
    assert_eq!(passed.served, Served::Passed);
    assert_eq!(read(passed).await.expect("body"), b"release v2");
    assert_eq!(script.asked(2).condition, None);
    leader.abort();
}

/// Proves: a waiter for a revalidation whose new body could not be stored (it is larger than the
/// cache may hold) reads the origin on its own, the old index is not served in between, and the
/// stored index is gone.
///
/// Catches: a replacement that cannot be kept leaving the old index to be served for ever.
#[tokio::test]
async fn a_replacement_too_large_to_keep_leaves_no_old_index_and_passes_the_waiters() {
    let (_dir, cache, time) = open("pp2-oversize");
    let script = Script::new([
        Reply::Body(b"short", "\"v1\""),
        Reply::Gated(Box::new(Reply::Body(
            b"far too long for this cache",
            "\"v2\"",
        ))),
        Reply::Body(b"far too long for this cache", "\"v2\""),
    ]);
    let origin = origin(&script);
    let small = Limits {
        max_bytes: 10,
        ..limits()
    };
    let ask = |cache: ProxyCache, origin: Origin, small: Limits| async move {
        let answer = cache
            .get_index(&key(), None, &origin, &small)
            .await
            .expect("an answer");
        let served = answer.served;
        (served, read(answer).await.expect("body"))
    };
    ask(cache.clone(), origin.clone(), small.clone()).await;
    time.advance(301);
    let leader = tokio::spawn(ask(cache.clone(), origin.clone(), small.clone()));
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    let waiter = tokio::spawn(ask(cache.clone(), origin.clone(), small.clone()));
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    script.release(1);
    let (led, led_body) = leader.await.expect("a leader");
    assert_eq!(led, Served::Filled);
    assert_eq!(led_body, b"far too long for this cache");
    let (waited, waited_body) = waiter.await.expect("a waiter");
    assert_eq!(waited, Served::Passed);
    assert_eq!(waited_body, b"far too long for this cache");
    assert!(cache.peek(&key()).is_none());
    assert_eq!(script.calls(), 3);
}

/// Proves: a renewal of an object that was replaced since it was read leaves the new one alone,
/// and a renewal whose sidecar cannot be written still renews it in memory.
///
/// Catches: a late 304 stamping a newer object with an older one's confirmation.
#[tokio::test]
async fn a_late_renewal_leaves_a_newer_object_alone() {
    let (dir, cache, time) = open("pp2-late");
    let script = Script::new([Reply::Body(b"release v1", "\"v1\"")]);
    let origin = origin(&script);
    read(index(&cache, &origin).await.expect("an answer"))
        .await
        .expect("body");
    let born = cache.shared.state().entries[&key().digest()].born;
    time.advance(500);
    cache.renew(&key(), born + 1);
    assert_eq!(cache.peek(&key()).map(|m| m.fetched_at), Some(START));
    fs::remove_dir_all(dir.path().join("tmp")).expect("tmp removed");
    cache.renew(&key(), born);
    assert_eq!(cache.peek(&key()).map(|m| m.fetched_at), Some(START + 500));
}

/// Proves: a condition is made of what the object was stored with: its tag and its date, a date
/// alone when that is all the validator is, a weak tag as a tag, and nothing for an object with no
/// validator.
///
/// Catches: a revalidation that sends a date as `If-None-Match`, or sends nothing for a weak tag.
#[test]
fn a_condition_is_made_of_the_validators_the_object_was_stored_with() {
    let meta = |validator: Option<&str>, last_modified: Option<&str>| Meta {
        validator: validator.map(str::to_owned),
        last_modified: last_modified.map(str::to_owned),
        fetched_at: START,
        length: 1,
    };
    let condition = |etag: Option<&str>, last_modified: Option<&str>| Condition {
        etag: etag.map(str::to_owned),
        last_modified: last_modified.map(str::to_owned),
    };
    assert_eq!(
        Condition::of(&meta(Some("\"a\""), Some(DATE))),
        condition(Some("\"a\""), Some(DATE))
    );
    assert_eq!(
        Condition::of(&meta(Some("W/\"a\""), None)),
        condition(Some("W/\"a\""), None)
    );
    assert_eq!(
        Condition::of(&meta(Some(DATE), None)),
        condition(None, Some(DATE))
    );
    assert_eq!(Condition::of(&meta(None, None)), condition(None, None));
    assert!(crate::proxy_cache::is_etag("\"a\"") && crate::proxy_cache::is_etag("W/\"a\""));
    assert!(!crate::proxy_cache::is_etag(DATE));
}

/// Proves: a stale index whose origin says it is unchanged, but whose file has gone from the
/// disk, is filled again from the origin and the ask is answered.
///
/// Catches: a renewal of a file that is not there answered with an error for ever.
#[tokio::test]
async fn a_renewed_index_whose_file_vanished_is_filled_again() {
    let (dir, cache, time) = open("pp2-vanished");
    let script = Script::new([
        Reply::Body(b"release v1", "\"v1\""),
        Reply::Gated(Box::new(Reply::Unchanged)),
        Reply::Body(b"release v1", "\"v1\""),
    ]);
    let origin = origin(&script);
    read(index(&cache, &origin).await.expect("an answer"))
        .await
        .expect("body");
    time.advance(301);
    let asker = {
        let (cache, origin) = (cache.clone(), origin.clone());
        tokio::spawn(async move { index(&cache, &origin).await })
    };
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    fs::remove_file(dir.path().join("objects").join(key().digest())).expect("the body removed");
    script.release(1);
    let again = asker.await.expect("an asker").expect("an answer");
    assert_eq!(again.served, Served::Filled);
    assert_eq!(read(again).await.expect("body"), b"release v1");
    assert_eq!(script.calls(), 3);
}

/// Proves: a renewal of a key that is not stored does nothing, and one whose sidecar cannot be
/// replaced (a directory is where it goes) renews in memory and leaves no temp file behind.
///
/// Catches: a failed rewrite that leaves its scratch file in `tmp/`.
#[tokio::test]
async fn a_renewal_that_cannot_rewrite_its_sidecar_leaves_nothing_behind() {
    let (dir, cache, time) = open("pp2-rewrite");
    let script = Script::new([Reply::Body(b"release v1", "\"v1\"")]);
    let origin = origin(&script);
    read(index(&cache, &origin).await.expect("an answer"))
        .await
        .expect("body");
    let born = cache.shared.state().entries[&key().digest()].born;
    cache.renew(&Key::new("https://example.test/apt", "dists/other"), born);
    time.advance(500);
    let meta = dir
        .path()
        .join("objects")
        .join(format!("{}.meta", key().digest()));
    fs::remove_file(&meta).expect("the sidecar removed");
    fs::create_dir(&meta).expect("a directory in its place");
    cache.renew(&key(), born);
    assert_eq!(cache.peek(&key()).map(|m| m.fetched_at), Some(START + 500));
    assert!(crate::proxy_cache::tests::names(dir.path(), "tmp").is_empty());
}
