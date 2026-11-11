//! The package proxy's cache: what a miss, a hit, a range, a crowd of askers, a failing origin and
//! a failing disk each do. The origin is a fake whose bodies wait for an explicit release (no
//! sleeps), the wait of a waiter runs on tokio's paused clock, and every directory is a scratch.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use futures::stream;
use hostkeeper_enrollment::validity::UnixClock;
use hostkeeper_testkit::Scratch;
use tokio::sync::Semaphore;

use super::{
    Answer, BodyStream, ByteRange, FetchError, Fetched, Key, Limits, Origin, ProxyCache,
    ProxyError, Request, Served,
};

const NOW: u64 = 1_800_000_000;

pub(super) struct Clock(AtomicU64);

impl UnixClock for Clock {
    fn now_unix(&self) -> u64 {
        self.0.fetch_add(1, Ordering::SeqCst)
    }
}

pub(super) fn clock() -> Arc<Clock> {
    Arc::new(Clock(AtomicU64::new(NOW)))
}

pub(super) fn scratch(name: &str) -> Scratch {
    let base = std::env::current_exe()
        .expect("the test binary")
        .parent()
        .expect("its directory")
        .join("hostkeeper-coordinator-unit");
    Scratch::under(&base, name).expect("a scratch directory")
}

pub(super) fn limits() -> Limits {
    Limits::from_settings(&BTreeMap::new())
}

pub(super) fn open(dir: &Path) -> ProxyCache {
    ProxyCache::open(dir, clock(), &limits()).expect("a cache")
}

pub(super) fn key(path: &str) -> Key {
    Key::new("example", path)
}

/// The origin's side of a test: how often it was read, what it was asked, and the release of the
/// bodies that wait.
pub(super) struct Fake {
    calls: AtomicU64,
    asked: std::sync::Mutex<Vec<Request>>,
    gate: Arc<Semaphore>,
}

impl Fake {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicU64::new(0),
            asked: std::sync::Mutex::new(Vec::new()),
            gate: Arc::new(Semaphore::new(0)),
        })
    }

    pub(super) fn calls(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }

    /// Lets `n` waiting bodies go on.
    fn release(&self, n: usize) {
        self.gate.add_permits(n);
    }

    fn requests(&self) -> Vec<Request> {
        self.asked.lock().expect("the log").clone()
    }
}

/// What a read of the fake answers: the `n`th read (from 1) is given to `plan`.
pub(super) fn origin(
    fake: &Arc<Fake>,
    plan: impl Fn(u64) -> Plan + Send + Sync + 'static,
) -> Origin {
    let fake = Arc::clone(fake);
    Arc::new(move |request| {
        let fake = Arc::clone(&fake);
        let n = fake.calls.fetch_add(1, Ordering::SeqCst) + 1;
        fake.asked.lock().expect("the log").push(request);
        let plan = plan(n);
        Box::pin(async move { plan.fetched(&fake.gate) })
    })
}

