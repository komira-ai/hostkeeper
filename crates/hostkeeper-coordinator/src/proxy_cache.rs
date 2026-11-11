//! The package proxy's cache (`docs/design/package-proxy.md`, section 4): one origin fetch per
//! key however many ask, a fill streamed to its first asker while it is written, a commit that is
//! one rename, and least-recently-used eviction of committed objects only.
//!
//! The cache knows no HTTP and no origin: the caller gives each call an [`Origin`], a function
//! that starts one read, and the [`Limits`] the settings (`proxy_*`, [`crate::mdm_settings`]) hold
//! at that moment. What each call does:
//!
//! - **Hit.** A committed object is read from its file, whole or as a [`ByteRange`].
//! - **Miss.** The first asker leads: its read starts and the body is streamed to it while a task
//!   writes it to `tmp/` and commits it when it ends whole, even if the asker has gone or has
//!   fallen behind (it is cut with [`FetchError::Behind`], and the fill goes on). A second
//!   asker of the same key waits for that fill at most [`Limits::wait`], then reads the file, or,
//!   when the wait ends first or the fill could not be stored, is passed through to the origin
//!   on its own, uncached. When the fill failed, the waiters ask again and the first of them leads.
//! - **Range miss.** A range of an object that is not committed is passed through and stores
//!   nothing: a partial body is never committed.
//! - **Disk trouble.** A temp file that cannot be made, a write that fails or a commit that fails
//!   leaves the answer streaming and stores nothing ([`Stats::store_failures`]).
//! - **Origin trouble.** A body that ends short of its declared length or fails is not committed.
//!
//! At most [`Limits::fetches`] reads are in flight, fills and pass-throughs together; the next
//! asker gets [`ProxyError::Busy`]. Nothing is held whole in memory and no size is capped: a body
//! is written frame by frame, and an object larger than [`Limits::max_bytes`] is not stored. The
//! writes of a fill are blocking writes of one frame to a local file inside the task: the cache
//! is fed by one origin read at a time per key and a LAN's worth of askers, and a task that
//! awaited a thread pool for each frame would let a test clock run ahead of the disk.
//!
//! Single node: one coordinator owns the directory. Nothing is synced (see
//! [`crate::proxy_cache_disk`]).

use std::collections::HashMap;
use std::fs::File;
use std::future::Future;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use bytes::Bytes;
use futures::stream::{self, Stream, StreamExt};
use hostkeeper_enrollment::validity::{SystemClock, UnixClock};
use tokio::sync::{mpsc, watch};

use crate::config::StartError;
use crate::mdm_settings::whole_of;
pub use crate::proxy_cache_disk::{Key, Meta};
use crate::proxy_cache_disk::{Layout, scan};

/// The bytes of a hit read from its file at a time.
const CHUNK: u64 = 64 * 1024;

/// The frames a fill may be ahead of the asker it streams to.
const AHEAD: usize = 8;

/// A body, frame by frame.
pub type BodyStream = Pin<Box<dyn Stream<Item = Result<Bytes, FetchError>> + Send>>;

/// A read of the origin, started by [`Origin`].
pub type Fetching = Pin<Box<dyn Future<Output = Result<Fetched, FetchError>> + Send>>;

/// Starts one read of the origin for a [`Request`].
pub type Origin = Arc<dyn Fn(Request) -> Fetching + Send + Sync>;

/// Makes the temp file of a fill. The cache's own is [`File::create_new`]; a test gives one that
/// fails.
pub(crate) type OpenTmp = dyn Fn(&Path) -> std::io::Result<Box<dyn Write + Send>> + Send + Sync;

/// Why a read of the origin, or of a cached file, failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FetchError {
    /// The origin could not be read, or its body failed; the text says how and holds no secret.
    #[error("the origin failed: {0}")]
    Failed(String),
    /// The body ended before the length the origin declared.
    #[error("the origin gave {got} bytes of {want}")]
    Short {
        /// The bytes that came.
        got: u64,
        /// The bytes the origin declared.
        want: u64,
    },
    /// The origin has no such file (404).
    #[error("the origin has no such file")]
    NotFound,
    /// A cached file could not be read to its end.
    #[error("a cached file could not be read")]
    Cache,
    /// The asker of a fill read too slowly: the fill went on to the disk without it, and the file
    /// is a hit for its next ask.
    #[error("the reader fell behind the fill")]
    Behind,
}

