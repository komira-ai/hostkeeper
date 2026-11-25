//! How the coordinator learns that a new Xcode exists (docs/design/maintenance-updates.md, section
//! 1.1): no host can see one, so the coordinator reads a public list of Xcode releases once a day
//! (the job `xcode-feed`), keeps what it read ([`KnownReleases`]), and compares it with what its
//! Xcode store holds ([`XcodeHeld`]). A release (not a beta, not a release candidate) newer than
//! any the store holds raises the warning alert `xcode_release_waiting` ([`waiting`]), which
//! clears when the store holds it.
//!
//! The list is the Xcode Releases site's `data.json` at the address of `--xcode-feed-url` (empty:
//! no job). Of each entry [`parse`] takes four fields, the version, the build, the release type
//! and the macOS it requires, and never a file or a link. A read that fails, or a list that cannot
//! be parsed, is a log line, and the list kept before stands until the next day's read: never an
//! alert.

use std::cmp::Ordering;
use std::convert::Infallible;
use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::StartError;
use crate::origin_https::OriginUrl;
use crate::service::Coordinator;
use crate::xcode_store::XcodeStore;

/// The period of the job `xcode-feed`: a day.
pub(crate) const FEED_EVERY: Duration = Duration::from_secs(86_400);

/// The most bytes of the list read: the whole list is about 400 KiB.
pub(crate) const MAX_FEED_BYTES: usize = 8 * 1024 * 1024;

/// How long a read of the list may take: the connection, the answer and the body.
pub(crate) const FEED_WITHIN: Duration = Duration::from_secs(60);

/// An Xcode version number as the list writes it (`27.2`, `16.4.1`), ordered by its numbers:
/// `27.10` is newer than `27.9`, and `27` is `27.0`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct XcodeVersion {
    text: String,
    numbers: Vec<u32>,
}

/// Why a version number is refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0:?} is not a version number: dot-separated decimal numbers")]
pub(crate) struct VersionError(String);

impl XcodeVersion {
    /// `text` as a version number: one to four decimal numbers separated by dots.
    ///
    /// # Errors
    /// [`VersionError`].
    pub(crate) fn parse(text: &str) -> Result<Self, VersionError> {
        let refused = || VersionError(text.to_owned());
        let numbers = text
            .split('.')
            .map(|part| {
                if !part.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(refused());
                }
                part.parse::<u32>().map_err(|_| refused())
            })
            .collect::<Result<Vec<u32>, VersionError>>()?;
        if numbers.len() > 4 {
            return Err(refused());
        }
        Ok(Self {
            text: text.to_owned(),
            numbers,
        })
    }

    /// The number as the list wrote it.
    pub(crate) fn as_str(&self) -> &str {
        &self.text
    }

    /// The `i`th number, 0 past the last.
    fn number(&self, i: usize) -> u32 {
        self.numbers.get(i).copied().unwrap_or(0)
    }
}

impl Ord for XcodeVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        let width = self.numbers.len().max(other.numbers.len());
        (0..width)
            .map(|i| self.number(i).cmp(&other.number(i)))
            .find(|order| order.is_ne())
            .unwrap_or(Ordering::Equal)
            // Two spellings of one number (`27`, `27.0`) are still two values.
            .then_with(|| self.text.cmp(&other.text))
    }
}

impl PartialOrd for XcodeVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for XcodeVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

/// One Xcode: its version and its build (`27.2`, `27B52`), ordered by version, then by build.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct XcodeId {
    /// The version number.
    pub(crate) version: XcodeVersion,
    /// Apple's build, as the list writes it.
    pub(crate) build: String,
}

/// What kind of entry of the list a release is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Channel {
    /// A release: the list's `release`.
    Release,
    /// A beta: the list's `beta`, and `dp` (a developer preview).
    Beta,
    /// A release candidate: the list's `rc`, and `gm` and `gmSeed` (a golden master, the
    /// candidate's earlier name).
    ReleaseCandidate,
}