/// One answer of the fake origin.
#[derive(Clone)]
pub(super) enum Plan {
    /// The headers fail.
    Refuse,
    /// A body of frames, with the length it declares; it waits for a release before the frame
    /// at `wait_at` (or ends there, failing, when `then_fail`).
    Body {
        frames: Vec<Vec<u8>>,
        declared: Option<u64>,
        wait_at: Option<usize>,
        then: Then,
        seen_drop: Option<Arc<AtomicBool>>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Then {
    Go,
    Fail,
    Hang,
}

impl Plan {
    pub(super) fn whole(bytes: &[u8]) -> Self {
        Self::frames(&[bytes])
    }

    pub(super) fn frames(frames: &[&[u8]]) -> Self {
        Self::Body {
            declared: Some(frames.iter().map(|f| f.len() as u64).sum()),
            frames: frames.iter().map(|f| f.to_vec()).collect(),
            wait_at: None,
            then: Then::Go,
            seen_drop: None,
        }
    }

    /// The body waits for a release before its frame `at`.
    fn waiting_at(mut self, at: usize) -> Self {
        if let Self::Body { wait_at, .. } = &mut self {
            *wait_at = Some(at);
        }
        self
    }

    /// After the wait the body fails (or hangs) instead of going on.
    fn then(mut self, how: Then) -> Self {
        if let Self::Body { then, .. } = &mut self {
            *then = how;
        }
        self
    }

    fn declaring(mut self, length: Option<u64>) -> Self {
        if let Self::Body { declared, .. } = &mut self {
            *declared = length;
        }
        self
    }

    fn noticing_drop(mut self, flag: &Arc<AtomicBool>) -> Self {
        if let Self::Body { seen_drop, .. } = &mut self {
            *seen_drop = Some(Arc::clone(flag));
        }
        self
    }

    fn fetched(self, gate: &Arc<Semaphore>) -> Result<Fetched, FetchError> {
        let Self::Body {
            frames,
            declared,
            wait_at,
            then,
            seen_drop,
        } = self
        else {
            return Err(FetchError::Failed("refused".to_owned()));
        };
        let gate = Arc::clone(gate);
        let witness = seen_drop.map(DropFlag);
        let body: BodyStream = Box::pin(stream::unfold(
            (0usize, frames, witness),
            move |(at, frames, witness)| {
                let gate = Arc::clone(&gate);
                async move {
                    if Some(at) == wait_at {
                        gate.acquire().await.expect("the gate").forget();
                        match then {
                            Then::Fail => {
                                return Some((
                                    Err(FetchError::Failed("reset".to_owned())),
                                    (usize::MAX, frames, witness),
                                ));
                            }
                            Then::Hang => std::future::pending::<()>().await,
                            Then::Go => {}
                        }
                    }
                    let frame = frames.get(at)?.clone();
                    Some((Ok(Bytes::from(frame)), (at + 1, frames, witness)))
                }
            },
        ));
        Ok(Fetched {
            validator: Some("\"v1\"".to_owned()),
            last_modified: None,
            unchanged: false,
            slice: None,
            length: declared,
            body,
        })
    }
}

/// Sets its flag when the body that holds it is dropped.
struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

pub(super) async fn read(mut answer: Answer) -> Result<Vec<u8>, FetchError> {
    let mut all = Vec::new();
    while let Some(frame) = answer.body.next().await {
        all.extend_from_slice(&frame?);
    }
    Ok(all)
}

pub(super) async fn get(
    cache: &ProxyCache,
    path: &str,
    origin: &Origin,
) -> Result<Answer, ProxyError> {
    cache.get(&key(path), None, origin, &limits()).await
}

pub(super) fn names(dir: &Path, sub: &str) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir.join(sub))
        .expect("the directory")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

pub(super) fn digest(path: &str) -> String {
    key(path).digest()
}

/// Proves: a miss answers from the origin, whole, and commits the file: the next ask is a hit
/// from the file with the origin's validator and the fetch time, the origin is read once, the
/// body and its sidecar are in `objects/` and `tmp/` is empty.
///
/// Catches: a fill that streams but keeps nothing, or leaves its temp file behind.
#[tokio::test]
async fn a_miss_streams_the_origin_and_the_next_ask_is_a_hit() {
    let dir = scratch("pp1-miss");
    let cache = open(dir.path());
    let fake = Fake::new();
    let origin = origin(&fake, |_| Plan::frames(&[b"hello ", b"world"]));

    let first = get(&cache, "a.deb", &origin).await.expect("an answer");
    assert_eq!(first.served, Served::Filled);
    assert_eq!(first.length, Some(11));
    assert_eq!(first.validator.as_deref(), Some("\"v1\""));
    assert_eq!(read(first).await.expect("the body"), b"hello world");

    let second = get(&cache, "a.deb", &origin).await.expect("an answer");
    assert_eq!(second.served, Served::Hit);
    assert_eq!(second.length, Some(11));
    assert_eq!(second.validator.as_deref(), Some("\"v1\""));
    assert_eq!(second.fetched_at, NOW);
    assert_eq!(second.range, None);
    assert_eq!(read(second).await.expect("the body"), b"hello world");

    assert_eq!(fake.calls(), 1);
    let d = digest("a.deb");
    assert_eq!(
        names(dir.path(), "objects"),
        [d.clone(), format!("{d}.meta")]
    );
    assert!(names(dir.path(), "tmp").is_empty());
    assert_eq!(cache.bytes(), 11);
    let stats = cache.stats();
    assert_eq!((stats.fills, stats.hits, stats.passed), (1, 1, 0));
}

/// Proves: fifty askers of one file at once cause one origin read, and every one of them gets the
/// whole body; the first is streamed the fill and the other forty-nine read the committed file.
///
/// Catches: a stampede of one origin pull per host, which is what the cache is for.
#[tokio::test(start_paused = true)]
async fn fifty_askers_of_one_file_cause_one_origin_fetch() {
    let dir = scratch("pp1-fifty");
    let cache = open(dir.path());
    let fake = Fake::new();
    let origin = origin(&fake, |_| {
        Plan::frames(&[b"one ", b"two ", b"three"]).waiting_at(1)
    });
    let askers: Vec<_> = (0..50)
        .map(|_| {
            let (cache, origin) = (cache.clone(), origin.clone());
            tokio::spawn(async move {
                let answer = get(&cache, "pool/a.deb", &origin).await.expect("an answer");
                let served = answer.served;
                (served, read(answer).await.expect("the body"))
            })
        })
        .collect();
    // Let every asker run until it waits on the fill (the runtime has one thread).
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert_eq!(fake.calls(), 1);
    fake.release(1);
    let mut filled = 0;
    for asker in askers {
        let (served, body) = asker.await.expect("an asker");
        assert_eq!(body, b"one two three");
        filled += usize::from(served == Served::Filled);
    }
    assert_eq!(filled, 1);
    assert_eq!(fake.calls(), 1);
    assert_eq!(cache.stats().hits, 49);
}

/// Proves: when the fill fails, the waiters ask again, the first of them reads the origin once
/// more, and the rest wait for that fill: the three waiters get the file from two origin reads,
/// and the asker whose fill failed is told.
///
/// Catches: a failed fetch that fails every waiter, or one that sends every waiter to the origin.
#[tokio::test(start_paused = true)]
async fn a_failed_fetch_lets_the_next_waiter_fetch() {
    let dir = scratch("pp1-failed");
    let cache = open(dir.path());
    let fake = Fake::new();
    let origin = origin(&fake, |n| {
        if n == 1 {
            Plan::frames(&[b"ab", b"cd"]).waiting_at(1).then(Then::Fail)
        } else {
            Plan::whole(b"abcd")
        }
    });
    let leader = get(&cache, "f", &origin).await.expect("an answer");
    let waiters: Vec<_> = (0..3)
        .map(|_| {
            let (cache, origin) = (cache.clone(), origin.clone());
            tokio::spawn(async move {
                let answer = get(&cache, "f", &origin).await.expect("an answer");
                read(answer).await.expect("the body")
            })
        })
        .collect();
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    fake.release(1);
    assert_eq!(
        read(leader).await,
        Err(FetchError::Failed("reset".to_owned()))
    );
    for waiter in waiters {
        assert_eq!(waiter.await.expect("a waiter"), b"abcd");
    }
    assert_eq!(fake.calls(), 2);
    assert!(names(dir.path(), "tmp").is_empty());
}

/// Proves: an origin that fails before its body leaves no flight behind: the asker is told, and
/// the next ask reads the origin again and is served.
///
/// Catches: a key stuck as "being fetched" after one refused read.
#[tokio::test]
async fn an_origin_that_refuses_is_asked_again_by_the_next() {
    let dir = scratch("pp1-refused");
    let cache = open(dir.path());
    let fake = Fake::new();
    let origin = origin(&fake, |n| {
        if n == 1 {
            Plan::Refuse
        } else {
            Plan::whole(b"ok")
        }
    });
    assert!(matches!(
        get(&cache, "r", &origin).await,
        Err(ProxyError::Origin(FetchError::Failed(_)))
    ));
    let answer = get(&cache, "r", &origin).await.expect("an answer");
    assert_eq!(read(answer).await.expect("the body"), b"ok");
    assert_eq!(fake.calls(), 2);
}

/// Proves: fifty askers of a fill that never finishes are each passed through to the origin on
/// their own after `proxy_wait_s` on the paused clock, get the whole body, and store nothing; the
/// fill is left running and nothing is committed.
///
/// Catches: one slow fill that stalls every host for as long as it lasts.
#[tokio::test(start_paused = true)]
async fn waiters_of_a_fill_that_never_finishes_are_passed_through() {
    let dir = scratch("pp1-hang");
    let cache = open(dir.path());
    let fake = Fake::new();
    let origin = origin(&fake, |n| {
        if n == 1 {
            Plan::frames(&[b"xy", b"z"]).waiting_at(1).then(Then::Hang)
        } else {
            Plan::whole(b"xyz")
        }
    });
    let wide = Limits {
        fetches: 100,
        wait: Duration::from_secs(10),
        ..limits()
    };
    let leader = cache
        .get(&key("slow"), None, &origin, &wide)
        .await
        .expect("an answer");
    let askers: Vec<_> = (0..50)
        .map(|_| {
            let (cache, origin, wide) = (cache.clone(), origin.clone(), wide.clone());
            tokio::spawn(async move {
                let answer = cache
                    .get(&key("slow"), None, &origin, &wide)
                    .await
                    .expect("an answer");
                let served = answer.served;
                (served, read(answer).await.expect("the body"))
            })
        })
        .collect();
    for asker in askers {
        let (served, body) = asker.await.expect("an asker");
        assert_eq!(served, Served::Passed);
        assert_eq!(body, b"xyz");
    }
    assert_eq!(fake.calls(), 51);
    assert_eq!(cache.stats().passed, 50);
    assert!(names(dir.path(), "objects").is_empty());
    drop(leader);
}

/// Proves: no more than `proxy_fetches` reads are in flight, a pass-through included: with one
/// allowed, a second key is refused Busy while the first fill runs, a waiter that would pass
/// through is refused Busy too, and the place is free again when the fill ends.
///
/// Catches: a cache that lets a slow origin be opened by every asker.
#[tokio::test(start_paused = true)]
async fn the_reads_in_flight_are_capped_by_proxy_fetches() {
    let dir = scratch("pp1-busy");
    let cache = open(dir.path());
    let fake = Fake::new();
    let origin = origin(&fake, |_| Plan::frames(&[b"a", b"b"]).waiting_at(1));
    let one = Limits {
        fetches: 1,
        ..limits()
    };
    let leader = cache
        .get(&key("one"), None, &origin, &one)
        .await
        .expect("an answer");
    assert!(matches!(
        cache.get(&key("two"), None, &origin, &one).await,
        Err(ProxyError::Busy)
    ));
    // A waiter of the first key, whose wait ends with the fill still going, finds no place.
    assert!(matches!(
        cache.get(&key("one"), None, &origin, &one).await,
        Err(ProxyError::Busy)
    ));
    assert_eq!(fake.calls(), 1);
    fake.release(1);
    assert_eq!(read(leader).await.expect("the body"), b"ab");
    // The leader's body ended; the fill has finished and given its place back.
    let again = cache
        .get(&key("two"), None, &origin, &one)
        .await
        .expect("an answer");
    fake.release(1);
    assert_eq!(read(again).await.expect("the body"), b"ab");
}

/// Proves: a leader refused Busy leaves no flight behind: the same key can be asked once a place
/// is free.
///
/// Catches: a key stuck as "being fetched" after a refusal for want of a place.
#[tokio::test(start_paused = true)]
async fn a_refusal_for_want_of_a_place_does_not_hold_the_key() {
    let dir = scratch("pp1-busy-key");
    let cache = open(dir.path());
    let fake = Fake::new();
    let origin = origin(&fake, |_| Plan::frames(&[b"a", b"b"]).waiting_at(1));
    let none = Limits {
        fetches: 1,
        ..limits()
    };
    let held = cache
        .get(&key("held"), None, &origin, &none)
        .await
        .expect("an answer");
    assert!(matches!(
        cache.get(&key("other"), None, &origin, &none).await,
        Err(ProxyError::Busy)
    ));
    fake.release(2);
    assert_eq!(read(held).await.expect("the body"), b"ab");
    let other = cache
        .get(&key("other"), None, &origin, &none)
        .await
        .expect("an answer");
    assert_eq!(other.served, Served::Filled);
    assert_eq!(read(other).await.expect("the body"), b"ab");
}

/// A writer that fails at its first write, or at its flush.
struct Failing {
    flush: bool,
    _file: fs::File,
}

impl Write for Failing {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.flush {
            Ok(buf.len())
        } else {
            Err(io::Error::other("the disk is full"))
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("the disk is full"))
    }
}