/// Why the cache gave no answer.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ProxyError {
    /// [`Limits::fetches`] reads are in flight.
    #[error("the proxy is fetching as many files as it may")]
    Busy,
    /// The read of the origin failed before its body began.
    #[error(transparent)]
    Origin(#[from] FetchError),
    /// The range starts at or past the end of the object, whose length is given.
    #[error("the range starts past the end of an object of {0} bytes")]
    Unsatisfiable(u64),
}

/// A range of bytes: from `start` to `end` inclusive, or to the end of the object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ByteRange {
    /// The first byte.
    pub start: u64,
    /// The last byte, or none for the end.
    pub end: Option<u64>,
}

/// The validators of a stored object, sent to its origin to ask whether it changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Condition {
    /// The object's `ETag`: `If-None-Match`.
    pub etag: Option<String>,
    /// The object's `Last-Modified`: `If-Modified-Since`.
    pub last_modified: Option<String>,
}

impl Condition {
    /// The validators `meta` holds: its validator is an `ETag` when it is a quoted string (or a
    /// weak one), else a date.
    #[must_use]
    pub fn of(meta: &Meta) -> Self {
        let etag = meta.validator.clone().filter(|v| is_etag(v));
        let last_modified = meta
            .last_modified
            .clone()
            .or_else(|| meta.validator.clone().filter(|_| etag.is_none()));
        Self {
            etag,
            last_modified,
        }
    }
}

/// Whether a validator is an entity tag (a quoted string, or a weak one) and not a date.
#[must_use]
pub fn is_etag(validator: &str) -> bool {
    validator.starts_with('"') || validator.starts_with("W/\"")
}

/// One read of the origin that the cache asks [`Origin`] to start.
#[derive(Clone, Debug)]
pub struct Request {
    /// What to read.
    pub key: Key,
    /// The range to read, for a pass-through of a range; a fill reads the whole.
    pub range: Option<ByteRange>,
    /// For the revalidation of a stored object: its validators. The read is then conditional, and
    /// an origin that says the object is unchanged answers [`Fetched::unchanged`].
    pub condition: Option<Condition>,
}

/// The part of an object a `206` body is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slice {
    /// The first byte of the body in the object.
    pub start: u64,
    /// The last byte of the body in the object.
    pub end: u64,
    /// The length of the whole object, when the origin said it.
    pub total: Option<u64>,
}

/// What the origin answered: its validator and length, and the body.
pub struct Fetched {
    /// The origin's validator for the object.
    pub validator: Option<String>,
    /// The origin's `Last-Modified`, when it gave one.
    pub last_modified: Option<String>,
    /// The origin answered a conditional read with 304: the stored object is current, and the body
    /// is empty.
    pub unchanged: bool,
    /// The part of the object the body is, when the origin answered a range with 206; none for the
    /// whole object.
    pub slice: Option<Slice>,
    /// The length the origin declared for the body, if it did.
    pub length: Option<u64>,
    /// The body.
    pub body: BodyStream,
}

/// The settings the cache reads (`proxy_*`), taken at each call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    /// `proxy_wait_s`: how long a waiter waits for a fill.
    pub wait: Duration,
    /// `proxy_redirects`: how many redirects an origin client follows.
    pub redirects: u64,
    /// `proxy_index_ttl_s`: how long an index is served without asking the origin.
    pub index_ttl: Duration,
    /// `proxy_cache_max_bytes`: the most bytes of committed objects.
    pub max_bytes: u64,
    /// `proxy_fetches`: the most reads in flight.
    pub fetches: u64,
}

impl Limits {
    /// The limits of the settings that were `set` (their defaults for the rest, and for a value
    /// that is not a number).
    #[must_use]
    pub fn from_settings(set: &std::collections::BTreeMap<String, String>) -> Self {
        Self {
            wait: Duration::from_secs(whole_of(set, "proxy_wait_s", 10)),
            redirects: whole_of(set, "proxy_redirects", 3),
            index_ttl: Duration::from_secs(whole_of(set, "proxy_index_ttl_s", 300)),
            max_bytes: whole_of(set, "proxy_cache_max_bytes", 20 << 30),
            fetches: whole_of(set, "proxy_fetches", 8),
        }
    }
}

/// How an answer was made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Served {
    /// From a committed file.
    Hit,
    /// From the origin, written to the cache as it streamed.
    Filled,
    /// From the origin, stored nowhere.
    Passed,
}