impl Channel {
    /// Its token, as the store keeps it: `release`, `beta` or `rc`.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::Beta => "beta",
            Self::ReleaseCandidate => "rc",
        }
    }

    /// The channel of the store's token `text`.
    pub(crate) fn of_token(text: &str) -> Option<Self> {
        match text {
            "release" => Some(Self::Release),
            "beta" => Some(Self::Beta),
            "rc" => Some(Self::ReleaseCandidate),
            _ => None,
        }
    }

    /// The channel of the list's release type `key`: none for a type this build does not know.
    fn of_feed(key: &str) -> Option<Self> {
        match key {
            "release" => Some(Self::Release),
            "beta" | "dp" => Some(Self::Beta),
            "rc" | "gm" | "gmSeed" => Some(Self::ReleaseCandidate),
            _ => None,
        }
    }
}

/// One entry of the list, as the coordinator keeps it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct XcodeRelease {
    /// Its version and build.
    pub(crate) id: XcodeId,
    /// Release, beta or release candidate.
    pub(crate) channel: Channel,
    /// The macOS version it requires, as the list writes it (`26.6`).
    pub(crate) requires_macos: String,
}

/// Why the list could not be read or used.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum FeedError {
    /// The body is not a JSON array of objects.
    #[error("the list is not a JSON array of releases: {0}")]
    NotList(String),
    /// No entry of the list could be read.
    #[error("the list holds no entry with a version, a build, a release type and a macOS")]
    Empty,
    /// The read of the list failed.
    #[error("the list could not be read: {0}")]
    Read(String),
    /// The list read could not be kept.
    #[error("the list could not be kept: {0}")]
    Keep(String),
}

/// What [`parse`] read of a list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Parsed {
    /// The entries read, newest first, each once.
    pub(crate) releases: Vec<XcodeRelease>,
    /// How many entries were left out: one without a field [`parse`] takes, with a field it
    /// cannot read, or with a release type it does not know.
    pub(crate) skipped: usize,
}

/// An entry of the list: only the fields the coordinator takes. Every other field (the links
/// among them) is never read.
#[derive(Deserialize)]
struct Entry {
    version: Option<EntryVersion>,
    requires: Option<String>,
}

#[derive(Deserialize)]
struct EntryVersion {
    number: Option<String>,
    build: Option<String>,
    release: Option<serde_json::Map<String, serde_json::Value>>,
}

/// The entry `entry` as a release: none when a field is missing or cannot be read, or its release
/// type is not exactly one this build knows.
fn release_of(entry: Entry) -> Option<XcodeRelease> {
    let version = entry.version?;
    let number = XcodeVersion::parse(version.number?.trim()).ok()?;
    let build = version.build?.trim().to_owned();
    let kinds = version.release?;
    let mut keys = kinds.keys();
    let (Some(key), None) = (keys.next(), keys.next()) else {
        return None;
    };
    let channel = Channel::of_feed(key)?;
    let requires = entry.requires?.trim().to_owned();
    let fits = |text: &str| {
        !text.is_empty()
            && text.len() <= 32
            && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.')
    };
    (fits(&build) && fits(&requires)).then_some(XcodeRelease {
        id: XcodeId {
            version: number,
            build,
        },
        channel,
        requires_macos: requires,
    })
}

/// The releases of the list `body` (the Xcode Releases site's `data.json`): of each entry its
/// version's `number`, its `build`, the one key of its `release`, and `requires`, and nothing
/// else. An entry that lacks one, or whose release type is not `release`, `beta`, `dp`, `rc`,
/// `gm` or `gmSeed`, is left out and counted.
///
/// # Errors
/// [`FeedError::NotList`]: the body is not a JSON array of objects; [`FeedError::Empty`]: no
/// entry could be read.
pub(crate) fn parse(body: &[u8]) -> Result<Parsed, FeedError> {
    let entries: Vec<serde_json::Map<String, serde_json::Value>> =
        serde_json::from_slice(body).map_err(|why| FeedError::NotList(why.to_string()))?;
    let total = entries.len();
    let mut releases: Vec<XcodeRelease> = entries
        .into_iter()
        .filter_map(|entry| serde_json::from_value::<Entry>(serde_json::Value::Object(entry)).ok())
        .filter_map(release_of)
        .collect();
    releases.sort_by(|a, b| b.cmp(a));
    releases.dedup();
    let read = releases.len();
    if read == 0 {
        return Err(FeedError::Empty);
    }
    Ok(Parsed {
        releases,
        skipped: total.saturating_sub(read),
    })
}