fn open_failing(dir: &Path, flush: bool) -> ProxyCache {
    ProxyCache::open_with(
        dir,
        clock(),
        &limits(),
        Box::new(move |path| {
            Ok(Box::new(Failing {
                flush,
                _file: fs::File::create_new(path)?,
            }))
        }),
    )
    .expect("a cache")
}

/// Proves: a write that fails still streams the whole answer to the asker, stores nothing, leaves
/// no temp file, and is counted; a waiter of that fill is passed through at once (not made to
/// fetch one after another), and the next ask fills again.
///
/// Catches: a full disk that fails the download of every host.
#[tokio::test(start_paused = true)]
async fn a_disk_write_failure_still_streams_the_answer() {
    let dir = scratch("pp1-write-fail");
    let cache = open_failing(dir.path(), false);
    let fake = Fake::new();
    let origin = origin(&fake, |_| Plan::frames(&[b"abc", b"def"]).waiting_at(1));
    let leader = get(&cache, "w", &origin).await.expect("an answer");
    let waiter = {
        let (cache, origin) = (cache.clone(), origin.clone());
        tokio::spawn(async move {
            let answer = get(&cache, "w", &origin).await.expect("an answer");
            let served = answer.served;
            (served, read(answer).await.expect("the body"))
        })
    };
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    // One release for the leader's body, one for the waiter's own read once it is passed through.
    fake.release(2);
    assert_eq!(read(leader).await.expect("the body"), b"abcdef");
    assert_eq!(
        waiter.await.expect("a waiter"),
        (Served::Passed, b"abcdef".to_vec())
    );
    assert!(names(dir.path(), "objects").is_empty());
    assert!(names(dir.path(), "tmp").is_empty());
    assert_eq!(cache.stats().store_failures, 1);
    assert_eq!(fake.calls(), 2);
}

