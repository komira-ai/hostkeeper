//! A pinned file fetched from a list of URLs: the first download whose SHA-256 (and size, when
//! one is pinned) is the pin's is kept. `debs` (the Debian packages a CI job installs with dpkg)
//! and `netboot-iso` (the Ubuntu ISO of the network-install jobs) fetch through it, so that a
//! CI job whose first source is unreachable, Canonical's names failing to resolve included, takes
//! its file from the next.
//!
//! [`fetch`] tries the URLs in their order, in [`WAITS`]`.len()` rounds, pausing [`WAITS`]`[i]`
//! before round `i` (none before the first). Each attempt is one GET through a [`Web`] (HTTPS
//! only, each request bounded by its client's own timeouts), its body written to `<name>.part`
//! beside the file and hashed as it is written; the attempt is kept only when the answer is a
//! `200`, its SHA-256 is the pin's and its size the pinned one: then `<name>.part` is renamed to
//! `<name>` (mode 0644). A download of other bytes is one more failed attempt, like an
//! unreachable URL. When every attempt failed, `<name>.part` is removed and the error names each
//! attempt, in order, with why it failed; the file is never written.

use std::fs::File;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::Digest;

use crate::coordinator_update::public::{Get, Web};
use crate::install::Sha256;

/// The pause before each round of attempts: none before the first.
pub(crate) const WAITS: [Duration; 3] = [
    Duration::ZERO,
    Duration::from_secs(10),
    Duration::from_secs(30),
];

/// The `Accept` of a download: any type, the bytes are checked by their digest.
const ACCEPT: &str = "*/*";

/// A pinned file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pin {
    /// Its file name in the directory it is fetched into.
    pub(crate) name: String,
    /// Its SHA-256.
    pub(crate) sha256: Sha256,
    /// Its size in bytes, when it is pinned too: a body longer than that is cut off and refused.
    pub(crate) bytes: Option<u64>,
}

/// Why a fetch failed.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum Error {
    /// The `.part` file or the file could not be written, or the `.part` file not removed.
    #[error("{path:?}: {why}")]
    Write {
        /// The file.
        path: PathBuf,
        /// The system's reason.
        why: String,
    },
    /// No attempt gave the pinned file.
    #[error("{name}: no download was the pinned file: {}", attempts.join("; "))]
    Unavailable {
        /// The file's name.
        name: String,
        /// Each attempt, in order: its URL and why it failed.
        attempts: Vec<String>,
    },
}

/// Whether `base` is a base URL a path is appended to: `https://` and a host, and no trailing
/// `/`, query, fragment or user.
pub(crate) fn is_base_url(base: &str) -> bool {
    let host = base.strip_prefix("https://").unwrap_or_default();
    !host.is_empty() && !base.ends_with('/') && !base.contains(['?', '#', '@'])
}

/// A writer that hashes and counts what it writes to `inner`.
pub(crate) struct Hashed<W> {
    pub(crate) inner: W,
    hasher: sha2::Sha256,
    bytes: u64,
}

impl<W: Write> Hashed<W> {
    pub(crate) fn new(inner: W) -> Hashed<W> {
        Hashed {
            inner,
            hasher: sha2::Sha256::new(),
            bytes: 0,
        }
    }

    /// How many bytes were written, and their SHA-256.
    pub(crate) fn finish(self) -> (u64, Sha256) {
        (
            self.bytes,
            Sha256::from_digest(self.hasher.finalize().into()),
        )
    }
}

impl<W: Write> Write for Hashed<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        self.bytes += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Fetches `pin` into `dir` from the first of `urls` that gives its pinned bytes (see the
/// module), through `web`, `pause` waiting before each round: the line that says where it came
/// from, and the attempts that failed before it.
pub(crate) fn fetch(
    urls: &[String],
    pin: &Pin,
    dir: &Path,
    web: &dyn Web,
    pause: &dyn Fn(Duration),
) -> Result<String, Error> {
    let path = dir.join(&pin.name);
    let part = dir.join(format!("{}.part", pin.name));
    let mut attempts = Vec::new();
    for wait in WAITS {
        pause(wait);
        for url in urls {
            match attempt(url, pin, &part, web)? {
                None => {
                    std::fs::rename(&part, &path).map_err(write_error(&path))?;
                    let mut line = format!("ok: {url} has SHA-256 {}", pin.sha256);
                    if !attempts.is_empty() {
                        line.push_str(&format!(
                            ", after {} failed: {}",
                            attempts.len(),
                            attempts.join("; ")
                        ));
                    }
                    return Ok(line);
                }
                Some(why) => attempts.push(format!("{url}: {why}")),
            }
        }
    }
    std::fs::remove_file(&part).map_err(write_error(&part))?;
    Err(Error::Unavailable {
        name: pin.name.clone(),
        attempts,
    })
}

/// One download of `url` into `part`: `None` when it holds the pinned bytes, else why not (one
/// line). An error is a `part` that could not be written.
fn attempt(url: &str, pin: &Pin, part: &Path, web: &dyn Web) -> Result<Option<String>, Error> {
    let file = File::create(part).map_err(write_error(part))?;
    file.set_permissions(std::fs::Permissions::from_mode(0o644))
        .map_err(write_error(part))?;
    let mut body = Hashed::new(file);
    let get = Get {
        url,
        accept: ACCEPT,
        if_none_match: None,
        limit: pin.bytes,
    };
    let head = match web.get(&get, &mut body) {
        Ok(head) => head,
        Err(e) => return Ok(Some(e.why.replace(['\r', '\n'], " "))),
    };
    if head.status != 200 {
        return Ok(Some(format!("HTTP {}", head.status)));
    }
    body.inner.sync_all().map_err(write_error(part))?;
    let (got, actual) = body.finish();
    Ok(match pin.bytes {
        Some(bytes) if bytes != got => Some(format!("{got} bytes, not the pinned {bytes}")),
        _ if actual != pin.sha256 => Some(format!("SHA-256 {actual}, not the pinned one")),
        _ => None,
    })
}

/// The [`Error::Write`] of `path`, for `map_err`.
fn write_error(path: &Path) -> impl FnOnce(io::Error) -> Error + '_ {
    move |e| Error::Write {
        path: path.to_owned(),
        why: e.to_string(),
    }
}

#[cfg(test)]
pub(crate) mod tests;
