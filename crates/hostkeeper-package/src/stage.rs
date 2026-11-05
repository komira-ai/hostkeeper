//! The staging file and the one download loop that fills it, shared by the archive and the
//! attestation bundle.
//!
//! A staging file is `packages/<service>/.fetch-<pid>-<n>` (the bundle's with `.json` after it): created exclusively (an existing
//! name, a link included, is never opened), never through a link, mode 0600, and removed when
//! its [`TempFile`] is dropped, so every path that does not hand it on leaves nothing behind.
//! The bytes are counted and hashed in the pass that writes them.

use std::fs::{self, DirBuilder, File, OpenOptions, Permissions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use hostkeeper_steps::{Clock, Stamp};
use sha2::{Digest, Sha256};

use crate::ports::{Fetcher, Got, Limit, Sink, Stop, Url, remaining_secs};

/// The prefix of every staging file's name.
pub(crate) const PREFIX: &str = ".fetch-";
/// The longest a fetch waits to connect (design section 5.5, row "1 Download").
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// How many names a staging file is tried under before the fetch gives up.
const NAME_TRIES: u64 = 16;

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A file this crate made, removed when the value is dropped.
#[derive(Debug)]
pub(crate) struct TempFile {
    path: PathBuf,
}

impl TempFile {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        // A file that cannot be removed now is an orphan: `remove_orphan_fetches` removes it
        // before the next fetch of the service and at start.
        let _ = fs::remove_file(&self.path);
    }
}

/// Whether `dir` is a directory: `Ok(false)` when nothing is there, an error when it is
/// anything but a directory (a link to one included) or when it is writable by its group or by
/// others (anyone who can write there can replace a staging file by name).
pub(crate) fn is_dir(dir: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(dir) {
        Ok(meta) if meta.is_dir() & (meta.permissions().mode() & 0o022 != 0) => Err(
            io::Error::other(format!("{} is writable by group or others", dir.display())),
        ),
        Ok(meta) if meta.is_dir() => Ok(true),
        Ok(_) => Err(io::Error::other(format!(
            "{} is not a directory",
            dir.display()
        ))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Makes `dir` (one level, mode 0755) unless it exists, and checks that it is a directory and
/// not a link.
pub(crate) fn make_dir(dir: &Path) -> io::Result<()> {
    if !is_dir(dir)? {
        DirBuilder::new().mode(0o755).create(dir)?;
    }
    is_dir(dir).map(|_| ())
}

/// The names a staging file is tried under, in order. A list, not an iterator, so that
/// [`create`] has one instantiation whatever its caller.
pub(crate) fn names() -> Vec<String> {
    names_with("")
}

/// [`names`], each ending in `suffix`: the bundle's end in `.json`, because `gh` reads the
/// extension of the file it is given to choose its parser. Every name still begins with
/// [`PREFIX`], which is what the orphan sweep looks for.
pub(crate) fn names_with(suffix: &str) -> Vec<String> {
    let pid = std::process::id();
    (0..NAME_TRIES)
        .map(|_| {
            format!(
                "{PREFIX}{pid}-{}{suffix}",
                NEXT.fetch_add(1, Ordering::Relaxed)
            )
        })
        .collect()
}

/// Creates the first of `names` that does not exist in `dir`, exclusively and without following
/// a link, mode 0600. A name that exists in any form is skipped; when every name exists the
/// error is `AlreadyExists`, and nothing was opened.
pub(crate) fn create(dir: &Path, names: Vec<String>) -> io::Result<(File, TempFile)> {
    let mut last = io::Error::from(io::ErrorKind::AlreadyExists);
    for name in names {
        let path = dir.join(name);
        let opened = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path);
        match opened {
            Ok(file) => {
                let temp = TempFile { path };
                // The mode is exact whatever the umask.
                file.set_permissions(Permissions::from_mode(0o600))?;
                return Ok((file, temp));
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => last = e,
            Err(e) => return Err(e),
        }
    }
    Err(last)
}

/// Why a download did not produce a file.
#[derive(Debug)]
pub(crate) enum Fault {
    /// The announced length or the bytes received passed the cap.
    TooLarge,
    /// The deadline passed.
    Deadline,
    /// The network failed.
    Net,
    /// The staging file could not be made or written.
    Storage(io::Error),
    /// The adapter said the sink stopped it, and the sink did not.
    Unexplained,
}

/// What one download is: from where, into which directory, how many bytes at most, by when.
pub(crate) struct Job<'a> {
    pub fetcher: &'a dyn Fetcher,
    pub clock: &'a dyn Clock,
    pub dir: &'a Path,
    pub url: &'a Url,
    /// What the staging file's name ends in: `""`, or `.json` for a bundle.
    pub suffix: &'static str,
    pub cap: u64,
    pub deadline: Stamp,
}

/// A downloaded file: its bytes' SHA-256 (lower-case hex) and count.
#[derive(Debug)]
pub(crate) struct Download {
    pub file: TempFile,
    pub sha256: String,
    pub size: u64,
}

/// How the staging file is written to: the file itself, or in a test a writer that fails.
pub(crate) type OpenWriter = for<'a> fn(&'a File) -> Box<dyn Write + 'a>;

/// The staging file itself.
pub(crate) fn plain(file: &File) -> Box<dyn Write + '_> {
    Box::new(file)
}