/// What the cache answers.
pub struct Answer {
    /// How the answer was made.
    pub served: Served,
    /// The origin's validator for the object.
    pub validator: Option<String>,
    /// The origin's `Last-Modified`, when it gave one.
    pub last_modified: Option<String>,
    /// For a hit and for a range, the length of the whole object (none when the origin did not
    /// say); otherwise the length the origin declared for the body, if it did.
    pub length: Option<u64>,
    /// When the object was fetched, in unix seconds; for an answer that is not a hit, now.
    pub fetched_at: u64,
    /// For a range, the first and the last byte of the body in the object.
    pub range: Option<(u64, u64)>,
    /// The body.
    pub body: BodyStream,
}

/// What the cache has done since it opened.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Answers from a file.
    pub hits: u64,
    /// Fills started.
    pub fills: u64,
    /// Answers passed through to the origin, stored nowhere.
    pub passed: u64,
    /// Fills that streamed but stored nothing because the disk failed or the object did not fit.
    pub store_failures: u64,
    /// Objects deleted to make room.
    pub evictions: u64,
}

/// What a flight of one key is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Done {
    Pending,
    Committed,
    /// The fill ended whole and was not stored.
    Uncached,
    Failed,
}

struct Entry {
    meta: Meta,
    used: u64,
    /// The tick of its commit: no two commits of a key share one, so a reader that holds a
    /// stale one cannot mistake a newer object for the one it read.
    born: u64,
}

#[derive(Default)]
struct State {
    entries: HashMap<String, Entry>,
    total: u64,
    tick: u64,
    flights: HashMap<String, watch::Sender<Done>>,
    active: u64,
    stats: Stats,
}

struct Shared {
    layout: Layout,
    clock: Arc<dyn UnixClock>,
    open_tmp: Box<OpenTmp>,
    counter: AtomicU64,
    state: Mutex<State>,
}

/// The cache: a handle to the directory, cheap to clone.
#[derive(Clone)]
pub struct ProxyCache {
    shared: Arc<Shared>,
}

/// A place in the in-flight reads, given back when it drops.
pub(crate) struct Slot(Arc<Shared>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.state().active -= 1;
    }
}

/// The leadership of one key's fill: ends the flight when it drops, as failed unless it was
/// finished with an outcome.
struct Flight {
    shared: Arc<Shared>,
    digest: String,
    finished: bool,
}

impl Flight {
    fn finish(mut self, done: Done) {
        self.end(done);
    }

    fn end(&mut self, done: Done) {
        self.finished = true;
        // Out of the map first, so that a waiter woken by the outcome that asks again finds no
        // flight of this key and leads.
        let flight = self.shared.state().flights.remove(&self.digest);
        flight.into_iter().for_each(|flight| {
            flight.send_replace(done);
        });
    }
}

impl Drop for Flight {
    fn drop(&mut self) {
        if !self.finished {
            self.end(Done::Failed);
        }
    }
}

enum Claim {
    /// The key was committed while its asker was on the way: ask again, it is a hit.
    Cached,
    Lead(Flight),
    Wait(watch::Receiver<Done>),
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl ProxyCache {
    /// Opens the cache in `dir`, which is made when missing: the files a crash left are deleted
    /// and the committed objects are counted, and the least recently fetched are evicted while
    /// they pass `limits.max_bytes`.
    ///
    /// # Errors
    /// The directory cannot be made or read.
    pub fn open(dir: &Path, clock: Arc<dyn UnixClock>, limits: &Limits) -> std::io::Result<Self> {
        Self::open_with(
            dir,
            clock,
            limits,
            Box::new(|path| Ok(Box::new(File::create_new(path)?))),
        )
    }

    pub(crate) fn open_with(
        dir: &Path,
        clock: Arc<dyn UnixClock>,
        limits: &Limits,
        open_tmp: Box<OpenTmp>,
    ) -> std::io::Result<Self> {
        let layout = Layout::create(dir)?;
        let mut state = State::default();
        for found in scan(&layout)? {
            state.total += found.meta.length;
            state.tick += 1;
            state.entries.insert(
                found.digest,
                Entry {
                    meta: found.meta,
                    used: state.tick,
                    born: state.tick,
                },
            );
        }
        let shared = Arc::new(Shared {
            layout,
            clock,
            open_tmp,
            counter: AtomicU64::new(0),
            state: Mutex::new(state),
        });
        evict_down_to(&shared, &mut shared.state(), limits.max_bytes);
        Ok(Self { shared })
    }

    /// What the cache has done since it opened.
    #[must_use]
    pub fn stats(&self) -> Stats {
        self.shared.state().stats
    }

    /// The bytes of the committed objects.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.shared.state().total
    }

