//! `netboot-iso --pins <pins.toml> --platform <platform> --dir <dir> [--mirror <base>]...`: the
//! Ubuntu live-server ISO of a boot set of network install, fetched for the network-install jobs
//! of `net-install.yml` by its pin in `packaging/netboot/pins.toml` (the `url`, `bytes` and
//! `sha256` of `[<platform>.iso]`), from that URL or a mirror of it.
//!
//! A mirror is a base URL that stands for the pinned URL's `https://<host>`: the ISO
//! `https://releases.ubuntu.com/24.04/<iso>` is
//! `https://mirrors.edge.kernel.org/ubuntu-releases/24.04/<iso>` on the mirror
//! `https://mirrors.edge.kernel.org/ubuntu-releases`. The pinned URL is tried first, then each
//! mirror in the order given (`mirrored::fetch`: bounded rounds, the size and SHA-256 checked,
//! a `.part` file renamed into place), so an outage of Canonical's names does not stop the job.
//!
//! A file of the ISO's name already in the directory (a build cache restored it) is kept when
//! its size and SHA-256 are the pin's, and fetched again over it when they are not. The test
//! checks the ISO against the same pin again (`pxe update-hook prepare`).
//!
//! The ISO has no actions-cache entry of its own: the two ISOs are about 8 GB, most of what a
//! repository's actions cache holds. A directory under `target/` may still hold it from an
//! earlier run when a job's build cache restored it.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::coordinator_update::public::Web;
use crate::install::Sha256;
use crate::mirrored;
use crate::release::netboot::Netboot;

/// Why `netboot-iso` failed.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum Error {
    /// The pins could not be read, or are not pins of the boot sets.
    #[error("the pins {path:?}: {why}")]
    Pins {
        /// The file.
        path: PathBuf,
        /// What is wrong.
        why: String,
    },
    /// The pins have no boot set of the platform.
    #[error("the pins have no ISO of the platform {0:?}")]
    Platform(String),
    /// A `--mirror` that is not a plain HTTPS base URL.
    #[error(
        "the mirror {0:?} is not an https:// URL with no trailing '/', query, fragment or user"
    )]
    Mirror(String),
    /// The directory, or the file already in it, could not be read.
    #[error("{path:?}: {why}")]
    Dir {
        /// The directory or the file.
        path: PathBuf,
        /// The system's reason.
        why: String,
    },
    /// No URL gave the pinned ISO, or it could not be written.
    #[error(transparent)]
    Fetch(#[from] mirrored::Error),
}

/// Fetches the ISO of `platform`'s boot set in the pins file `pins` into `dir` (see the
/// module) through `web`, `pause` waiting before each round: the line that says where it came
/// from, or that the file there was kept.
pub(crate) fn run(
    pins: &Path,
    platform: &str,
    mirrors: &[String],
    dir: &Path,
    web: &dyn Web,
    pause: &dyn Fn(Duration),
) -> Result<String, Error> {
    let pins_error = |why: String| Error::Pins {
        path: pins.to_owned(),
        why,
    };
    let text = std::fs::read_to_string(pins).map_err(|e| pins_error(e.to_string()))?;
    let netboot = Netboot::parse(&text, &pins.display().to_string())
        .map_err(|e| pins_error(e.to_string()))?;
    let (url, pin) = netboot
        .iso(platform)
        .ok_or_else(|| Error::Platform(platform.to_owned()))?;
    if let Some(bad) = mirrors.iter().find(|m| !mirrored::is_base_url(m)) {
        return Err(Error::Mirror(bad.clone()));
    }
    // The pin's URL is `https://` and plain (`Netboot::parse`): its path starts at the first
    // `/` after the scheme.
    let after_scheme = url.strip_prefix("https://").unwrap_or_default();
    let path = after_scheme
        .find('/')
        .map_or("", |slash| &after_scheme[slash..]);
    let urls: Vec<String> = std::iter::once(url.clone())
        .chain(mirrors.iter().map(|mirror| format!("{mirror}{path}")))
        .collect();
    std::fs::create_dir_all(dir).map_err(dir_error(dir))?;
    let file = dir.join(&pin.name);
    match kept(&file) {
        Ok((bytes, sha256)) if Some(bytes) == pin.bytes && sha256 == pin.sha256 => {
            return Ok(format!(
                "ok: the kept {} has {bytes} bytes and SHA-256 {sha256}",
                pin.name
            ));
        }
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(dir_error(&file)(e)),
    }
    Ok(mirrored::fetch(&urls, &pin, dir, web, pause)?)
}

/// The size and SHA-256 of the file at `path`, read in pieces (an ISO is gigabytes).
fn kept(path: &Path) -> io::Result<(u64, Sha256)> {
    let mut file = std::fs::File::open(path)?;
    let mut hashed = mirrored::Hashed::new(io::sink());
    io::copy(&mut file, &mut hashed)?;
    Ok(hashed.finish())
}

/// The [`Error::Dir`] of `path`, for `map_err`.
fn dir_error(path: &Path) -> impl FnOnce(io::Error) -> Error + '_ {
    move |e| Error::Dir {
        path: path.to_owned(),
        why: e.to_string(),
    }
}

#[cfg(test)]
mod tests;
