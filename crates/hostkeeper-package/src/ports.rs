//! The two ports of this crate and the data that crosses them: [`Fetcher`], which downloads one
//! URL into a [`Sink`], and [`Verifier`], which checks one attestation offline. [`crate::net`] and
//! [`crate::gh`] are their real adapters; the tests use fakes.

use std::path::Path;
use std::time::Duration;

use hostkeeper_config::daemon::{Sha256Hex, TrustedSigners};
use hostkeeper_steps::{Clock, Stamp};

/// A URL on the release channel, made by [`crate::index::index_url`] or
/// [`crate::index::package_url`] from the daemon config's channel, an order's service and version,
/// and the host's platform (the bundle's adds a suffix to the package's). Nothing else makes one,
/// so a fetch can only reach the configured channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url(String);

impl Url {
    pub(crate) fn new(text: String) -> Url {
        Url(text)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// This URL with `suffix` appended to its last segment.
    pub(crate) fn with_suffix(&self, suffix: &str) -> Url {
        Url(format!("{}{suffix}", self.0))
    }
}

/// The bounds of one transfer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limit {
    /// The longest the adapter waits to connect.
    pub connect_timeout: Duration,
    /// When the whole transfer must have ended, on the injected clock.
    pub deadline: Stamp,
}

/// The seconds left until `deadline` on `clock`: `0` once it has passed, and `0` when the host
/// booted since it was set (the time before a reboot is unknown).
pub fn remaining_secs(deadline: &Stamp, clock: &dyn Clock) -> u64 {
    if deadline.boot_id == clock.boot_id() {
        deadline.mono_secs.saturating_sub(clock.mono_secs())
    } else {
        0
    }
}

/// `start` plus `secs`, on the same boot.
pub(crate) fn after(start: &Stamp, secs: u64) -> Stamp {
    Stamp {
        boot_id: start.boot_id.clone(),
        mono_secs: start.mono_secs.saturating_add(secs),
    }
}

/// The sink's answer that ends a transfer: the adapter reads nothing more and returns
/// [`Got::Stopped`]. The sink knows why it stopped; the adapter does not need to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stop;

/// Where a transfer's bytes go, in order, as they arrive.
pub trait Sink {
    /// Called once, after the response head and before the first byte, with the length the
    /// server announced, if it announced one.
    fn begin(&mut self, content_length: Option<u64>) -> Result<(), Stop>;
    /// Called for each chunk of the body, in order.
    fn chunk(&mut self, bytes: &[u8]) -> Result<(), Stop>;
}

/// How a transfer ended when the network did not fail it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Got {
    /// Every byte of the body went to the sink.
    Complete,
    /// The sink stopped the transfer.
    Stopped,
}

/// Why the network did not deliver a body.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum NetError {
    #[error("could not connect, or the request was not answered")]
    Connect,
    /// A status other than 2xx, a redirect included.
    #[error("the server answered with status {0}")]
    Status(u16),
    #[error("the body ended before the length the server announced")]
    Truncated,
    #[error("the transfer did not end before its deadline")]
    DeadlineExceeded,
    #[error("reading the body failed")]
    Io,
}

/// Downloads one URL.
pub trait Fetcher {
    /// Sends one request for `url` and gives the body to `sink`: [`Sink::begin`] once, then
    /// [`Sink::chunk`] for each chunk, stopping at once when the sink returns [`Stop`]. Returns
    /// [`Got::Complete`] only when the whole body was delivered, and never waits past
    /// `limit.deadline`.
    fn get(&self, url: &Url, sink: &mut dyn Sink, limit: Limit) -> Result<Got, NetError>;
}

/// A commit, as a source digest: 40 or 64 lower-case hex digits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitId(String);

impl CommitId {
    pub fn new(text: &str) -> Option<CommitId> {
        let hex = text
            .bytes()
            .all(|b| b.is_ascii_digit() | (b'a'..=b'f').contains(&b));
        (hex & matches!(text.len(), 40 | 64)).then(|| CommitId(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The most bytes of a [`SourceRef`].
pub const MAX_SOURCE_REF: usize = 256;

/// A git ref an attestation was made on: `refs/` and 1 to [`MAX_SOURCE_REF`] bytes in all of
/// letters, digits and `. _ - + /`, with no empty, `.` or `..` component. It is passed to the
/// verifier as one argument, so what could not be a ref is refused here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceRef(String);

impl SourceRef {
    pub fn new(text: &str) -> Option<SourceRef> {
        let plain = text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() | b"._-+/".contains(&b));
        let rest = text.strip_prefix("refs/").unwrap_or("");
        let components = rest.split('/').all(|c| !matches!(c, "" | "." | ".."));
        (plain & components & (text.len() <= MAX_SOURCE_REF)).then(|| SourceRef(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What the verifier is asked: is `archive`, whose SHA-256 is `sha256`, attested by `bundle` for
/// `commit` on `source_ref`, by `signers`? `commit` and `source_ref` are what the bundle itself
/// names; the verifier checks them against the signed certificate, so a bundle that lies about
/// either fails.
#[derive(Clone, Copy, Debug)]
pub struct Attest<'a> {
    pub archive: &'a Path,
    pub bundle: &'a Path,
    pub sha256: &'a Sha256Hex,
    pub commit: &'a CommitId,
    pub source_ref: &'a SourceRef,
    pub signers: &'a TrustedSigners,
    /// When the verifier must have answered, on the injected clock.
    pub deadline: &'a Stamp,
}

/// What a verifier confirmed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attested {
    pub commit: CommitId,
    pub source_ref: SourceRef,
}

/// Why a verifier did not confirm an attestation.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum VerifyError {
    /// The verifier could not be run, or gave no verdict: nothing is known about the package.
    #[error("the verifier is unavailable")]
    Unavailable,
    /// The verifier did not answer before the deadline.
    #[error("the verifier ran out of time")]
    TimedOut,
    /// The verifier's verdict: not attested by the trusted signer. The text is for the log.
    #[error("not attested by the trusted signer: {0}")]
    Rejected(String),
}

/// Checks one attestation offline.
pub trait Verifier {
    fn verify(&self, request: &Attest<'_>) -> Result<Attested, VerifyError>;
}
