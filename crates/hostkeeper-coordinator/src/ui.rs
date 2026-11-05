//! The static files of `--ui-dir`, served under [`PREFIX`] on the private listener.
//!
//! This is a file server and nothing else: it parses no token, holds no state between requests,
//! builds no page and caches nothing. The operator web app it serves is plain files that call the
//! operator API on the same listener with the operator's token, so the app is same-origin and
//! needs no cross-origin access, no cookie and no session (`docs/ui-serving.md`).
//!
//! A request is answered from the directory only when every part of the path below
//! [`PREFIX`] is one plain name: after percent-decoding, a part that is empty (`//`, an
//! absolute path), that starts with `.` (`.`, `..`, a hidden file) or that holds `/`, `\` or
//! NUL is not found, as is a part that is not percent-encoded correctly or not UTF-8. A path
//! ending in `/` is the `index.html` of that directory. The name's extension picks the content
//! type from a fixed table ([`content_type`]); any other extension is not found. The file is
//! then resolved with every symbolic link followed, and one whose real path is outside the
//! directory, or that is not a regular file, or that is larger than [`MAX_FILE_BYTES`], is not
//! found and is never opened. A file is read whole on each request.
//!
//! Every answer, the not-found and method-not-allowed ones included, carries
//! `Cache-Control: no-store`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`
//! and [`CONTENT_SECURITY_POLICY`], and no `Access-Control-*` header. `GET` and `HEAD` are the
//! only methods; `HEAD` has the headers of `GET` and no body.

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::Full;
use hyper::header::{
    CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, REFERRER_POLICY, X_CONTENT_TYPE_OPTIONS,
};
use hyper::{Method, Response, StatusCode};

/// The path every file is served under: `/ui/` itself is the directory's `index.html`.
pub(crate) const PREFIX: &str = "/ui/";

/// The largest file served, in bytes; a larger one is not found.
pub(crate) const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// The `Content-Security-Policy` of every answer: the page loads only what its own origin
/// serves, and no other page may frame it.
pub(crate) const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; frame-ancestors 'none'";

/// The extensions served, and the content type of each. Matched exactly: `INDEX.HTML` is not
/// found.
const TYPES: [(&str, &str); 7] = [
    ("html", "text/html; charset=utf-8"),
    ("css", "text/css; charset=utf-8"),
    ("js", "text/javascript; charset=utf-8"),
    ("json", "application/json"),
    ("svg", "image/svg+xml"),
    ("png", "image/png"),
    ("ico", "image/x-icon"),
];

/// How a file the directory resolves to is opened: `File::open`, or a test's port that counts
/// what it is asked to open.
pub(crate) type Open = Arc<dyn Fn(&Path) -> io::Result<File> + Send + Sync>;

/// Why `--ui-dir` is refused at start.
#[derive(Debug, thiserror::Error)]
pub enum UiDirError {
    /// The path cannot be resolved: it is missing, or a part of it cannot be read.
    #[error("{path}: {source}")]
    Unreadable {
        /// The path given.
        path: PathBuf,
        /// Why it cannot be resolved.
        #[source]
        source: io::Error,
    },
    /// The path is not a directory.
    #[error("{0} is not a directory")]
    NotADirectory(PathBuf),
}

/// The directory of `--ui-dir`: its real path, resolved once at start, and how a file in it
/// is opened.
pub struct UiDir {
    root: PathBuf,
    open: Open,
}

impl UiDir {
    /// The directory at `path`, with every symbolic link in it resolved.
    ///
    /// # Errors
    /// [`UiDirError`]: `path` does not exist or cannot be resolved, or it is not a directory.
    pub fn new(path: &Path) -> Result<Self, UiDirError> {
        Self::with_open(path, Arc::new(open_file))
    }

    /// [`Self::new`], with files opened by `open`.
    pub(crate) fn with_open(path: &Path, open: Open) -> Result<Self, UiDirError> {
        let root = std::fs::canonicalize(path).map_err(|source| UiDirError::Unreadable {
            path: path.to_owned(),
            source,
        })?;
        if !root.is_dir() {
            return Err(UiDirError::NotADirectory(path.to_owned()));
        }
        Ok(Self { root, open })
    }