/// Proves: a temp file that cannot be made, and a flush or a commit that fails, each leave the
/// answer whole and the cache empty, with no file of the fill left in `tmp/` or `objects/`.
///
/// Catches: a failing commit that serves a half-stored object later, or leaks the temp file.
#[tokio::test]
async fn a_temp_file_flush_or_commit_that_fails_stores_nothing() {
    let fake = Fake::new();
    let plan = origin(&fake, |_| Plan::frames(&[b"abc", b"def"]));

    // No temp file.
    let dir = scratch("pp1-no-tmp");
    let cache = ProxyCache::open_with(
        dir.path(),
        clock(),
        &limits(),
        Box::new(|_| Err(io::Error::other("no space"))),
    )
    .expect("a cache");
    let answer = get(&cache, "t", &plan).await.expect("an answer");
    assert_eq!(read(answer).await.expect("the body"), b"abcdef");
    assert!(names(dir.path(), "objects").is_empty());

    // No temp file, and the origin fails midway: the asker is told.
    let broken = origin(&fake, |_| {
        Plan::frames(&[b"abc", b"def"])
            .waiting_at(1)
            .then(Then::Fail)
    });
    fake.release(1);
    let answer = get(&cache, "t2", &broken).await.expect("an answer");
    assert_eq!(
        read(answer).await,
        Err(FetchError::Failed("reset".to_owned()))
    );

    // The flush fails.
    let dir = scratch("pp1-flush");
    let cache = open_failing(dir.path(), true);
    let answer = get(&cache, "t", &plan).await.expect("an answer");
    assert_eq!(read(answer).await.expect("the body"), b"abcdef");
    // The fill ends after its asker has the body: the next ask waits for it, then fills again.
    let again = get(&cache, "t", &plan).await.expect("an answer");
    assert_eq!(read(again).await.expect("the body"), b"abcdef");
    assert!(names(dir.path(), "objects").is_empty());
    assert!(names(dir.path(), "tmp").is_empty());
    assert_eq!(cache.stats().store_failures, 2);

    // The directory the sidecar goes to is gone.
    let dir = scratch("pp1-no-objects");
    let cache = open(dir.path());
    fs::remove_dir_all(dir.path().join("objects")).expect("removed");
    let answer = get(&cache, "t", &plan).await.expect("an answer");
    assert_eq!(read(answer).await.expect("the body"), b"abcdef");
    assert!(names(dir.path(), "tmp").is_empty());
    assert_eq!(cache.stats().store_failures, 1);

    // The body cannot take its place: a directory with a file in it is there.
    let dir = scratch("pp1-body-place");
    let cache = open(dir.path());
    let place = dir.path().join("objects").join(digest("t"));
    fs::create_dir(&place).expect("a directory");
    fs::write(place.join("x"), b"x").expect("a file");
    let answer = get(&cache, "t", &plan).await.expect("an answer");
    assert_eq!(read(answer).await.expect("the body"), b"abcdef");
    assert!(names(dir.path(), "tmp").is_empty());
    assert_eq!(cache.stats().store_failures, 1);
    assert!(
        !dir.path()
            .join("objects")
            .join(format!("{}.meta", digest("t")))
            .exists()
    );
}