    /// The answer for `key`, or for `range` of it: see the module's text.
    ///
    /// # Errors
    /// [`ProxyError`]: the proxy is busy, the origin failed before its body began, or the range is
    /// past the end of a committed object.
    pub async fn get(
        &self,
        key: &Key,
        range: Option<ByteRange>,
        origin: &Origin,
        limits: &Limits,
    ) -> Result<Answer, ProxyError> {
        let digest = key.digest();
        loop {
            if let Some(answer) = self.hit(&digest, range)? {
                return Ok(answer);
            }
            if range.is_some() {
                return self.pass(key, range, origin, limits).await;
            }
            match self.claim(&digest) {
                Claim::Cached => {}
                Claim::Lead(flight) => return self.lead(key, flight, origin, limits).await,
                Claim::Wait(mut done) => {
                    let ended = tokio::time::timeout(
                        limits.wait,
                        done.wait_for(|done| *done != Done::Pending),
                    )
                    .await;
                    // A fill that ended and could not be stored is read by each waiter on its
                    // own; one that was committed is a hit on the next turn, and one that failed
                    // lets the first waiter to ask again lead.
                    let pass = ended.map_or(true, |ended| {
                        ended.is_ok_and(|done| *done == Done::Uncached)
                    });
                    if pass {
                        return self.pass(key, range, origin, limits).await;
                    }
                }
            }
        }
    }

    /// The committed object `digest`, read now, if there is one.
    fn hit(&self, digest: &str, range: Option<ByteRange>) -> Result<Option<Answer>, ProxyError> {
        let (meta, born) = {
            let mut state = self.shared.state();
            state.tick += 1;
            let tick = state.tick;
            match state.entries.get_mut(digest) {
                Some(entry) => {
                    entry.used = tick;
                    (entry.meta.clone(), entry.born)
                }
                None => return Ok(None),
            }
        };
        let length = meta.length;
        let (start, end) = match range {
            None => (0, length.saturating_sub(1)),
            Some(range)
                if range.start < length && range.end.is_none_or(|end| end >= range.start) =>
            {
                (
                    range.start,
                    range.end.map_or(length - 1, |end| end.min(length - 1)),
                )
            }
            Some(_) => return Err(ProxyError::Unsatisfiable(length)),
        };
        let opened = File::open(self.shared.layout.object(digest)).and_then(|mut file| {
            file.seek(SeekFrom::Start(start))?;
            Ok(file)
        });
        let Ok(file) = opened else {
            // The file is gone (the directory was wiped): forget that object, if it is still the
            // one read, and the next ask fills it.
            self.shared.forget(digest, born);
            return Ok(None);
        };
        self.shared.state().stats.hits += 1;
        let wanted = if length == 0 { 0 } else { end - start + 1 };
        Ok(Some(Answer {
            served: Served::Hit,
            validator: meta.validator,
            last_modified: meta.last_modified,
            length: Some(length),
            fetched_at: meta.fetched_at,
            range: range.map(|_| (start, end)),
            body: file_body(file, wanted),
        }))
    }

    /// Makes the asker the leader of the key's fill, or gives it the flight to wait on.
    fn claim(&self, digest: &str) -> Claim {
        let mut state = self.shared.state();
        // Looked at under the lock that inserts the flight, and the commit inserts its entry
        // before its flight ends: a key is never led while it is committed.
        if state.entries.contains_key(digest) {
            return Claim::Cached;
        }
        if let Some(flight) = state.flights.get(digest) {
            return Claim::Wait(flight.subscribe());
        }
        state
            .flights
            .insert(digest.to_owned(), watch::channel(Done::Pending).0);
        Claim::Lead(Flight {
            shared: self.shared.clone(),
            digest: digest.to_owned(),
            finished: false,
        })
    }

