//! The published releases of hostkeeper, read over plain HTTPS with no token: hostkeeper's
//! repository is public, so its release list and its release assets need no login.
//!
//! - The list is GitHub's `GET <api>/repos/<repo>/releases?per_page=100`, once a run. Its body
//!   and its `ETag` are kept in the download directory (`releases.json`, `releases.etag`), and the
//!   next run asks with `If-None-Match`: a `304 Not Modified` is answered from the kept body,
//!   with nothing downloaded again. A `200` replaces the kept pair: the old `ETag` is removed
//!   first and the new one written last, so an `ETag` is never kept beside another body.
//! - An asset is `GET <site>/<repo>/releases/download/<tag>/<name>`, the public download URL,
//!   following its redirects (to GitHub's storage), `https://` only.
//! - GitHub limits requests without a token by address (60 an hour to its API). A `429`, or a
//!   `403` that says the limit is used up (`x-ratelimit-remaining: 0`, or a `Retry-After`), is
//!   [`FetchError::RateLimited`]: the run waits for the next timer tick, with nothing changed.
//!   Any other answer than the ones above is [`FetchError::Failed`].
//!
//! [`UreqWeb`] is the [`Web`] of a host: a blocking client with rustls, HTTPS only, no proxy, no
//! cookies, no credentials, trusting the roots of `--ca-bundle` (the system's bundle file by
//! default) and nothing else.
//! What it fetches is trusted only after its sum and its attestation are checked.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use ureq::tls::{Certificate, PemItem, RootCerts, TlsConfig, TlsProvider};

use super::Releases;
use crate::release::contract::Repo;
use crate::release::github::releases_of;
use crate::release::ports::{PortError, Release};

/// GitHub's API, whose release list the updater reads.
pub(crate) const API: &str = "https://api.github.com";
/// GitHub's site, whose public download URLs serve the release assets.
pub(crate) const SITE: &str = "https://github.com";
/// The kept body of the last release list GitHub sent.
pub(crate) const LIST: &str = "releases.json";
/// The `ETag` GitHub sent with it.
pub(crate) const ETAG: &str = "releases.etag";
/// The most bytes of a release list (100 releases, each with its assets).
pub(crate) const MAX_LIST_BYTES: u64 = 16 << 20;
/// What the client calls itself: GitHub's API refuses a request without a `User-Agent`.
pub(crate) const USER_AGENT: &str = "hostkeeper-coordinator-update";
/// The media type of GitHub's REST API.
pub(crate) const API_JSON: &str = "application/vnd.github+json";
/// The most redirects a request follows.
pub(crate) const MAX_REDIRECTS: u32 = 5;
/// The longest a request waits to connect.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// The longest one request may take, its body included (an image tarball).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// One GET.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Get<'a> {
    pub(crate) url: &'a str,
    /// The value of `Accept`.
    pub(crate) accept: &'a str,
    /// The value of `If-None-Match`, when one is sent.
    pub(crate) if_none_match: Option<&'a str>,
    /// The most bytes of the body that are read; more is an error.
    pub(crate) limit: Option<u64>,
}

/// The status and the headers of an answer that the updater reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Head {
    pub(crate) status: u16,
    pub(crate) etag: Option<String>,
    /// `x-ratelimit-remaining`.
    pub(crate) remaining: Option<String>,
    /// `x-ratelimit-reset`: when the limit is reset, in seconds since the Unix epoch.
    pub(crate) reset: Option<String>,
    /// `retry-after`, in seconds.
    pub(crate) retry_after: Option<String>,
}