/// Proves: a range of an object that is not committed is passed through with the range asked of
/// the origin and stores nothing; a full ask then fills the cache, and a range of the committed
/// object is read from the file: the bytes asked, clamped at the end, with the first and last
/// byte named, a range from the end or past it refused with the length, and so is a range whose
/// end is before its start.
///
/// Catches: a partial body committed as the whole object, or a range answered from the origin
/// again once the file is there.
#[tokio::test]
async fn a_range_miss_stores_nothing_and_a_range_of_a_hit_is_read_from_the_file() {
    let dir = scratch("pp1-range");
    let cache = open(dir.path());
    let fake = Fake::new();
    let origin = origin(&fake, |_| Plan::whole(b"0123456789").declaring(None));
    let range = |start, end| Some(ByteRange { start, end });

    let passed = cache
        .get(&key("r"), range(2, Some(4)), &origin, &limits())
        .await
        .expect("an answer");
    assert_eq!(passed.served, Served::Passed);
    assert_eq!(passed.length, None);
    assert_eq!(read(passed).await.expect("the body"), b"0123456789");
    assert_eq!(fake.requests()[0].range, range(2, Some(4)));
    assert!(names(dir.path(), "objects").is_empty());

    let whole = get(&cache, "r", &origin).await.expect("an answer");
    assert_eq!(read(whole).await.expect("the body"), b"0123456789");
    assert_eq!(fake.calls(), 2);

    let mid = cache
        .get(&key("r"), range(3, Some(5)), &origin, &limits())
        .await
        .expect("an answer");
    assert_eq!(
        (mid.served, mid.range, mid.length),
        (Served::Hit, Some((3, 5)), Some(10))
    );
    assert_eq!(read(mid).await.expect("the body"), b"345");
    let tail = cache
        .get(&key("r"), range(7, None), &origin, &limits())
        .await
        .expect("an answer");
    assert_eq!(tail.range, Some((7, 9)));
    assert_eq!(read(tail).await.expect("the body"), b"789");
    let clamped = cache
        .get(&key("r"), range(8, Some(99)), &origin, &limits())
        .await
        .expect("an answer");
    assert_eq!(clamped.range, Some((8, 9)));
    assert_eq!(read(clamped).await.expect("the body"), b"89");
    for bad in [range(10, None), range(99, Some(100)), range(5, Some(4))] {
        assert!(matches!(
            cache.get(&key("r"), bad, &origin, &limits()).await,
            Err(ProxyError::Unsatisfiable(10))
        ));
    }
    assert_eq!(fake.calls(), 2);
}

