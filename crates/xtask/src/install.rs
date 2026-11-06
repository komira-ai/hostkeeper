//! `install-tool --download <file> --sha256 <hex> --out <path> [--extract tar.gz --member <path>]`:
//! installs a pinned tool for a CI job from a file the job downloaded.
//!
//! The download itself is not done here. The workflow step before this one runs `curl` with
//! `--proto =https --proto-redir =https`, so the transfer and its redirects are HTTPS only, and
//! this verb has no network code. What it does, in this order:
//!
//! 1. The whole download is read into memory.
//! 2. Its SHA-256 is compared with `--sha256`. A mismatch is refused here, before any byte is
//!    unpacked or written: nothing is created on disk.
//! 3. It is unpacked in memory: as is, or one regular file (`--member`, its path inside the
//!    archive) out of a gzip tar (`--extract tar.gz`).
//! 4. The file is written to a temporary file of this invocation's own beside `--out`, created
//!    exclusively (an existing file or symbolic link of that name is never opened), made
//!    executable (0755) through its open handle, and renamed over `--out`. When a step fails,
//!    the temporary file is removed, so `--out` holds either the whole new file or exactly what
//!    it held before.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::Digest;

/// A SHA-256 digest, written as 64 lowercase hex digits.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Sha256([u8; 32]);

impl Sha256 {
    /// The digest of `bytes`.
    pub(crate) fn of(bytes: &[u8]) -> Sha256 {
        Sha256(sha2::Sha256::digest(bytes).into())
    }
}

impl fmt::Display for Sha256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

impl fmt::Debug for Sha256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// A `--sha256` value that is not 64 lowercase hex digits.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("not a SHA-256: {0:?} (expected 64 lowercase hex digits)")]
pub(crate) struct NotASha256(String);

impl FromStr for Sha256 {
    type Err = NotASha256;

    fn from_str(s: &str) -> Result<Sha256, NotASha256> {
        let mut digest = [0; 32];
        let lowercase = s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        match hex::decode_to_slice(s, &mut digest) {
            Ok(()) if lowercase => Ok(Sha256(digest)),
            _ => Err(NotASha256(s.to_owned())),
        }
    }
}

/// What `--extract` names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum Format {
    /// A gzip-compressed tar: the file is its `--member`.
    #[value(name = "tar.gz")]
    TarGz,
}

/// How the installed file comes out of the download.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Unpack {
    /// The download is the file.
    Raw,
    /// The download is a gzip-compressed tar archive; the file is the regular file at
    /// `member` in it.
    TarGz { member: PathBuf },
}

/// `--extract` and `--member` that do not fit together.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum UnpackFlags {
    #[error("--extract tar.gz needs --member, the file to take")]
    ExtractWithoutMember,
    #[error("--member is a file in a tar: it needs --extract tar.gz")]
    MemberWithoutExtract,
}

impl Unpack {
    /// `--extract` and `--member` as given.
    pub(crate) fn new(
        extract: Option<Format>,
        member: Option<PathBuf>,
    ) -> Result<Unpack, UnpackFlags> {
        match (extract, member) {
            (None, None) => Ok(Unpack::Raw),
            (Some(Format::TarGz), Some(member)) => Ok(Unpack::TarGz { member }),
            (Some(Format::TarGz), None) => Err(UnpackFlags::ExtractWithoutMember),
            (None, Some(_)) => Err(UnpackFlags::MemberWithoutExtract),
        }
    }

    /// The file this unpacking makes of `bytes`.
    fn apply(&self, bytes: &[u8]) -> Result<Vec<u8>, Error> {
        match self {
            Unpack::Raw => Ok(bytes.to_vec()),
            Unpack::TarGz { member } => {
                let mut tar = Vec::new();
                flate2::read::MultiGzDecoder::new(bytes)
                    .read_to_end(&mut tar)
                    .map_err(unpack_error)?;
                tar_member(&tar, member)
            }
        }
    }
}

/// Why an install failed.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum Error {
    #[error("read the download {path:?}: {why}")]
    Read { path: PathBuf, why: String },
    #[error("the download's SHA-256 is {actual}, not the pinned {expected}; nothing was installed")]
    Mismatch { expected: Sha256, actual: Sha256 },
    #[error("unpack the download: {0}")]
    Unpack(String),
    #[error("the archive has no {0:?}")]
    NoMember(PathBuf),
    #[error("{0:?} in the archive is not a regular file")]
    NotAFile(PathBuf),
    #[error("install {path:?}: {why}")]
    Install { path: PathBuf, why: String },
}

