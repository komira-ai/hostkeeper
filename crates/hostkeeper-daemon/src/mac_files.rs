//! The stores file and the macos-pkg signers file of a Mac, written from the managed preferences
//! its MDM installed (docs/design/packages-repositories.md, section 4.7, and
//! docs/design/hostkeeper-mdm.md, section 4). The start of a Mac's daemon does this for the
//! domains `dev.hostkeeper.stores` ([`stores`]) and `dev.hostkeeper.macos-pkg` ([`macos_pkg`])
//! after it has written the daemon config ([`crate::mac_config`]) and before it loads any of the
//! three. Each domain is judged alone:
//!
//! 1. A managed file that is not there changes nothing: the target file stays as the operator
//!    left it, and none is made.
//! 2. Otherwise the managed file is read as a file its owner alone can write
//!    ([`hostkeeper_config::trust::read_trusted`], checked on the open file), parsed as a property
//!    list whose top level is a dictionary, and rendered as TOML by the rendering of
//!    [`crate::mac_config`]. A file that is not trusted, not a property list or not renderable is
//!    refused.
//! 3. The text is judged by the daemon's own loader before anything is written: the stores by
//!    [`hostkeeper_package::stores::parse`] (which reads their credential files as the load does)
//!    and the signers by [`hostkeeper_install::macos_pkg::signers::parse`]. A refusal is the
//!    loader's reason.
//! 4. The managed preferences are the file's source, so a target that differs is replaced, unlike
//!    the daemon config: a target holding exactly the rendered bytes is left alone (not written),
//!    and otherwise a new temporary file in the target's directory (made mode 0755 when missing),
//!    mode 0644, written and synced, is renamed over the target, and the directory is synced. No
//!    temporary file is left on any failure.
//! 5. Something at the target path that is not a regular file (a link, a directory) is refused and
//!    left as it is ([`MacFilesError::NotAFile`]): the loaders read only a regular file.
//!
//! The rendered file starts with a comment that names its domain. A refusal is the start's error
//! ([`crate::start::StartError::MacFiles`], [`crate::start::EXIT_CONFIG`]).

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use hostkeeper_config::trust::{TrustError, read_trusted};
use hostkeeper_install::macos_pkg::signers::{self, SignersError};
use hostkeeper_package::stores::{self, StoresError};

use crate::mac_config::{RenderError, render};

/// The managed preferences domain of the stores file.
const STORES_DOMAIN: &str = "dev.hostkeeper.stores";

/// The managed preferences domain of the macos-pkg signers file.
const MACOS_PKG_DOMAIN: &str = "dev.hostkeeper.macos-pkg";

/// What [`stores`] and [`macos_pkg`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rendered {
    /// There is no managed file; the target is as it was.
    Absent,
    /// The target holds the rendered bytes already and was not written.
    Unchanged,
    /// The target was made or replaced.
    Written,
}