/// Proves: an empty object is cached and read back as an empty body, and no range of it is
/// satisfiable.
///
/// Catches: a length-zero file that underflows a range or never ends.
#[tokio::test]
async fn an_empty_object_is_a_hit_with_no_body() {
    let dir = scratch("pp1-empty");
    let cache = open(dir.path());
    let fake = Fake::new();
    let origin = origin(&fake, |_| Plan::frames(&[]));
    let first = get(&cache, "e", &origin).await.expect("an answer");
    assert_eq!(read(first).await.expect("the body"), b"");
    let second = get(&cache, "e", &origin).await.expect("an answer");
    assert_eq!((second.served, second.length), (Served::Hit, Some(0)));
    assert_eq!(read(second).await.expect("the body"), b"");
    let range = Some(ByteRange {
        start: 0,
        end: None,
    });
    assert!(matches!(
        cache.get(&key("e"), range, &origin, &limits()).await,
        Err(ProxyError::Unsatisfiable(0))
    ));
}

/// Proves: when a commit would pass `proxy_cache_max_bytes`, the least recently used committed
/// object is deleted (the one not read since, not the oldest written), with its sidecar, and the
/// rest stay; a fill still in progress keeps its temp file.
///
/// Catches: first-in-first-out eviction, or eviction that deletes a file being written.
#[tokio::test]
async fn a_full_cache_evicts_the_least_recently_used_committed_object_only() {
    let dir = scratch("pp1-lru");
    let cache = open(dir.path());
    let fake = Fake::new();
    let origin = origin(&fake, |_| Plan::whole(b"1234"));
    let small = Limits {
        max_bytes: 10,
        ..limits()
    };
    let ask = |path: &'static str| {
        let (cache, origin, small) = (cache.clone(), origin.clone(), small.clone());
        async move {
            let answer = cache
                .get(&key(path), None, &origin, &small)
                .await
                .expect("an answer");
            read(answer).await.expect("the body")
        }
    };
    ask("a").await;
    ask("b").await;
    assert_eq!(cache.bytes(), 8);
    // `a` is read again, so `b` is the least recently used.
    ask("a").await;
    assert_eq!(fake.calls(), 2);

    // A fill in progress: its temp file is not a candidate.
    let slow = Fake::new();
    let slow_origin = self::origin(&slow, |_| Plan::frames(&[b"12", b"34"]).waiting_at(1));
    let pending = cache
        .get(&key("d"), None, &slow_origin, &small)
        .await
        .expect("an answer");
    assert_eq!(names(dir.path(), "tmp").len(), 1);

    ask("c").await;
    assert_eq!(cache.bytes(), 8);
    assert_eq!(cache.stats().evictions, 1);
    let kept: Vec<String> = ["a", "c"]
        .iter()
        .flat_map(|p| [digest(p), format!("{}.meta", digest(p))])
        .collect();
    let mut kept = kept;
    kept.sort();
    assert_eq!(names(dir.path(), "objects"), kept);
    assert_eq!(names(dir.path(), "tmp").len(), 1);

    // The fill in progress commits and takes the place of the next least recently used: `a`.
    slow.release(1);
    assert_eq!(read(pending).await.expect("the body"), b"1234");
    assert_eq!(cache.bytes(), 8);
    assert!(!dir.path().join("objects").join(digest("a")).exists());
    assert!(dir.path().join("objects").join(digest("d")).exists());
}

