//! Where hostkeeper's pkg is, and the fetch of it (docs/design/mac-bootstrap.md, sections 3 and
//! 4), over the repository's own formats and the daemon's own transport:
//!
//! 1. `<base>/hostkeeper-repository.json`, the descriptor, read by
//!    [`hostkeeper_release::descriptor::check`]; its `macos-pkg` format says the directory of the
//!    installers and the platforms they are built for ([`PLATFORM`] must be one).
//! 2. `<base>/<path>/<dir>/stable.json`, the pointer: `{"version", "files": {<name>: {"sha256",
//!    "size"}}}`. The file is `hostkeeper-<version>-macos-arm64.pkg`.
//! 3. `<base>/<path>/<dir>/<version>/<file>`, the pkg, written to the staging file as it arrives
//!    and hashed in the same pass, with no whole-file buffer. The size and the SHA-256 must be the
//!    pointer's, which guards against a cut or corrupted transfer; it is the same origin as the
//!    file, so the signer check is what decides.

use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::Duration;

use hostkeeper_package::net::{Timeouts, Transport};
use hostkeeper_package::ports::NetError;
use hostkeeper_release::descriptor::{self, DescriptorError};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::settings::Settings;

/// The platform of the pkg the bootstrap installs.
pub const PLATFORM: &str = "macos-arm64";
/// The name of hostkeeper's package in the repository's `macos-pkg` format.
pub const PACKAGE: &str = "hostkeeper";
/// The pointer's file name, the repository's `stable` channel.
pub const POINTER: &str = "stable.json";
/// The most bytes of the pointer.
pub const MAX_POINTER_BYTES: u64 = 64 * 1024;
/// The most bytes of one read of a body.
const CHUNK: usize = 64 * 1024;

/// Why the pkg could not be found or fetched.
#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    #[error("GET {url}: {source}")]
    Net {
        url: String,
        #[source]
        source: NetError,
    },
    #[error("GET {url} answered {status}")]
    Status { url: String, status: u16 },
    #[error("GET {url}: the body is larger than {limit} bytes")]
    TooLarge { url: String, limit: u64 },
    #[error("GET {url}: the body could not be read: {source}")]
    Read {
        url: String,
        #[source]
        source: io::Error,
    },
    #[error("the repository descriptor is refused: {0}")]
    Descriptor(#[from] DescriptorError),
    #[error("the repository has no macos-pkg format for {PLATFORM}")]
    NoMacosPkg,
    #[error("the pointer {url} is not what a pointer is: {why}")]
    Pointer { url: String, why: String },
    #[error("the pkg is {found} bytes, the pointer says {wanted}")]
    Size { found: u64, wanted: u64 },
    #[error("the pkg's SHA-256 is {found}, the pointer says {wanted}")]
    Digest { found: String, wanted: String },
    #[error("the staging file {path}: {source}")]
    Staging {
        path: String,
        #[source]
        source: io::Error,
    },
}

/// The pkg the repository's pointer names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Located {
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

fn timeouts(settings: &Settings, total: u64) -> Timeouts {
    Timeouts {
        connect: Duration::from_secs(settings.bounds.connect_timeout_secs),
        total: Duration::from_secs(total),
    }
}

/// Counts and hashes what passes through to `inner`.
struct Tee<W> {
    inner: W,
    hasher: Sha256,
    count: u64,
}

impl<W: Write> Tee<W> {
    fn put(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.inner.write_all(bytes)?;
        self.hasher.update(bytes);
        self.count += bytes.len() as u64;
        Ok(())
    }
}

/// One GET of `url` into `out`, at most `limit` bytes; the bytes written and their SHA-256 as
/// lower-case hex.
pub fn fetch(
    transport: &dyn Transport,
    url: &str,
    timeouts: Timeouts,
    limit: u64,
    out: &mut dyn Write,
) -> Result<(u64, String), PackageError> {
    let response = transport
        .open(url, timeouts)
        .map_err(|source| PackageError::Net {
            url: url.to_owned(),
            source,
        })?;
    if response.status != 200 {
        return Err(PackageError::Status {
            url: url.to_owned(),
            status: response.status,
        });
    }
    let too_large = || PackageError::TooLarge {
        url: url.to_owned(),
        limit,
    };
    if response.content_length.is_some_and(|length| length > limit) {
        return Err(too_large());
    }
    let mut tee = Tee {
        inner: out,
        hasher: Sha256::new(),
        count: 0,
    };
    let mut body = response.body.take(limit + 1);
    let mut buffer = vec![0u8; CHUNK];
    loop {
        let read = body
            .read(&mut buffer)
            .map_err(|source| PackageError::Read {
                url: url.to_owned(),
                source,
            })?;
        if read == 0 {
            break;
        }
        tee.put(&buffer[..read])
            .map_err(|source| PackageError::Read {
                url: url.to_owned(),
                source,
            })?;
        if tee.count > limit {
            return Err(too_large());
        }
    }
    Ok((tee.count, hex::encode(tee.hasher.finalize())))
}