/// Why a file is not made from its managed preferences.
#[derive(Debug, thiserror::Error)]
pub enum MacFilesError {
    /// The managed file is there but is not one the trusted owner alone can write, or cannot be
    /// read.
    #[error("the managed preferences {} of {domain} are refused: {source}", path.display())]
    Refused {
        domain: &'static str,
        path: PathBuf,
        #[source]
        source: TrustError,
    },
    /// The managed file has no TOML form.
    #[error("the managed preferences {} of {domain} are refused: {source}", path.display())]
    Render {
        domain: &'static str,
        path: PathBuf,
        #[source]
        source: RenderError,
    },
    /// The stores loader refused the rendered text.
    #[error("the managed preferences {} of dev.hostkeeper.stores do not make a stores file: {source}", path.display())]
    Stores {
        path: PathBuf,
        #[source]
        source: StoresError,
    },
    /// The signers loader refused the rendered text.
    #[error("the managed preferences {} of dev.hostkeeper.macos-pkg do not make a signers file: {source}", path.display())]
    Signers {
        path: PathBuf,
        #[source]
        source: SignersError,
    },
    /// Something other than a regular file is at the target path; it is left as it is.
    #[error("{} is there but is not a regular file; it is not replaced", path.display())]
    NotAFile { path: PathBuf },
    /// The target could not be read or written.
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// The stores file `target` from the managed preferences `managed` of `dev.hostkeeper.stores`;
/// `trusted_uid` is the owner the managed file must have and the owner the stores' credential
/// files must have.
pub fn stores(managed: &Path, target: &Path, trusted_uid: u32) -> Result<Rendered, MacFilesError> {
    render_to(managed, target, trusted_uid, STORES_DOMAIN, |text| {
        stores::parse(text.as_bytes(), trusted_uid)
            .map(drop)
            .map_err(|source| MacFilesError::Stores {
                path: managed.to_path_buf(),
                source,
            })
    })
}

/// The signers file `target` from the managed preferences `managed` of `dev.hostkeeper.macos-pkg`;
/// `trusted_uid` is the owner the managed file must have.
pub fn macos_pkg(
    managed: &Path,
    target: &Path,
    trusted_uid: u32,
) -> Result<Rendered, MacFilesError> {
    render_to(managed, target, trusted_uid, MACOS_PKG_DOMAIN, |text| {
        signers::parse(text.as_bytes())
            .map(drop)
            .map_err(|source| MacFilesError::Signers {
                path: managed.to_path_buf(),
                source,
            })
    })
}

/// The module's steps for the domain `domain`, `check` being its loader.
fn render_to(
    managed: &Path,
    target: &Path,
    trusted_uid: u32,
    domain: &'static str,
    check: impl FnOnce(&str) -> Result<(), MacFilesError>,
) -> Result<Rendered, MacFilesError> {
    if fs::symlink_metadata(managed).is_err_and(|e| e.kind() == io::ErrorKind::NotFound) {
        return Ok(Rendered::Absent);
    }
    let bytes = read_trusted(managed, trusted_uid).map_err(|source| MacFilesError::Refused {
        domain,
        path: managed.to_path_buf(),
        source,
    })?;
    let text = render(&bytes).map_err(|source| MacFilesError::Render {
        domain,
        path: managed.to_path_buf(),
        source,
    })?;
    check(&text)?;
    let file = format!("# Written by hostkeeper from the managed preferences of {domain}.\n{text}");
    replace(target, file.as_bytes()).map_err(|error| match error {
        Replace::NotAFile => MacFilesError::NotAFile {
            path: target.to_path_buf(),
        },
        Replace::Io(source) => MacFilesError::Io {
            path: target.to_path_buf(),
            source,
        },
    })
}

/// Why [`replace`] stopped.
enum Replace {
    /// Something other than a regular file is at the path.
    NotAFile,
    /// A filesystem call failed.
    Io(io::Error),
}

impl From<io::Error> for Replace {
    fn from(error: io::Error) -> Replace {
        Replace::Io(error)
    }
}

/// Makes `target` hold `bytes`, as the module's steps 4 and 5 say.
fn replace(target: &Path, bytes: &[u8]) -> Result<Rendered, Replace> {
    match fs::symlink_metadata(target) {
        Ok(meta) if meta.file_type().is_file() => {
            if fs::read(target)? == bytes {
                return Ok(Rendered::Unchanged);
            }
        }
        Ok(_) => return Err(Replace::NotAFile),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(Replace::Io(error)),
    }
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(parent)?;
    let path = parent.join(temporary_name());
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(&path)?;
    let temporary = Temporary(path);
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temporary.0, target)?;
    drop(temporary);
    File::open(parent)?.sync_all()?;
    Ok(Rendered::Written)
}

/// The name of the temporary file, in the target's directory.
fn temporary_name() -> String {
    format!(".hostkeeper-mac-files.{}.tmp", std::process::id())
}

/// Removes the file at its path when dropped, so that no failure leaves it behind; after a rename
/// there is nothing at the path and the removal finds nothing.
struct Temporary(PathBuf);

impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