/// Sends a GET and follows its redirects.
pub(crate) trait Web {
    /// The final answer's head; its body goes to `body` when, and only when, its status is 200.
    /// An error is a request that got no answer (no connection, TLS, a redirect to a URL that is
    /// not `https://`, a body cut short or longer than the limit).
    fn get(&self, get: &Get<'_>, body: &mut dyn Write) -> Result<Head, PortError>;
}

/// Why a release or an asset could not be fetched.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum FetchError {
    /// GitHub's limit of requests without a token is used up: try again at the next run.
    #[error("{0}")]
    RateLimited(String),
    #[error(transparent)]
    Failed(#[from] PortError),
}

/// [`Releases`] over GitHub's public URLs (see the module).
pub(crate) struct PublicReleases<'a> {
    pub(crate) web: &'a dyn Web,
    /// GitHub's API, `https://<host>[:<port>]` ([`API`]).
    pub(crate) api: &'a str,
    /// GitHub's site, `https://<host>[:<port>]` ([`SITE`]).
    pub(crate) site: &'a str,
    /// Where the last list and its `ETag` are kept: the download directory.
    pub(crate) cache: &'a Path,
}

impl PublicReleases<'_> {
    /// The kept `ETag` and body, when both are there.
    fn kept(&self) -> Option<(String, Vec<u8>)> {
        let etag = std::fs::read_to_string(self.cache.join(ETAG)).ok()?;
        let body = std::fs::read(self.cache.join(LIST)).ok()?;
        Some((etag.trim().to_owned(), body))
    }

    /// Keeps `body` and, when GitHub sent one, its `etag` (see the module for the order).
    fn keep(&self, etag: Option<&str>, body: &[u8]) -> Result<(), PortError> {
        let etag_path = self.cache.join(ETAG);
        remove_if_there(&etag_path)?;
        write_whole(&self.cache.join(LIST), body)?;
        match etag {
            Some(etag) => write_whole(&etag_path, etag.as_bytes()),
            None => Ok(()),
        }
    }
}

impl Releases for PublicReleases<'_> {
    fn list(&self, repo: &Repo) -> Result<Vec<Release>, FetchError> {
        let url = format!("{}/repos/{repo}/releases?per_page=100", self.api);
        let kept = self.kept();
        let mut body = Vec::new();
        let get = Get {
            url: &url,
            accept: API_JSON,
            if_none_match: kept.as_ref().map(|(etag, _)| etag.as_str()),
            limit: Some(MAX_LIST_BYTES),
        };
        let head = self.web.get(&get, &mut body)?;
        let what = format!("GET {url}");
        let body = match (head.status, kept) {
            (200, _) => {
                self.keep(head.etag.as_deref(), &body)?;
                body
            }
            (304, Some((_, kept))) => kept,
            _ => return Err(refusal(&what, &head)),
        };
        let value: Value = serde_json::from_slice(&body)
            .map_err(|e| PortError::new(what.clone(), format!("not JSON: {e}")))?;
        Ok(releases_of(&value, &what)?)
    }

    fn download(&self, repo: &Repo, tag: &str, name: &str, dir: &Path) -> Result<(), FetchError> {
        let url = format!("{}/{repo}/releases/download/{tag}/{name}", self.site);
        let what = format!("GET {url}");
        let path = dir.join(name);
        let mut file = File::create_new(&path)
            .map_err(|e| PortError::new(format!("create {}", path.display()), e.to_string()))?;
        let get = Get {
            url: &url,
            accept: "application/octet-stream",
            if_none_match: None,
            limit: None,
        };
        let outcome = match self.web.get(&get, &mut file) {
            Ok(head) if head.status == 200 => Ok(()),
            Ok(head) => Err(refusal(&what, &head)),
            Err(e) => Err(e.into()),
        };
        if outcome.is_err() {
            // What is there is not the asset; the directory it is in is discarded either way.
            drop(std::fs::remove_file(&path));
        }
        outcome
    }
}

/// The error of an answer that is not what was asked for: [`FetchError::RateLimited`] when it
/// says the limit is used up, else [`FetchError::Failed`] with its status.
fn refusal(what: &str, head: &Head) -> FetchError {
    let used_up = head.remaining.as_deref() == Some("0") || head.retry_after.is_some();
    match head.status {
        429 => {}
        403 if used_up => {}
        status => return PortError::new(what, format!("HTTP {status}")).into(),
    }
    let mut when = String::new();
    if let Some(reset) = &head.reset {
        when.push_str(&format!(", reset at {reset} (Unix time)"));
    }
    if let Some(secs) = &head.retry_after {
        when.push_str(&format!(", retry after {secs} seconds"));
    }
    FetchError::RateLimited(format!(
        "GitHub's limit of requests without a token is used up ({what}: HTTP {}{when})",
        head.status
    ))
}