    /// The directory's real path.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The answer to `method` for `rest`, the request's path after [`PREFIX`] (still
    /// percent-encoded, without the query).
    pub(crate) async fn answer(&self, method: &Method, rest: &str) -> Response<Full<Bytes>> {
        let head = *method == Method::HEAD;
        if !head && *method != Method::GET {
            return refusal(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed", head);
        }
        let Some((relative, kind)) = resolve(rest) else {
            return refusal(StatusCode::NOT_FOUND, "not_found", head);
        };
        let root = self.root.clone();
        let open = Arc::clone(&self.open);
        // File-system calls and a read of up to 4 MiB: off the runtime's threads.
        let read = tokio::task::spawn_blocking(move || read_inside(&root, &relative, &open))
            .await
            .ok()
            .flatten();
        match read {
            Some(bytes) => respond(StatusCode::OK, kind, Bytes::from(bytes), head),
            None => refusal(StatusCode::NOT_FOUND, "not_found", head),
        }
    }
}

/// Opens the file at `real`, read-only: how a file is opened outside the tests.
fn open_file(real: &Path) -> io::Result<File> {
    File::open(real)
}

/// The path below the directory that `rest` names, and its content type; `None` when `rest`
/// is not a path of plain names with an extension of [`TYPES`].
fn resolve(rest: &str) -> Option<(PathBuf, &'static str)> {
    let mut parts: Vec<&str> = rest.split('/').collect();
    let last = parts.pop().unwrap_or_default();
    let mut relative = PathBuf::new();
    for part in parts {
        relative.push(name(part)?);
    }
    let file = if last.is_empty() {
        "index.html".to_owned()
    } else {
        name(last)?
    };
    let kind = content_type(&file)?;
    relative.push(file);
    Some((relative, kind))
}

/// One part of a path, percent-decoded, if it is a plain name: not empty, not starting with
/// `.`, and holding no `/`, `\` or NUL.
fn name(raw: &str) -> Option<String> {
    let decoded = percent_decoded(raw)?;
    let plain =
        !decoded.is_empty() && !decoded.starts_with('.') && !decoded.contains(['/', '\\', '\0']);
    plain.then_some(decoded)
}

/// `raw` with each `%` and two hex digits replaced by the byte they spell, if every `%` is
/// followed by two hex digits and the result is UTF-8.
fn percent_decoded(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let digit = |i: usize| bytes.get(i).and_then(|b| char::from(*b).to_digit(16));
            let byte = digit(at + 1)
                .zip(digit(at + 2))
                .map(|(high, low)| high * 16 + low)?;
            out.push(u8::try_from(byte).unwrap_or_default());
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The content type of the file `name`, from its extension.
fn content_type(name: &str) -> Option<&'static str> {
    let (_, extension) = name.rsplit_once('.')?;
    TYPES
        .iter()
        .find(|(known, _)| *known == extension)
        .map(|(_, kind)| *kind)
}

/// The bytes of `root/relative`, if its real path (every symbolic link followed) is inside
/// `root` and is a regular file of at most [`MAX_FILE_BYTES`]. Nothing outside `root` is
/// opened.
fn read_inside(root: &Path, relative: &Path, open: &Open) -> Option<Vec<u8>> {
    let real = std::fs::canonicalize(root.join(relative)).ok()?;
    if !real.starts_with(root) {
        return None;
    }
    // Looked at before it is opened: a FIFO or a device is never opened, and a file too
    // large is never read.
    let meta = std::fs::metadata(&real).ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    (**open)(&real)
        .ok()?
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    u64::try_from(bytes.len())
        .is_ok_and(|n| n <= MAX_FILE_BYTES)
        .then_some(bytes)
}

/// An error answer `{"error":"<code>"}` with the headers of every answer.
fn refusal(status: StatusCode, code: &'static str, head: bool) -> Response<Full<Bytes>> {
    let body = format!("{{\"error\":\"{code}\"}}");
    respond(status, "application/json", Bytes::from(body), head)
}

/// An answer of `status` with `body` of `kind`, the headers of every answer, and no body when
/// it answers a `HEAD` (its `Content-Length` is still the body's).
fn respond(
    status: StatusCode,
    kind: &'static str,
    body: Bytes,
    head: bool,
) -> Response<Full<Bytes>> {
    let length = body.len();
    let body = if head { Bytes::new() } else { body };
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, kind)
        .header(CONTENT_LENGTH, length)
        .header(CACHE_CONTROL, "no-store")
        .header(X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(REFERRER_POLICY, "no-referrer")
        .header(
            hyper::header::CONTENT_SECURITY_POLICY,
            CONTENT_SECURITY_POLICY,
        )
        .body(Full::new(body))
        .unwrap_or_default()
}