/// A small body, whole.
fn get_small(
    transport: &dyn Transport,
    url: &str,
    timeouts: Timeouts,
    limit: u64,
) -> Result<Vec<u8>, PackageError> {
    let mut bytes = Vec::new();
    fetch(transport, url, timeouts, limit, &mut bytes)?;
    Ok(bytes)
}

/// Reads the descriptor and the pointer, and names the pkg.
pub fn locate(transport: &dyn Transport, settings: &Settings) -> Result<Located, PackageError> {
    let base = &settings.release_channel;
    let small = timeouts(settings, settings.bounds.fetch_timeout_secs);
    let bytes = get_small(
        transport,
        &format!("{base}/{}", descriptor::FILE),
        small,
        descriptor::MAX_BYTES as u64,
    )?;
    let described = descriptor::check(&bytes)?;
    let format = described
        .macos_pkg
        .filter(|format| format.platforms.iter().any(|p| p == PLATFORM))
        .ok_or(PackageError::NoMacosPkg)?;
    let dir = format!("{base}/{}/{}", format.path, format.dir(PACKAGE));
    let pointer_url = format!("{dir}/{POINTER}");
    let pointer = get_small(transport, &pointer_url, small, MAX_POINTER_BYTES)?;
    let bad = |why: &str| PackageError::Pointer {
        url: pointer_url.clone(),
        why: why.to_owned(),
    };
    let value: Value = serde_json::from_slice(&pointer).map_err(|e| bad(&e.to_string()))?;
    let version = value
        .get("version")
        .and_then(Value::as_str)
        .filter(|v| is_version(v))
        .ok_or_else(|| bad("version is missing or is not a version"))?;
    let name = format!("{PACKAGE}-{version}-{PLATFORM}.pkg");
    let entry = value
        .get("files")
        .and_then(|files| files.get(&name))
        .ok_or_else(|| bad(&format!("files lists no {name}")))?;
    let sha256 = entry
        .get("sha256")
        .and_then(Value::as_str)
        .filter(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| bad("the file's sha256 is missing or is not 64 hex digits"))?
        .to_ascii_lowercase();
    let size = entry
        .get("size")
        .and_then(Value::as_u64)
        .ok_or_else(|| bad("the file's size is missing or is not a whole number"))?;
    Ok(Located {
        version: version.to_owned(),
        url: format!("{dir}/{version}/{name}"),
        sha256,
        size,
    })
}

/// Whether `text` can be one path component of a version.
fn is_version(text: &str) -> bool {
    (1..=64).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() | matches!(b, b'.' | b'-' | b'+' | b'_'))
}

/// Downloads `located` to the new file `staging` (mode 0600; `create_new` never replaces a file and never follows a link) and holds
/// it to the pointer's size and digest. On a mismatch the file is left for the caller to remove.
pub fn download(
    transport: &dyn Transport,
    settings: &Settings,
    located: &Located,
    staging: &Path,
) -> Result<(), PackageError> {
    let limit = settings.bounds.max_package_bytes.min(located.size);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(staging)
        .map_err(|source| PackageError::Staging {
            path: staging.display().to_string(),
            source,
        })?;
    let (size, digest) = fetch(
        transport,
        &located.url,
        timeouts(settings, settings.bounds.fetch_timeout_secs),
        limit,
        &mut file,
    )?;
    if size != located.size {
        return Err(PackageError::Size {
            found: size,
            wanted: located.size,
        });
    }
    if digest != located.sha256 {
        return Err(PackageError::Digest {
            found: digest,
            wanted: located.sha256.clone(),
        });
    }
    Ok(())
}
