//! The release channel a set of the desired state is checked against (`--release-channel`).
//!
//! CI publishes a version to the release channel and then sets it as the desired version
//! (design section 5.2). When the set comes first, every host the version is ordered to fetches
//! an index the channel does not hold yet, and the service's rollout stays live on a version
//! that cannot succeed. With `--release-channel` the coordinator asks the channel first: before a
//! set is decided, it fetches the version's index, `{channel}/{service}/{version}/index.json`
//! ([`index_path`]), and refuses the set unless the channel holds it:
//!
//! - no index (404), an answer that is not an index, an index that lists no package, or a fetch
//!   that failed in any way (no connection, a deadline passed, a status other than 200 or 404, a
//!   redirect, an answer over [`MAX_INDEX_BYTES`]): [`ChannelRefusal::NotInChannel`], token
//!   [`NOT_IN_CHANNEL`]. It is retryable: setting the version again once the channel holds it
//!   succeeds;
//! - an index of another service or version, or, when the set names a package digest, an index
//!   none of whose packages has that SHA-256, or, when the set names a release index, an index
//!   whose own SHA-256 is not the one the set names: [`ChannelRefusal::DigestMismatch`], token
//!   [`CHANNEL_DIGEST_MISMATCH`]. It is not retryable: the channel holds another artifact than
//!   the one CI is setting.
//!
//! A set the channel holds goes on to the refusals it always had. A refusal here changes
//! nothing: the state file, the ops file and memory stay as they were, and no host is ordered.
//!
//! The coordinator reads only what it compares (`service`, `version` and the `sha256` of each
//! package) and ignores the rest: the index is the daemon's to check in full, which it does
//! before it fetches a package (design section 5.5). How the index is fetched is a
//! [`ChannelFetcher`]: [`crate::channel_https::HttpsChannel`] in the binary, a fake in the
//! tests. Without the flag nothing is fetched and no set is refused for the channel.

use std::fmt;
use std::sync::Arc;

use futures::future::BoxFuture;
use hostkeeper_wire::version::Version;
use serde::Deserialize;

use crate::desired::Desired;
use crate::service::Coordinator;

/// The most bytes of an index the coordinator reads; a longer answer is a failed fetch.
pub const MAX_INDEX_BYTES: usize = 64 * 1024;

/// The reason token of a set whose version the release channel does not hold.
pub const NOT_IN_CHANNEL: &str = "not_in_channel";

/// The reason token of a set the release channel's index of the version disagrees with.
pub const CHANNEL_DIGEST_MISMATCH: &str = "channel_digest_mismatch";

/// `{service}/{version}/index.json`: the index of a version, relative to the channel.
#[must_use]
pub fn index_path(service: &str, version: &Version) -> String {
    format!("{service}/{version}/index.json")
}

/// What the channel answered for a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fetched {
    /// 200, with the body: at most [`MAX_INDEX_BYTES`].
    Found(Vec<u8>),
    /// 404: the channel holds nothing at the path.
    NotFound,
}

/// Why a fetch from the channel failed. Every one refuses the set as [`NOT_IN_CHANNEL`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FetchError {
    /// The connection or its TLS handshake failed.
    #[error("{url}: the connection failed: {why}")]
    Connect {
        /// The URL asked.
        url: String,
        /// Why.
        why: String,
    },
    /// The connection and its TLS handshake were not done within the connect deadline.
    #[error("{url}: not connected within {secs} s")]
    ConnectTimedOut {
        /// The URL asked.
        url: String,
        /// The deadline, in seconds.
        secs: u64,
    },
    /// The whole fetch was not done within the total deadline.
    #[error("{url}: not answered within {secs} s")]
    TimedOut {
        /// The URL asked.
        url: String,
        /// The deadline, in seconds.
        secs: u64,
    },
    /// The answer is longer than [`MAX_INDEX_BYTES`].
    #[error("{url}: the answer is longer than {MAX_INDEX_BYTES} bytes")]
    TooLarge {
        /// The URL asked.
        url: String,
    },
    /// A status other than 200 and 404; a redirect is one, and is not followed.
    #[error("{url}: status {status}")]
    Status {
        /// The URL asked.
        url: String,
        /// The status.
        status: u16,
    },
    /// HTTP failed after the handshake.
    #[error("{url}: {why}")]
    Http {
        /// The URL asked.
        url: String,
        /// Why.
        why: String,
    },
}

