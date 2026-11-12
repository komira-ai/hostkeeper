//! `debs --pins <file> --dir <dir>`: the Debian packages a CI job installs with `dpkg`, each
//! pinned by its exact version and SHA-256 in a file of `.github/debs/`, so that a run needs no
//! `apt-get update` and, when the actions cache restored the packages, no package archive.
//!
//! The pins file has a table per Debian architecture (`amd64`, `arm64`), each with `mirrors`
//! (HTTPS base URLs of one archive, tried in their order) and `debs` (per package its `package`,
//! `version`, `path` below a mirror, and `sha256`). The verb takes the table of the host's
//! architecture and the directory the actions cache restores and saves, and:
//!
//! 1. refuses the directory when it holds a file no pin names, so that
//!    `dpkg --install <dir>/*.deb` installs the pinned packages and nothing else;
//! 2. for each pin, in the file's order, keeps the file of that name in the directory when it
//!    is there and its SHA-256 is the pinned one, and refuses it, without replacing it, when its
//!    SHA-256 is another (a cache entry under the key of these pins holds other bytes);
//! 3. when it is not there, fetches it from each mirror in turn (`mirrored::fetch`: bounded
//!    rounds, the SHA-256 checked, a `.part` file renamed into place).
//!
//! So a job whose packages the actions cache restored makes no network call, and a job whose
//! cache missed survives a mirror being unreachable; either way only the pinned bytes reach the
//! directory.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::coordinator_update::public::Web;
use crate::install::Sha256;
use crate::mirrored;

/// A Debian architecture the pins files have a table for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Arch {
    /// `amd64` (Rust's `x86_64`).
    Amd64,
    /// `arm64` (Rust's `aarch64`).
    Arm64,
}

impl Arch {
    /// The Debian architecture of Rust's `std::env::consts::ARCH` value `arch`.
    pub(crate) fn of(arch: &str) -> Result<Arch, Error> {
        match arch {
            "x86_64" => Ok(Arch::Amd64),
            "aarch64" => Ok(Arch::Arm64),
            other => Err(Error::Arch(other.to_owned())),
        }
    }

    /// Its name: the table of a pins file, and the last part of a package's file name.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Arch::Amd64 => "amd64",
            Arch::Arm64 => "arm64",
        }
    }
}

/// Why `debs` failed.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum Error {
    /// The host's architecture has no Debian name here.
    #[error("this host's architecture {0:?} is neither x86_64 nor aarch64")]
    Arch(String),
    /// The pins file could not be read, or does not keep its rules.
    #[error("the pins file {path:?}: {why}")]
    Pins {
        /// The file.
        path: PathBuf,
        /// What is wrong.
        why: String,
    },
    /// The directory, or a file in it, could not be read.
    #[error("{path:?}: {why}; nothing was installed")]
    Dir {
        /// The directory or the file.
        path: PathBuf,
        /// The system's reason.
        why: String,
    },
    /// The directory holds a file no pin names.
    #[error("{dir:?} holds {name:?}, which no pin names; nothing was installed")]
    Stray {
        /// The directory.
        dir: PathBuf,
        /// The file's name.
        name: String,
    },
    /// A file in the directory is not the pinned file.
    #[error(
        "the cached {path:?} has SHA-256 {actual}, not the pinned {expected}; nothing was installed"
    )]
    CachedMismatch {
        /// The file.
        path: PathBuf,
        /// The pinned digest.
        expected: Sha256,
        /// The digest of the file.
        actual: Sha256,
    },
    /// No mirror gave a pinned file, or it could not be written.
    #[error("{0}; nothing was installed")]
    Fetch(#[from] mirrored::Error),
}

/// The pins file as written: a table per architecture.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    amd64: Option<Table>,
    arm64: Option<Table>,
}

/// The pins of one architecture, as written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Table {
    mirrors: Vec<String>,
    debs: Vec<Written>,
}

/// One pin, as written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Written {
    package: String,
    version: String,
    path: String,
    sha256: String,
}

/// One pinned package.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Pin {
    /// Its file name: `<package>_<version without epoch>_<arch or all>.deb`.
    pub(crate) name: String,
    /// Its path below a mirror (`pool/...`).
    pub(crate) path: String,
    /// Its SHA-256.
    pub(crate) sha256: Sha256,
}

/// The pins of one architecture.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Pins {
    /// The base URLs to download from, in order.
    pub(crate) mirrors: Vec<String>,
    /// The packages, in order.
    pub(crate) debs: Vec<Pin>,
}