/// The newest release of `known` (of the channel [`Channel::Release`]: never a beta or a release
/// candidate) when no Xcode of `held` is as new: the one the coordinator waits for the store to
/// hold. None when `known` holds no release, or the store holds it or a newer one.
pub(crate) fn waiting<'a>(known: &'a [XcodeRelease], held: &[XcodeId]) -> Option<&'a XcodeRelease> {
    let newest = known
        .iter()
        .filter(|release| release.channel == Channel::Release)
        .max_by(|a, b| a.id.cmp(&b.id))?;
    (!held.iter().any(|id| *id >= newest.id)).then_some(newest)
}

/// The text of the alert `xcode_release_waiting` for `release`.
pub(crate) fn waiting_message(release: &XcodeRelease) -> String {
    let id = &release.id;
    format!(
        "Xcode {} ({}), which requires macOS {} or later, is released and the coordinator's \
         Xcode store holds no release as new: push it from a Mac signed in to the Apple \
         developer account with `hk xcode-push {}`",
        id.version, id.build, release.requires_macos, id.version
    )
}

/// The address of the list (`--xcode-feed-url`): an `https://` URL of a file, as the origin client
/// reads it ([`crate::repository_origin::RepositoryOrigin::read_file`]): the base and the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FeedUrl {
    base: String,
    file: String,
}

/// Why `--xcode-feed-url` is refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum FeedUrlError {
    /// Not an `https://` URL of a host, without a user, a query or a fragment.
    #[error("{0}")]
    Url(#[from] crate::origin_https::OriginUrlError),
    /// No file at the end of the path.
    #[error("{0:?} names no file: the list's address ends with its file's name")]
    NoFile(String),
}

impl FeedUrl {
    /// `text` as the list's address: `https://<host>[:<port>]/[<path>/]<file>`.
    ///
    /// # Errors
    /// [`FeedUrlError`].
    pub(crate) fn parse(text: &str) -> Result<Self, FeedUrlError> {
        OriginUrl::parse(text)?;
        let no_file = || FeedUrlError::NoFile(text.to_owned());
        let (_, path) = text
            .split_once("://")
            .and_then(|(_, after_scheme)| after_scheme.split_once('/'))
            .ok_or_else(no_file)?;
        let (_, file) = path.rsplit_once('/').unwrap_or(("", path));
        if file.is_empty() {
            return Err(no_file());
        }
        let base = &text[..text.len() - file.len() - 1];
        OriginUrl::parse(base)?;
        Ok(Self {
            base: base.to_owned(),
            file: file.to_owned(),
        })
    }

    /// The base URL the origin client reads under.
    pub(crate) fn base(&self) -> &str {
        &self.base
    }

    /// The file under the base.
    pub(crate) fn file(&self) -> &str {
        &self.file
    }
}

impl fmt::Display for FeedUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.base, self.file)
    }
}

/// The list of releases the coordinator read last: replaced whole by each read that succeeds,
/// read by every observation of the alerts.
pub(crate) trait KnownReleases: Send + Sync {
    /// Replaces the list with `releases`.
    ///
    /// # Errors
    /// Why it could not be written: the list kept before stands.
    fn keep(&self, releases: &[XcodeRelease]) -> Result<(), String>;

    /// The list: empty before the first read that succeeded.
    ///
    /// # Errors
    /// Why it could not be read.
    fn known(&self) -> Result<Vec<XcodeRelease>, String>;
}

/// The list in this process alone: a coordinator with no store of it (a test's).
#[derive(Debug, Default)]
pub(crate) struct MemoryReleases(Mutex<Vec<XcodeRelease>>);

impl KnownReleases for MemoryReleases {
    fn keep(&self, releases: &[XcodeRelease]) -> Result<(), String> {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = releases.to_vec();
        Ok(())
    }

    fn known(&self) -> Result<Vec<XcodeRelease>, String> {
        Ok(self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone())
    }
}

/// The releases the coordinator's Xcode store holds.
pub(crate) trait XcodeHeld: Send + Sync {
    /// Each Xcode the store holds.
    ///
    /// # Errors
    /// Why the store could not be read.
    fn held(&self) -> Result<Vec<XcodeId>, String>;
}

/// The Xcode store of a coordinator that has none: it holds no release.
#[derive(Debug, Default)]
pub(crate) struct NoXcodeStore;