/// Fetches a path of the release channel, bounded: a connect deadline, a total deadline, at most
/// [`MAX_INDEX_BYTES`] of answer, and no redirect followed.
pub trait ChannelFetcher: Send + Sync {
    /// What the channel holds at `path` (`{service}/{version}/index.json`).
    fn fetch<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Fetched, FetchError>>;
}

/// Why the release channel refused a set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ChannelRefusal {
    /// The channel does not hold the version's index, or it could not be read: retry once the
    /// channel holds it.
    #[error(
        "not_in_channel: the release channel does not hold the version; set it again once it does"
    )]
    NotInChannel,
    /// The channel's index of the version names another service or version, or no package with
    /// the digest the set names, or is not the release index the set names.
    #[error(
        "channel_digest_mismatch: the release channel's index of the version does not match the set"
    )]
    DigestMismatch,
}

impl ChannelRefusal {
    /// The reason token: [`NOT_IN_CHANNEL`] or [`CHANNEL_DIGEST_MISMATCH`].
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::NotInChannel => NOT_IN_CHANNEL,
            Self::DigestMismatch => CHANNEL_DIGEST_MISMATCH,
        }
    }
}

/// What the coordinator reads of an index: the rest is the daemon's to check.
#[derive(Deserialize)]
struct Listed {
    service: String,
    version: String,
    packages: Vec<ListedPackage>,
}

#[derive(Deserialize)]
struct ListedPackage {
    sha256: String,
}

/// The release channel a set is checked against.
#[derive(Clone)]
pub struct ReleaseChannel {
    fetcher: Arc<dyn ChannelFetcher>,
}

impl fmt::Debug for ReleaseChannel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReleaseChannel").finish_non_exhaustive()
    }
}

impl ReleaseChannel {
    /// The channel `fetcher` reads.
    #[must_use]
    pub fn new(fetcher: Arc<dyn ChannelFetcher>) -> Self {
        Self { fetcher }
    }

    /// Whether the channel holds `desired.version` of `service`, with the package digest or the
    /// release index the set names, if it names one (the module's rules).
    ///
    /// # Errors
    /// [`ChannelRefusal`].
    pub async fn check(&self, service: &str, desired: &Desired) -> Result<(), ChannelRefusal> {
        let version = &desired.version;
        let path = index_path(service, version);
        let refused = |refusal: ChannelRefusal, why: &dyn fmt::Display| {
            tracing::warn!(%service, %version, %why, reason = refusal.token(), "refused a desired state: the release channel");
            refusal
        };
        let body = match self.fetcher.fetch(&path).await {
            Ok(Fetched::Found(body)) => body,
            Ok(Fetched::NotFound) => {
                return Err(refused(ChannelRefusal::NotInChannel, &"no index"));
            }
            Err(why) => return Err(refused(ChannelRefusal::NotInChannel, &why)),
        };
        let listed: Listed = serde_json::from_slice(&body)
            .map_err(|why| refused(ChannelRefusal::NotInChannel, &why))?;
        if listed.service != service || listed.version != version.as_str() {
            let why = format!(
                "the index is of {} {}",
                listed.service.escape_debug(),
                listed.version.escape_debug()
            );
            return Err(refused(ChannelRefusal::DigestMismatch, &why));
        }
        if listed.packages.is_empty() {
            return Err(refused(ChannelRefusal::NotInChannel, &"no package"));
        }
        // A set over a release index names the very index the channel must hold: its SHA-256.
        if let Some(record) = &desired.release_index
            && crate::ops_file::digest(&body) != record.digest.as_str()
        {
            let why = format!("the index is not the one of SHA-256 {}", record.digest);
            return Err(refused(ChannelRefusal::DigestMismatch, &why));
        }
        if let Some(digest) = &desired.release_digest
            && !listed
                .packages
                .iter()
                .any(|package| package.sha256.eq_ignore_ascii_case(digest.as_str()))
        {
            let why = format!("no package has the digest {digest}");
            return Err(refused(ChannelRefusal::DigestMismatch, &why));
        }
        Ok(())
    }
}

impl Coordinator {
    /// The coordinator, refusing a set of the desired state whose version `channel` does not
    /// hold (the module's rules).
    #[must_use]
    pub fn with_release_channel(mut self, channel: ReleaseChannel) -> Self {
        self.channel = Some(channel);
        self
    }
}