    /// A place among the reads in flight, or [`ProxyError::Busy`].
    pub(crate) fn slot(&self, limits: &Limits) -> Result<Slot, ProxyError> {
        let mut state = self.shared.state();
        if state.active >= limits.fetches {
            return Err(ProxyError::Busy);
        }
        state.active += 1;
        Ok(Slot(self.shared.clone()))
    }

    /// The asker's own read of the origin, stored nowhere.
    async fn pass(
        &self,
        key: &Key,
        range: Option<ByteRange>,
        origin: &Origin,
        limits: &Limits,
    ) -> Result<Answer, ProxyError> {
        let slot = self.slot(limits)?;
        let fetched = origin(Request {
            key: key.clone(),
            range,
            condition: None,
        })
        .await?;
        self.shared.state().stats.passed += 1;
        // The place is held until the body is dropped, read to its end or not.
        let body = stream::unfold((fetched.body, slot), |(mut body, slot)| async move {
            body.next().await.map(|frame| (frame, (body, slot)))
        });
        Ok(Answer {
            served: Served::Passed,
            validator: fetched.validator,
            last_modified: fetched.last_modified,
            length: fetched.slice.map_or(fetched.length, |slice| slice.total),
            fetched_at: self.shared.clock.now_unix(),
            range: fetched.slice.map(|slice| (slice.start, slice.end)),
            body: Box::pin(body),
        })
    }

    /// The fill of the key the asker leads: its read starts here, and its body is streamed to the
    /// asker and written to a temp file by a task that outlives the asker.
    async fn lead(
        &self,
        key: &Key,
        flight: Flight,
        origin: &Origin,
        limits: &Limits,
    ) -> Result<Answer, ProxyError> {
        let slot = self.slot(limits)?;
        let fetched = origin(Request {
            key: key.clone(),
            range: None,
            condition: None,
        })
        .await?;
        Ok(self.start_fill(key, flight, fetched, slot, limits))
    }

    /// The fill of `key` from `fetched`, whose body is the whole object: streamed to the asker and
    /// written to a temp file by a task that outlives the asker. It replaces a stored object of
    /// the key when it commits.
    fn start_fill(
        &self,
        key: &Key,
        flight: Flight,
        fetched: Fetched,
        slot: Slot,
        limits: &Limits,
    ) -> Answer {
        let fits = fetched.length.is_none_or(|len| len <= limits.max_bytes);
        let sink = if fits {
            self.open_sink(key)
        } else {
            self.shared.state().stats.store_failures += 1;
            None
        };
        let fetched_at = self.shared.clock.now_unix();
        let (to_asker, from_fill) = mpsc::channel(AHEAD);
        let cut = Arc::new(AtomicBool::new(false));
        let cut_seen = Arc::clone(&cut);
        let answer = Answer {
            served: Served::Filled,
            validator: fetched.validator.clone(),
            last_modified: fetched.last_modified.clone(),
            length: fetched.length,
            fetched_at,
            range: None,
            body: Box::pin(stream::unfold(
                (from_fill, cut_seen),
                |(mut from_fill, cut)| async move {
                    match from_fill.recv().await {
                        Some(frame) => Some((frame, (from_fill, cut))),
                        // The fill dropped the asker for falling behind: an error, once, and
                        // not an end that looks whole.
                        None if cut.swap(false, Ordering::SeqCst) => {
                            Some((Err(FetchError::Behind), (from_fill, cut)))
                        }
                        None => None,
                    }
                },
            )),
        };
        self.shared.state().stats.fills += 1;
        let fill = Fill {
            shared: self.shared.clone(),
            key: key.clone(),
            sink,
            meta: Meta {
                validator: fetched.validator,
                last_modified: fetched.last_modified,
                fetched_at,
                length: 0,
            },
            declared: fetched.length,
            max_bytes: limits.max_bytes,
            asker: Some(to_asker),
            cut,
        };
        tokio::spawn(fill.run(fetched.body, flight, slot));
        answer
    }

    /// The temp file of a fill of `key`, or none when it cannot be made.
    fn open_sink(&self, key: &Key) -> Option<Sink> {
        let n = self.shared.counter.fetch_add(1, Ordering::Relaxed);
        let digest = key.digest();
        let body = self.shared.layout.tmp(&digest, n);
        match (self.shared.open_tmp)(&body) {
            Ok(writer) => Some(Sink { writer, body }),
            Err(why) => {
                tracing::warn!(error = %why, "the proxy cache could not make a temp file");
                None
            }
        }
    }
}