impl XcodeHeld for NoXcodeStore {
    fn held(&self) -> Result<Vec<XcodeId>, String> {
        Ok(Vec::new())
    }
}

/// What the coordinator has of Xcode: the list's address (none: the job is off), the list it read
/// last, and its Xcode store.
#[derive(Clone)]
pub(crate) struct Xcode {
    /// `--xcode-feed-url`; none when it is empty.
    pub(crate) feed: Option<FeedUrl>,
    /// The list read last.
    pub(crate) known: Arc<dyn KnownReleases>,
    /// What the Xcode store holds.
    pub(crate) held: Arc<dyn XcodeHeld>,
    /// The Xcode store of `--xcode-dir`; none without the flag, when the store's routes are not
    /// served.
    pub(crate) store: Option<Arc<XcodeStore>>,
}

impl Default for Xcode {
    fn default() -> Self {
        Self {
            feed: None,
            known: Arc::new(MemoryReleases::default()),
            held: Arc::new(NoXcodeStore),
            store: None,
        }
    }
}

impl fmt::Debug for Xcode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Xcode")
            .field("feed", &self.feed)
            .finish_non_exhaustive()
    }
}

impl Xcode {
    /// The release the alert `xcode_release_waiting` names now ([`waiting`]), of the list kept and
    /// what the store holds; none when either cannot be read, which is logged.
    pub(crate) fn release_waiting(&self) -> Option<XcodeRelease> {
        let known = self
            .known
            .known()
            .map_err(|why| tracing::warn!(%why, "the list of Xcode releases could not be read"))
            .ok()?;
        let held = self
            .held
            .held()
            .map_err(|why| tracing::warn!(%why, "the Xcode store could not be read"))
            .ok()?;
        waiting(&known, &held).cloned()
    }
}

/// A release as the files store and the database keep it: the four fields, as text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct KeptRelease {
    /// The version number.
    pub(crate) version: String,
    /// The build.
    pub(crate) build: String,
    /// `release`, `beta` or `rc` ([`Channel::as_str`]).
    pub(crate) channel: String,
    /// The macOS it requires.
    pub(crate) requires_macos: String,
}

impl From<&XcodeRelease> for KeptRelease {
    fn from(release: &XcodeRelease) -> Self {
        Self {
            version: release.id.version.as_str().to_owned(),
            build: release.id.build.clone(),
            channel: release.channel.as_str().to_owned(),
            requires_macos: release.requires_macos.clone(),
        }
    }
}

impl KeptRelease {
    /// The release it keeps.
    ///
    /// # Errors
    /// The version or the channel cannot be read.
    pub(crate) fn release(&self) -> Result<XcodeRelease, String> {
        let version = XcodeVersion::parse(&self.version).map_err(|why| why.to_string())?;
        let channel = Channel::of_token(&self.channel)
            .ok_or_else(|| format!("{:?} is not a channel of a kept release", self.channel))?;
        Ok(XcodeRelease {
            id: XcodeId {
                version,
                build: self.build.clone(),
            },
            channel,
            requires_macos: self.requires_macos.clone(),
        })
    }
}

impl Coordinator {
    /// The same coordinator reading the list at `feed` once a day (`--xcode-feed-url`); none: the
    /// job `xcode-feed` is off.
    #[must_use]
    pub(crate) fn with_xcode_feed(mut self, feed: Option<FeedUrl>) -> Self {
        self.xcode.feed = feed;
        self
    }

    /// The same coordinator keeping the list it reads in `known` instead of in its memory.
    #[must_use]
    pub(crate) fn with_known_xcode_releases(mut self, known: Arc<dyn KnownReleases>) -> Self {
        self.xcode.known = known;
        self
    }