/// Removes `path`; one that is not there is what is wanted.
fn remove_if_there(path: &Path) -> Result<(), PortError> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(PortError::new(
            format!("remove {}", path.display()),
            e.to_string(),
        )),
        _ => Ok(()),
    }
}

/// Writes `bytes` to `path` whole: to `<path>.part` first, then renamed into place.
fn write_whole(path: &Path, bytes: &[u8]) -> Result<(), PortError> {
    let mut part = path.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    std::fs::write(&part, bytes)
        .and_then(|()| std::fs::rename(&part, path))
        .map_err(|e| PortError::new(format!("write {}", path.display()), e.to_string()))
}

/// The default of `--ca-bundle`: the Debian family's CA bundle, as `pxe update-hook prepare`
/// takes it. A file, not the system's store as a library finds it, so that no environment
/// variable (`SSL_CERT_FILE`) decides what is trusted.
pub(crate) const CA_BUNDLE: &str = "/etc/ssl/certs/ca-certificates.crt";

/// The roots a [`UreqWeb`] trusts: every certificate of the PEM file `path`.
pub(crate) fn roots(path: &Path) -> Result<Vec<Certificate<'static>>, PortError> {
    let what = format!("--ca-bundle {}", path.display());
    let pem = std::fs::read(path).map_err(|e| PortError::new(what.clone(), e.to_string()))?;
    let certs: Vec<Certificate<'static>> = ureq::tls::parse_pem(&pem)
        .filter_map(|item| match item {
            Ok(PemItem::Certificate(cert)) => Some(cert),
            _ => None,
        })
        .collect();
    if certs.is_empty() {
        return Err(PortError::new(what, "no certificate could be read"));
    }
    Ok(certs)
}

/// The [`Web`] of a host (see the module).
pub(crate) struct UreqWeb {
    agent: ureq::Agent,
}

impl UreqWeb {
    /// A client that trusts `roots` and nothing else.
    pub(crate) fn new(roots: &[Certificate<'static>]) -> UreqWeb {
        let tls = TlsConfig::builder()
            .provider(TlsProvider::Rustls)
            .root_certs(RootCerts::new_with_certs(roots))
            .unversioned_rustls_crypto_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .build();
        let agent = ureq::Agent::config_builder()
            .https_only(true)
            .proxy(None)
            .http_status_as_error(false)
            .max_redirects(MAX_REDIRECTS)
            .user_agent(USER_AGENT)
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_global(Some(REQUEST_TIMEOUT))
            .tls_config(tls)
            .build()
            .into();
        UreqWeb { agent }
    }
}

impl Web for UreqWeb {
    fn get(&self, get: &Get<'_>, body: &mut dyn Write) -> Result<Head, PortError> {
        let what = format!("GET {}", get.url);
        let failed = |e: &dyn std::fmt::Display| PortError::new(what.clone(), e.to_string());
        let mut request = self.agent.get(get.url).header("Accept", get.accept);
        if let Some(etag) = get.if_none_match {
            request = request.header("If-None-Match", etag);
        }
        let mut response = request.call().map_err(|e| failed(&e))?;
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        let head = Head {
            status: response.status().as_u16(),
            etag: header("etag"),
            remaining: header("x-ratelimit-remaining"),
            reset: header("x-ratelimit-reset"),
            retry_after: header("retry-after"),
        };
        if head.status == 200 {
            // ureq's limit refuses a body of exactly its size (the read after it fails), so a
            // body of `limit` bytes is read with a limit one more.
            let limit = get.limit.map_or(u64::MAX, |l| l.saturating_add(1));
            let mut reader = response.body_mut().with_config().limit(limit).reader();
            io::copy(&mut reader, body).map_err(|e| failed(&e))?;
        }
        Ok(head)
    }
}