impl Shared {
    /// Forgets the object `digest` committed at `born` and deletes its files, unless that object
    /// has been evicted or replaced since it was read: then the files are another's.
    fn forget(&self, digest: &str, born: u64) {
        let mut state = self.state();
        if state
            .entries
            .get(digest)
            .is_none_or(|entry| entry.born != born)
        {
            return;
        }
        if let Some(entry) = state.entries.remove(digest) {
            state.total -= entry.meta.length;
        }
        // Under the lock: a commit of the key waits for the files to be gone.
        self.layout.remove(digest);
    }
}

/// Deletes the least recently used committed objects until `total` is at most `target`.
fn evict_down_to(shared: &Shared, state: &mut State, target: u64) {
    if state.total <= target {
        return;
    }
    let mut oldest: Vec<(u64, String)> = state
        .entries
        .iter()
        .map(|(digest, entry)| (entry.used, digest.clone()))
        .collect();
    oldest.sort_unstable();
    for (_, digest) in oldest {
        if state.total <= target {
            break;
        }
        let gone = state
            .entries
            .remove(&digest)
            .map_or(0, |entry| entry.meta.length);
        state.total -= gone;
        shared.layout.remove(&digest);
        state.stats.evictions += 1;
    }
}

/// The temp file a fill writes.
struct Sink {
    writer: Box<dyn Write + Send>,
    body: PathBuf,
}

/// A fill: the body read from the origin, streamed to the asker, written to `sink`, committed.
struct Fill {
    shared: Arc<Shared>,
    key: Key,
    sink: Option<Sink>,
    meta: Meta,
    declared: Option<u64>,
    max_bytes: u64,
    asker: Option<mpsc::Sender<Result<Bytes, FetchError>>>,
    /// Set when the asker was dropped for a full channel.
    cut: Arc<AtomicBool>,
}

impl Fill {
    async fn run(mut self, mut body: BodyStream, flight: Flight, slot: Slot) {
        let mut got = 0u64;
        let mut failed = None;
        let mut abandoned = false;
        while let Some(frame) = body.next().await {
            match frame {
                Ok(bytes) => {
                    got += bytes.len() as u64;
                    self.write(&bytes, got);
                    self.send(Ok(bytes)).await;
                }
                Err(why) => {
                    failed = Some(why);
                    break;
                }
            }
            // Nobody wants the rest: no asker and nothing written.
            if self.asker.is_none() && self.sink.is_none() {
                abandoned = true;
                break;
            }
        }
        drop(body);
        if failed.is_none()
            && !abandoned
            && let Some(want) = self.declared
            && got != want
        {
            failed = Some(FetchError::Short { got, want });
        }
        let done = match failed {
            Some(why) => {
                self.discard();
                self.send(Err(why)).await;
                Done::Failed
            }
            None if abandoned => Done::Uncached,
            None => self.commit(got),
        };
        drop(slot);
        flight.finish(done);
    }

    /// Writes a frame to the temp file, dropping the file when the write fails or the object has
    /// grown past what the cache may hold.
    fn write(&mut self, bytes: &[u8], got: u64) {
        let Some(sink) = self.sink.as_mut() else {
            return;
        };
        let written = if got > self.max_bytes {
            Err(std::io::Error::other("larger than proxy_cache_max_bytes"))
        } else {
            sink.writer.write_all(bytes)
        };
        if let Err(why) = written {
            tracing::warn!(error = %why, "the proxy cache stopped storing a file");
            self.discard();
            self.shared.state().stats.store_failures += 1;
        }
    }