/// The error for an archive that could not be decoded.
fn unpack_error(e: impl fmt::Display) -> Error {
    Error::Unpack(e.to_string())
}

/// Checks `bytes` against `sha256` and unpacks them: the file, written nowhere.
pub(crate) fn checked(bytes: &[u8], sha256: &Sha256, unpack: &Unpack) -> Result<Vec<u8>, Error> {
    let actual = Sha256::of(bytes);
    if actual != *sha256 {
        return Err(Error::Mismatch {
            expected: *sha256,
            actual,
        });
    }
    unpack.apply(bytes)
}

/// Checks `bytes` against `sha256`, unpacks them and installs the file at `out`, through the
/// temporary file numbered `n`.
fn install_as(
    bytes: &[u8],
    sha256: &Sha256,
    unpack: &Unpack,
    out: &Path,
    n: u64,
) -> Result<(), Error> {
    let file = checked(bytes, sha256, unpack)?;
    write_executable(out, &file, n).map_err(|e| Error::Install {
        path: out.to_owned(),
        why: e.to_string(),
    })
}

/// Numbers the temporary files of this process, so two installs never share one.
static NEXT: AtomicU64 = AtomicU64::new(0);

/// Checks `bytes` against `sha256`, unpacks them and installs the file at `out`.
pub(crate) fn install(
    bytes: &[u8],
    sha256: &Sha256,
    unpack: &Unpack,
    out: &Path,
) -> Result<(), Error> {
    install_as(
        bytes,
        sha256,
        unpack,
        out,
        NEXT.fetch_add(1, Ordering::Relaxed),
    )
}

/// Reads the file `download` and [`install`]s it at `out`: the line to print.
pub(crate) fn install_tool(
    download: &Path,
    sha256: &Sha256,
    unpack: &Unpack,
    out: &Path,
) -> Result<String, Error> {
    let bytes = std::fs::read(download).map_err(|e| Error::Read {
        path: download.to_owned(),
        why: e.to_string(),
    })?;
    install(&bytes, sha256, unpack, out)?;
    Ok(format!(
        "ok: {} has SHA-256 {sha256}; installed {}",
        download.display(),
        out.display()
    ))
}

/// The temporary file number `n` of this process for `out`: hidden, beside it, so the rename
/// stays on one file system.
fn part_path(out: &Path, n: u64) -> PathBuf {
    let name = out.file_name().unwrap_or_default().to_string_lossy();
    out.with_file_name(format!(".{name}.{}.{n}.part", std::process::id()))
}

/// Writes `data` to `out` (mode 0755) through the temporary file [`part_path`] `n`, removing
/// it when a later step fails.
fn write_executable(out: &Path, data: &[u8], n: u64) -> std::io::Result<()> {
    out.parent().map_or(Ok(()), std::fs::create_dir_all)?;
    let part = part_path(out, n);
    // `create_new` is O_CREAT|O_EXCL: an existing file or symbolic link at `part` is an error,
    // never followed or truncated.
    let mut file: File = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&part)?;
    let written = file
        .write_all(data)
        .and_then(|()| file.set_permissions(std::fs::Permissions::from_mode(0o755)))
        .and_then(|()| file.sync_all())
        .and_then(|()| std::fs::rename(&part, out));
    if written.is_err() {
        // A temporary file that cannot be removed is reported by the write error itself.
        let _ = std::fs::remove_file(&part);
    }
    written
}

/// The contents of the regular file at `member` in the tar archive `tar`.
fn tar_member(tar: &[u8], member: &Path) -> Result<Vec<u8>, Error> {
    let mut archive = tar::Archive::new(tar);
    for entry in archive.entries().map_err(unpack_error)? {
        let mut entry = entry.map_err(unpack_error)?;
        if entry.path().map_err(unpack_error)? != member {
            continue;
        }
        if !entry.header().entry_type().is_file() {
            return Err(Error::NotAFile(member.to_owned()));
        }
        let size = entry.size();
        let mut file = Vec::new();
        entry.read_to_end(&mut file).map_err(unpack_error)?;
        // A tar cut short inside the member reads fewer bytes than its header names.
        if file.len() as u64 != size {
            return Err(Error::Unpack(format!(
                "{member:?} is cut short: {} of its {size} bytes",
                file.len()
            )));
        }
        return Ok(file);
    }
    Err(Error::NoMember(member.to_owned()))
}

#[cfg(test)]
mod tests;