/// The pins of `arch` in the pins file `text`, or what is wrong with it.
pub(crate) fn parse(text: &str, arch: Arch) -> Result<Pins, String> {
    let file: File = toml::from_str(text).map_err(|e| e.message().to_owned())?;
    let table = match arch {
        Arch::Amd64 => file.amd64,
        Arch::Arm64 => file.arm64,
    }
    .ok_or_else(|| format!("no [{}] table", arch.name()))?;
    if table.mirrors.is_empty() {
        return Err(format!("[{}] names no mirror", arch.name()));
    }
    for mirror in &table.mirrors {
        if !mirrored::is_base_url(mirror) {
            return Err(format!(
                "the mirror {mirror:?} is not an https:// URL with no trailing '/', query, \
                 fragment or user"
            ));
        }
    }
    if table.debs.is_empty() {
        return Err(format!("[{}] pins no package", arch.name()));
    }
    let mut debs: Vec<Pin> = Vec::new();
    for written in table.debs {
        let pin = pin(written, arch)?;
        if debs.iter().any(|d| d.name == pin.name) {
            return Err(format!("{} is pinned twice", pin.name));
        }
        debs.push(pin);
    }
    Ok(Pins {
        mirrors: table.mirrors,
        debs,
    })
}

/// `written` checked: a digest, and a path below `pool/` that ends in the file name its
/// package, version and architecture make.
fn pin(written: Written, arch: Arch) -> Result<Pin, String> {
    let sha256: Sha256 = written.sha256.parse().map_err(|e| format!("{e}"))?;
    let version = written
        .version
        .split_once(':')
        .map_or(written.version.as_str(), |(_, v)| v);
    let names = [arch.name(), "all"].map(|a| format!("{}_{version}_{a}.deb", written.package));
    let name = written.path.rsplit('/').next().unwrap_or_default();
    let inside = written.path.starts_with("pool/")
        && !written.path.split('/').any(|s| s.is_empty() || s == "..");
    if !inside || !names.iter().any(|n| n == name) {
        return Err(format!(
            "the path {:?} of {} {} is not pool/.../{} (or the `all` package)",
            written.path, written.package, written.version, names[0]
        ));
    }
    Ok(Pin {
        name: name.to_owned(),
        path: written.path,
        sha256,
    })
}

/// Reads the pins of `arch` from `pins` and makes `dir` hold exactly the pinned files, from the
/// files already there or fetched through `web`, `pause` waiting before each round: one line
/// per package.
pub(crate) fn run(
    pins: &Path,
    arch: Arch,
    dir: &Path,
    web: &dyn Web,
    pause: &dyn Fn(Duration),
) -> Result<String, Error> {
    let pins_path = pins;
    let pins = std::fs::read_to_string(pins)
        .map_err(|e| e.to_string())
        .and_then(|text| parse(&text, arch))
        .map_err(|why| Error::Pins {
            path: pins_path.to_owned(),
            why,
        })?;
    let mut names = std::fs::create_dir_all(dir)
        .and_then(|()| std::fs::read_dir(dir))
        .and_then(|entries| {
            entries
                .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
                .collect::<std::io::Result<Vec<String>>>()
        })
        .map_err(dir_error(dir))?;
    names.sort();
    if let Some(name) = names
        .iter()
        .find(|n| !pins.debs.iter().any(|d| d.name == **n))
    {
        return Err(Error::Stray {
            dir: dir.to_owned(),
            name: name.clone(),
        });
    }
    let mut lines = Vec::new();
    for pin in &pins.debs {
        let path = dir.join(&pin.name);
        match std::fs::read(&path) {
            Ok(bytes) => {
                let actual = Sha256::of(&bytes);
                if actual != pin.sha256 {
                    return Err(Error::CachedMismatch {
                        path,
                        expected: pin.sha256,
                        actual,
                    });
                }
                lines.push(format!(
                    "ok: the cached {} has SHA-256 {}",
                    pin.name, pin.sha256
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let urls: Vec<String> = pins
                    .mirrors
                    .iter()
                    .map(|mirror| format!("{mirror}/{}", pin.path))
                    .collect();
                let fetched = mirrored::Pin {
                    name: pin.name.clone(),
                    sha256: pin.sha256,
                    bytes: None,
                };
                lines.push(mirrored::fetch(&urls, &fetched, dir, web, pause)?);
            }
            Err(e) => return Err(dir_error(&path)(e)),
        }
    }
    Ok(lines.join("\n"))
}

/// The [`Error::Dir`] of `path`, for `map_err`.
fn dir_error(path: &Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
    move |e| Error::Dir {
        path: path.to_owned(),
        why: e.to_string(),
    }
}

#[cfg(test)]
mod tests;