    /// The same coordinator asking `held` which releases its Xcode store holds.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_xcode_held(mut self, held: Arc<dyn XcodeHeld>) -> Self {
        self.xcode.held = held;
        self
    }

    /// The same coordinator keeping Xcode archives in `store` (`--xcode-dir`): it serves the
    /// store's routes, and the store says which releases it holds.
    #[must_use]
    pub(crate) fn with_xcode_store(mut self, store: Arc<XcodeStore>) -> Self {
        self.xcode.held = Arc::clone(&store) as Arc<dyn XcodeHeld>;
        self.xcode.store = Some(store);
        self
    }

    /// The same coordinator keeping Xcode archives in the directory `dir`, none larger than
    /// `max_bytes` (`--xcode-dir`, `--xcode-max-bytes`), as its start keeps them: the store is
    /// opened on the clock of the coordinator's ops store, so call it after
    /// [`Coordinator::with_ops`].
    ///
    /// # Errors
    /// [`StartError::XcodeDir`]: `dir` is not a directory that can be read, or an upload a stopped
    /// coordinator left in it could not be removed.
    pub fn with_xcode_dir(self, dir: &Path, max_bytes: u64) -> Result<Self, StartError> {
        let now = std::time::UNIX_EPOCH + Duration::from_secs(self.ops.unix_now());
        let store = XcodeStore::open(dir, max_bytes, now)
            .map_err(|why| StartError::XcodeDir(why.to_string()))?;
        Ok(self.with_xcode_store(Arc::new(store)))
    }

    /// Whether the job `xcode-feed` runs: `--xcode-feed-url` is not empty.
    #[must_use]
    pub(crate) fn reads_xcode_feed(&self) -> bool {
        self.xcode.feed.is_some()
    }

    /// One read of the list: fetched over HTTPS by the origin client (at most [`MAX_FEED_BYTES`],
    /// within [`FEED_WITHIN`], no redirect), parsed ([`parse`]) and kept whole; how many releases
    /// it holds. Without `--xcode-feed-url` it reads nothing and keeps nothing.
    ///
    /// # Errors
    /// [`FeedError`]: nothing is kept, and the list kept before stands.
    pub(crate) async fn read_xcode_feed(&self) -> Result<usize, FeedError> {
        let Some(feed) = self.xcode.feed.as_ref() else {
            return Ok(0);
        };
        let origin = self
            .repository_origin
            .as_ref()
            .ok_or_else(|| FeedError::Read("the coordinator has no HTTPS client".to_owned()))?;
        let body = origin
            .read_file(feed.base(), feed.file(), None, MAX_FEED_BYTES, FEED_WITHIN)
            .await
            .map_err(|why| FeedError::Read(why.to_string()))?;
        let Parsed { releases, skipped } = parse(&body)?;
        let count = releases.len();
        let known = Arc::clone(&self.xcode.known);
        let newest = releases
            .iter()
            .find(|release| release.channel == Channel::Release)
            .map(|release| format!("{} ({})", release.id.version, release.id.build));
        // The store's write blocks: it runs off the runtime's threads.
        tokio::task::spawn_blocking(move || known.keep(&releases))
            .await
            .map_err(|why| FeedError::Keep(why.to_string()))?
            .map_err(FeedError::Keep)?;
        tracing::info!(
            releases = count,
            skipped,
            newest = newest.as_deref().unwrap_or("none"),
            "the list of Xcode releases was read"
        );
        Ok(count)
    }

    /// The job `xcode-feed`: [`Self::read_xcode_feed`], whose failure is a log line and no alert;
    /// the next run is a period later. It then prunes the Xcode store
    /// ([`Self::prune_xcode_store`]), so that a group's lowered `xcode_keep` takes effect within a
    /// day.
    pub(crate) async fn xcode_feed_job(&self) {
        let read = self.read_xcode_feed().await;
        self.prune_xcode_store();
        if let Err(why) = read {
            let url = self
                .xcode
                .feed
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default();
            tracing::warn!(
                %why,
                %url,
                "the list of Xcode releases could not be read; the job reads it again at its next run"
            );
        }
    }
}

/// The job `xcode-feed` of a coordinator with no database (one coordinator, nothing to claim): at
/// once, then every `every`, for as long as the future is polled. Without `--xcode-feed-url` it
/// reads nothing and waits for ever. It never returns.
pub(crate) async fn read_every(coordinator: Arc<Coordinator>, every: Duration) -> Infallible {
    if coordinator.reads_xcode_feed() {
        let mut ticks = tokio::time::interval(every);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticks.tick().await;
            coordinator.xcode_feed_job().await;
        }
    } else {
        std::future::pending().await
    }
}

#[cfg(test)]
#[path = "xcode_feed_tests.rs"]
mod tests;