/// Proves: an object whose declared length is over `proxy_cache_max_bytes`, and one that grows
/// past it with no length declared, are streamed whole and not stored, and evict nothing.
///
/// Catches: one large file that empties the cache, or a cache that holds more than its limit.
#[tokio::test]
async fn an_object_larger_than_the_cache_is_streamed_and_not_stored() {
    let dir = scratch("pp1-large");
    let cache = open(dir.path());
    let fake = Fake::new();
    let small = Limits {
        max_bytes: 6,
        ..limits()
    };
    let keeps = origin(&fake, |_| Plan::whole(b"abc"));
    let kept = cache
        .get(&key("keep"), None, &keeps, &small)
        .await
        .expect("an answer");
    read(kept).await.expect("the body");

    let declared = origin(&fake, |_| Plan::whole(b"0123456789"));
    let answer = cache
        .get(&key("big"), None, &declared, &small)
        .await
        .expect("an answer");
    assert_eq!(read(answer).await.expect("the body"), b"0123456789");
    let undeclared = origin(&fake, |_| {
        Plan::frames(&[b"012", b"345", b"6789"]).declaring(None)
    });
    let answer = cache
        .get(&key("grows"), None, &undeclared, &small)
        .await
        .expect("an answer");
    assert_eq!(read(answer).await.expect("the body"), b"0123456789");
    assert_eq!(cache.stats().evictions, 0);
    assert_eq!(cache.stats().store_failures, 2);
    assert!(dir.path().join("objects").join(digest("keep")).exists());
    assert!(!dir.path().join("objects").join(digest("big")).exists());
    assert!(!dir.path().join("objects").join(digest("grows")).exists());
}

/// Proves: a body that ends short of the length the origin declared is not committed: the asker
/// is told how many bytes came of how many, nothing is stored, and the next ask reads the origin
/// again.
///
/// Catches: a truncated file cached as the object.
#[tokio::test]
async fn a_body_that_ends_short_is_not_committed() {
    let dir = scratch("pp1-short");
    let cache = open(dir.path());
    let fake = Fake::new();
    let origin = origin(&fake, |n| {
        if n == 1 {
            Plan::whole(b"1234").declaring(Some(10))
        } else {
            Plan::whole(b"0123456789")
        }
    });
    let first = get(&cache, "s", &origin).await.expect("an answer");
    assert_eq!(
        read(first).await,
        Err(FetchError::Short { got: 4, want: 10 })
    );
    assert!(names(dir.path(), "objects").is_empty());
    assert!(names(dir.path(), "tmp").is_empty());
    let second = get(&cache, "s", &origin).await.expect("an answer");
    assert_eq!(second.served, Served::Filled);
    assert_eq!(read(second).await.expect("the body"), b"0123456789");
    assert_eq!(fake.calls(), 2);
}