/// The sink of a download: counts, hashes and writes each chunk, and stops the transfer at the
/// first chunk past the cap or the deadline, or the first write that fails. It keeps the first
/// reason it stopped for.
struct Staging<'a> {
    out: Box<dyn Write + 'a>,
    hasher: Sha256,
    count: u64,
    cap: u64,
    clock: &'a dyn Clock,
    deadline: &'a Stamp,
    fault: Option<Fault>,
}

impl Staging<'_> {
    fn stop(&mut self, fault: Fault) -> Result<(), Stop> {
        self.fault = self.fault.take().or(Some(fault));
        Err(Stop)
    }
}

impl Sink for Staging<'_> {
    fn begin(&mut self, content_length: Option<u64>) -> Result<(), Stop> {
        if content_length.is_some_and(|length| length > self.cap) {
            return self.stop(Fault::TooLarge);
        }
        Ok(())
    }

    fn chunk(&mut self, bytes: &[u8]) -> Result<(), Stop> {
        if remaining_secs(self.deadline, self.clock) == 0 {
            return self.stop(Fault::Deadline);
        }
        let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        self.count = self.count.saturating_add(length);
        if self.count > self.cap {
            return self.stop(Fault::TooLarge);
        }
        self.hasher.update(bytes);
        if let Err(e) = self.out.write_all(bytes) {
            return self.stop(Fault::Storage(e));
        }
        Ok(())
    }
}

/// Downloads `job.url` into a new staging file in `job.dir`, written through `writer`. On any
/// fault the staging file is removed before this returns.
pub(crate) fn download(job: &Job<'_>, writer: OpenWriter) -> Result<Download, Fault> {
    let (file, temp) = create(job.dir, names_with(job.suffix)).map_err(Fault::Storage)?;
    let mut sink = Staging {
        out: writer(&file),
        hasher: Sha256::new(),
        count: 0,
        cap: job.cap,
        clock: job.clock,
        deadline: &job.deadline,
        fault: None,
    };
    let limit = Limit {
        connect_timeout: CONNECT_TIMEOUT,
        deadline: job.deadline.clone(),
    };
    let got = job.fetcher.get(job.url, &mut sink, limit);
    let Staging {
        hasher,
        count,
        fault,
        ..
    } = sink;
    match (got, fault) {
        (_, Some(fault)) => Err(fault),
        (Err(_), None) => Err(Fault::Net),
        (Ok(Got::Stopped), None) => Err(Fault::Unexplained),
        (Ok(Got::Complete), None) => {
            file.sync_all().map_err(Fault::Storage)?;
            // After the sync, which can take as long as the transfer: the file is only returned
            // when there is time left to use it.
            if remaining_secs(&job.deadline, job.clock) == 0 {
                return Err(Fault::Deadline);
            }
            Ok(Download {
                file: temp,
                sha256: hex::encode(&*hasher.finalize()),
                size: count,
            })
        }
    }
}

/// The bytes of the staging file at `path`, at most `cap` of them, read without following a
/// link.
pub(crate) fn read_back(path: &Path, cap: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?
        .take(cap)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Whether `e` is a full disk (`ENOSPC`). A quota that is reached (`EDQUOT`) is not: it is
/// `fetch_failed`, as the design names `ENOSPC` alone for `no_space`.
pub(crate) fn is_full(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::StorageFull
}

#[cfg(test)]
#[path = "stage_tests.rs"]
mod tests;
