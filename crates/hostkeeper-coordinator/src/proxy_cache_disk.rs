//! The package proxy cache's directory (`docs/design/package-proxy.md`, section 4): where a
//! committed object and its sidecar live, and what a start finds there.
//!
//! ```text
//! <--proxy-cache-dir>/tmp/<digest>.<n>     a fill in progress
//! <--proxy-cache-dir>/objects/<digest>     a committed body
//! <--proxy-cache-dir>/objects/<digest>.meta  its sidecar (JSON: the key, the validator, the
//!                                          fetch time, the length)
//! ```
//!
//! `<digest>` is the SHA-256 of the key (the origin and the path) in lowercase hex. A fill is
//! written to `tmp/` and renamed into `objects/` on the same filesystem, the sidecar first and the
//! body last, so a body in `objects/` is always whole. Nothing is synced to disk: a cache fill is
//! never worth the wait, so after a crash a start looks for what a crash leaves ([`scan`]): every
//! file of `tmp/`, a body whose length is not its sidecar's, and a sidecar with no body or no
//! matching key are deleted, and the rest is the cache.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The largest sidecar read: a sidecar is a few hundred bytes (a key and a validator).
const MAX_SIDECAR: u64 = 64 * 1024;

/// What a cached object is: the origin that gave it and its path under that origin.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    /// The origin: a repository's name, or the host a pkg is read from.
    pub origin: String,
    /// The path under the origin, as the origin spells it.
    pub path: String,
}

impl Key {
    /// A key.
    #[must_use]
    pub fn new(origin: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            origin: origin.into(),
            path: path.into(),
        }
    }

    /// The SHA-256 of the key in lowercase hex: the name of the object's files.
    #[must_use]
    pub fn digest(&self) -> String {
        let mut hash = Sha256::new();
        hash.update(self.origin.as_bytes());
        hash.update([0]);
        hash.update(self.path.as_bytes());
        hex::encode(hash.finalize())
    }
}

/// What the cache knows of a committed object besides its bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    /// The origin's validator (its `ETag`, else its `Last-Modified`), for a conditional request.
    pub validator: Option<String>,
    /// When the object was fetched, in unix seconds.
    pub fetched_at: u64,
    /// The length of the body in bytes.
    pub length: u64,
}

/// The sidecar file: the key besides the meta, so a start can tell a sidecar from another's.
#[derive(Serialize, Deserialize)]
struct Sidecar {
    origin: String,
    path: String,
    #[serde(flatten)]
    meta: Meta,
}

/// The two directories of the cache.
#[derive(Debug)]
pub(crate) struct Layout {
    objects: PathBuf,
    tmp: PathBuf,
}

impl Layout {
    /// The layout under `dir`, whose `objects/` and `tmp/` are made when missing.
    pub(crate) fn create(dir: &Path) -> io::Result<Self> {
        let layout = Self {
            objects: dir.join("objects"),
            tmp: dir.join("tmp"),
        };
        fs::create_dir_all(&layout.objects)?;
        fs::create_dir_all(&layout.tmp)?;
        Ok(layout)
    }

    /// The committed body of `digest`.
    pub(crate) fn object(&self, digest: &str) -> PathBuf {
        self.objects.join(digest)
    }

    /// The sidecar of `digest`.
    pub(crate) fn meta(&self, digest: &str) -> PathBuf {
        self.objects.join(format!("{digest}.meta"))
    }

    /// A file of `tmp/` for the `n`th fill of `digest`.
    pub(crate) fn tmp(&self, digest: &str, n: u64) -> PathBuf {
        self.tmp.join(format!("{digest}.{n}"))
    }

    /// Makes the files of a fill whole: the sidecar for `key` is written to `tmp_meta` and
    /// renamed, then the body `tmp_body` is renamed to its place. A failure leaves no sidecar
    /// that names a body this call did not place.
    pub(crate) fn commit(
        &self,
        key: &Key,
        meta: &Meta,
        tmp_body: &Path,
        tmp_meta: &Path,
    ) -> io::Result<()> {
        let digest = key.digest();
        let text = serde_json::to_vec(&Sidecar {
            origin: key.origin.clone(),
            path: key.path.clone(),
            meta: meta.clone(),
        })
        .map_err(io::Error::other)?;
        fs::write(tmp_meta, text)?;
        fs::rename(tmp_meta, self.meta(&digest))?;
        if let Err(why) = fs::rename(tmp_body, self.object(&digest)) {
            self.remove(&digest);
            return Err(why);
        }
        Ok(())
    }

    /// Deletes the files of `digest`, the body first; a file that is not there is no error.
    pub(crate) fn remove(&self, digest: &str) {
        for path in [self.object(digest), self.meta(digest)] {
            if let Err(why) = fs::remove_file(&path)
                && why.kind() != io::ErrorKind::NotFound
            {
                tracing::warn!(error = %why, path = %path.display(), "the proxy cache could not delete a file");
            }
        }
    }
}

/// An object a start found committed.
#[derive(Debug)]
pub(crate) struct Found {
    /// The digest of its key.
    pub(crate) digest: String,
    /// Its sidecar. The fetch time is the order a start gives the objects their recency: a hit
    /// is remembered only while the coordinator runs.
    pub(crate) meta: Meta,
}

/// What a start does to the directory: the files of `tmp/` are deleted, a partial object is
/// deleted with its sidecar, and the committed objects are returned, oldest first.
///
/// # Errors
/// The directories cannot be read.
pub(crate) fn scan(layout: &Layout) -> io::Result<Vec<Found>> {
    for entry in fs::read_dir(&layout.tmp)? {
        let path = entry?.path();
        if let Err(why) = fs::remove_file(&path) {
            tracing::warn!(error = %why, path = %path.display(), "the proxy cache could not delete a partial fill");
        }
    }
    let mut digests = Vec::new();
    for entry in fs::read_dir(&layout.objects)? {
        let name = entry?.file_name();
        let name = name.to_string_lossy();
        let digest = name.strip_suffix(".meta").unwrap_or(&name);
        if is_digest(digest) && !digests.iter().any(|seen| seen == digest) {
            digests.push(digest.to_owned());
        }
    }
    let mut found = Vec::new();
    for digest in digests {
        match read_committed(layout, &digest) {
            Some(one) => found.push(one),
            None => layout.remove(&digest),
        }
    }
    found.sort_by(|a, b| (a.meta.fetched_at, &a.digest).cmp(&(b.meta.fetched_at, &b.digest)));
    Ok(found)
}

/// The object `digest` if its body and its sidecar agree: the sidecar parses, names a key whose
/// digest this is, and gives the body's length.
fn read_committed(layout: &Layout, digest: &str) -> Option<Found> {
    let body = fs::metadata(layout.object(digest)).ok()?;
    let meta_path = layout.meta(digest);
    if fs::metadata(&meta_path).ok()?.len() > MAX_SIDECAR {
        return None;
    }
    let sidecar: Sidecar = serde_json::from_slice(&fs::read(meta_path).ok()?).ok()?;
    let named = Key::new(sidecar.origin, sidecar.path).digest();
    (body.is_file() && named == digest && body.len() == sidecar.meta.length).then(|| Found {
        digest: digest.to_owned(),
        meta: sidecar.meta,
    })
}

/// Whether `text` is a SHA-256 in lowercase hex.
fn is_digest(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
#[path = "proxy_cache_disk_tests.rs"]
mod tests;