    /// Passes a frame to the asker, who is dropped when it has gone. While the file is being
    /// stored (and for an error, which ends the fill) the fill does not wait for a full channel:
    /// an asker that has fallen behind is cut and gets [`FetchError::Behind`], and the fill goes
    /// on to the disk. With nothing stored the asker's pace is the fill's.
    async fn send(&mut self, frame: Result<Bytes, FetchError>) {
        let Some(asker) = &self.asker else {
            return;
        };
        let may_wait = self.sink.is_none() && frame.is_ok();
        let gone = if may_wait {
            asker.send(frame).await.is_err()
        } else {
            match asker.try_send(frame) {
                Ok(()) => false,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    self.cut.store(true, Ordering::SeqCst);
                    true
                }
                Err(mpsc::error::TrySendError::Closed(_)) => true,
            }
        };
        if gone {
            self.asker = None;
        }
    }

    /// Closes and deletes the temp file.
    fn discard(&mut self) {
        if let Some(sink) = self.sink.take() {
            drop(sink.writer);
            // A temp file that cannot be deleted is deleted by the next start.
            drop(std::fs::remove_file(sink.body));
        }
    }

    /// Commits the whole body of `got` bytes, making room first: the outcome of the flight.
    fn commit(&mut self, got: u64) -> Done {
        let Some(mut sink) = self.sink.take() else {
            return Done::Uncached;
        };
        let digest = self.key.digest();
        let tmp_meta = PathBuf::from(format!("{}.meta", sink.body.display()));
        self.meta.length = got;
        let mut state = self.shared.state();
        // The object this one replaces is dropped from the books now, to make room, and its files
        // with it when the commit fails: the books and the disk agree on every path.
        let replaced = state.entries.get(&digest).map_or(0, |e| e.meta.length);
        state.total -= replaced;
        state.entries.remove(&digest);
        evict_down_to(&self.shared, &mut state, self.max_bytes.saturating_sub(got));
        let committed = sink.writer.flush().and_then(|()| {
            self.shared
                .layout
                .commit(&self.key, &self.meta, &sink.body, &tmp_meta)
        });
        match committed {
            Ok(()) => {
                state.tick += 1;
                let used = state.tick;
                state.total += got;
                state.entries.insert(
                    digest,
                    Entry {
                        meta: self.meta.clone(),
                        used,
                        born: used,
                    },
                );
                Done::Committed
            }
            Err(why) => {
                tracing::warn!(error = %why, "the proxy cache could not commit a file");
                state.stats.store_failures += 1;
                self.shared.layout.remove(&digest);
                drop(state);
                drop(std::fs::remove_file(&sink.body));
                drop(std::fs::remove_file(&tmp_meta));
                Done::Uncached
            }
        }
    }
}

/// `wanted` bytes of `file` from where it is positioned, as frames.
fn file_body(file: File, wanted: u64) -> BodyStream {
    Box::pin(stream::unfold(
        (file, wanted),
        |(mut file, left)| async move {
            if left == 0 {
                return None;
            }
            let mut buf = vec![0u8; usize::try_from(left.min(CHUNK)).unwrap_or(0)];
            match file.read(&mut buf) {
                Ok(n) if n > 0 => {
                    buf.truncate(n);
                    Some((Ok(Bytes::from(buf)), (file, left - n as u64)))
                }
                // A file shorter than its sidecar says, or one that cannot be read.
                _ => Some((Err(FetchError::Cache), (file, 0))),
            }
        },
    ))
}

/// The cache of `--proxy-cache-dir` at start, opened under the limits of the settings in `setup`:
/// none without the flag.
///
/// # Errors
/// [`StartError::MdmSetupStore`]: the settings cannot be read. [`StartError::ProxyCache`]: the
/// directory cannot be made or read.
pub(crate) fn open_at_start(
    dir: Option<&Path>,
    setup: &crate::mdm_setup::MdmSetup,
) -> Result<Option<ProxyCache>, StartError> {
    let Some(dir) = dir else {
        return Ok(None);
    };
    let set = setup.settings().map_err(StartError::mdm_setup_store)?;
    ProxyCache::open(dir, Arc::new(SystemClock), &Limits::from_settings(&set))
        .map(Some)
        .map_err(|source| StartError::ProxyCache {
            path: dir.to_owned(),
            source,
        })
}

impl crate::service::Coordinator {
    /// The same coordinator with the package proxy's cache `cache` (`--proxy-cache-dir`).
    #[must_use]
    pub fn with_proxy_cache(mut self, cache: Option<ProxyCache>) -> Self {
        self.proxy.cache = cache.map(Arc::new);
        self
    }

    /// The package proxy's cache: none without `--proxy-cache-dir`.
    #[must_use]
    pub fn proxy_cache(&self) -> Option<&Arc<ProxyCache>> {
        self.proxy.cache.as_ref()
    }
}

#[path = "proxy_cache_stale.rs"]
mod stale;

#[cfg(test)]
#[path = "proxy_cache_review_tests.rs"]
mod review_tests;
#[cfg(test)]
#[path = "proxy_cache_start_tests.rs"]
mod start_tests;
#[cfg(test)]
#[path = "proxy_cache_tests.rs"]
mod tests;