/// Proves: an asker that goes away does not stop the fill: the file is committed and the next ask
/// is a hit, with one origin read.
///
/// Catches: a client that disconnects mid-download and leaves the cache cold for the next.
#[tokio::test(start_paused = true)]
async fn the_fill_continues_after_its_asker_has_gone() {
    let dir = scratch("pp1-gone");
    let cache = open(dir.path());
    let fake = Fake::new();
    let origin = origin(&fake, |_| {
        Plan::frames(&[b"ab", b"cd", b"ef"]).waiting_at(1)
    });
    let leader = get(&cache, "g", &origin).await.expect("an answer");
    drop(leader);
    fake.release(1);
    let hit = get(&cache, "g", &origin).await.expect("an answer");
    assert_eq!(read(hit).await.expect("the body"), b"abcdef");
    assert_eq!(fake.calls(), 1);
    assert_eq!(cache.stats().hits, 1);
}

/// Proves: a fill that nobody wants (its asker is gone and nothing is being stored) stops reading
/// the origin, drops the body, and lets a waiter read on its own.
///
/// Catches: a fill that pulls a whole large file for no one.
#[tokio::test(start_paused = true)]
async fn a_fill_nobody_wants_stops_reading() {
    let dir = scratch("pp1-unwanted");
    let cache = ProxyCache::open_with(
        dir.path(),
        clock(),
        &limits(),
        Box::new(|_| Err(io::Error::other("no space"))),
    )
    .expect("a cache");
    let fake = Fake::new();
    let dropped = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&dropped);
    let origin = origin(&fake, move |n| {
        if n == 1 {
            Plan::frames(&[b"ab", b"cd", b"ef"])
                .waiting_at(0)
                .noticing_drop(&seen)
        } else {
            Plan::whole(b"abcdef")
        }
    });
    let leader = get(&cache, "u", &origin).await.expect("an answer");
    let waiter = {
        let (cache, origin) = (cache.clone(), origin.clone());
        tokio::spawn(async move {
            let answer = get(&cache, "u", &origin).await.expect("an answer");
            let served = answer.served;
            (served, read(answer).await.expect("the body"))
        })
    };
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    drop(leader);
    fake.release(1);
    assert_eq!(
        waiter.await.expect("a waiter"),
        (Served::Passed, b"abcdef".to_vec())
    );
    assert!(dropped.load(Ordering::SeqCst));
}

/// Proves: when the directory is wiped under a running cache, the next ask forgets the object,
/// reads the origin and fills it again; and a cached file that is short, or cannot be read, ends
/// its body with an error and not with a short body that looks whole.
///
/// Catches: a cache that serves a vanished file forever, or a truncated one as complete.
#[tokio::test]
async fn a_file_that_is_gone_or_damaged_is_refilled_or_fails_the_read() {
    let dir = scratch("pp1-wiped");
    let cache = open(dir.path());
    let fake = Fake::new();
    let origin = origin(&fake, |_| Plan::whole(b"0123456789"));
    let first = get(&cache, "w", &origin).await.expect("an answer");
    read(first).await.expect("the body");
    let body = dir.path().join("objects").join(digest("w"));

    fs::remove_file(&body).expect("wiped");
    let second = get(&cache, "w", &origin).await.expect("an answer");
    assert_eq!(second.served, Served::Filled);
    read(second).await.expect("the body");
    assert_eq!(fake.calls(), 2);
    assert_eq!(cache.bytes(), 10);

    fs::write(&body, b"0123").expect("shortened");
    let short = get(&cache, "w", &origin).await.expect("an answer");
    assert_eq!(short.served, Served::Hit);
    assert_eq!(read(short).await, Err(FetchError::Cache));

    fs::remove_file(&body).expect("removed");
    fs::create_dir(&body).expect("a directory");
    let unreadable = get(&cache, "w", &origin).await.expect("an answer");
    assert_eq!(read(unreadable).await, Err(FetchError::Cache));
}
