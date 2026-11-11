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
//! 3. The text is judged by the loader of its file before anything is written: the stores by
//!    [`hostkeeper_package::stores::parse`] (which reads their credential files as the load does)
//!    and the signers by [`hostkeeper_install::macos_pkg::signers::parse`], and the file with its
//!    header must be no larger than the loader reads ([`stores::MAX_STORES_BYTES`],
//!    [`signers::MAX_SIGNERS_BYTES`]). A refusal is the loader's reason, or
//!    [`MacFilesError::TooLarge`].
//! 4. The managed preferences are the file's source, so a target that differs is replaced, unlike
//!    the daemon config: a target holding exactly the rendered bytes, owned by the trusted owner
//!    with mode 0644, is left alone (not written), and otherwise a new temporary file in the
//!    target's directory (made mode 0755 when missing), mode 0644, written and synced, is renamed
//!    over the target, and the directory is synced. A temporary file of the same name left by a
//!    start that was killed is removed first, and no temporary file is left on any error.
//! 5. Something at the target path that is not a regular file (a link, a directory) is refused and
//!    left as it is ([`MacFilesError::NotAFile`]): the loaders read only a regular file.
//!
//! The rendered file starts with a comment that names its domain. A refusal is the start's error
//! ([`crate::start::StartError::MacFiles`], [`crate::start::EXIT_CONFIG`]).

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
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
    /// The rendered file is larger than its loader reads.
    #[error("the managed preferences {} of {domain} render to {bytes} bytes, over the {cap} its file may hold", path.display())]
    TooLarge {
        domain: &'static str,
        path: PathBuf,
        bytes: usize,
        cap: u64,
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
    let domain = Domain {
        name: STORES_DOMAIN,
        cap: stores::MAX_STORES_BYTES,
    };
    render_to(managed, target, trusted_uid, domain, |text| {
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
    let domain = Domain {
        name: MACOS_PKG_DOMAIN,
        cap: signers::MAX_SIGNERS_BYTES,
    };
    render_to(managed, target, trusted_uid, domain, |text| {
        signers::parse(text.as_bytes())
            .map(drop)
            .map_err(|source| MacFilesError::Signers {
                path: managed.to_path_buf(),
                source,
            })
    })
}

/// A domain: its name, and the most bytes its file may hold.
struct Domain {
    name: &'static str,
    cap: u64,
}

/// The module's steps for the domain `domain`, `check` being its loader.
fn render_to(
    managed: &Path,
    target: &Path,
    trusted_uid: u32,
    Domain { name: domain, cap }: Domain,
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
    if file.len() as u64 > cap {
        return Err(MacFilesError::TooLarge {
            domain,
            path: managed.to_path_buf(),
            bytes: file.len(),
            cap,
        });
    }
    replace(target, file.as_bytes(), trusted_uid, TEMPORARY).map_err(|error| match error {
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
pub(crate) enum Replace {
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

/// The name the temporary file of a rendered file starts with ([`replace`]).
const TEMPORARY: &str = "hostkeeper-mac-files";

/// Makes `target` hold `bytes`, owned by `owner` with mode 0644, as the module's steps 4 and 5
/// say, through a temporary file in its directory named after `temporary` and the process
/// (`.<temporary>.<pid>.tmp`). The facts file is written the same way ([`crate::facts_file`]).
pub(crate) fn replace(
    target: &Path,
    bytes: &[u8],
    owner: u32,
    temporary: &str,
) -> Result<Rendered, Replace> {
    match fs::symlink_metadata(target) {
        Ok(meta) if meta.file_type().is_file() => {
            let sound = (meta.uid() == owner) & (meta.mode() & 0o7777 == 0o644);
            if sound && fs::read(target)? == bytes {
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
    let path = parent.join(temporary_name(temporary));
    match fs::remove_file(&path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(Replace::Io(error)),
        _ => {}
    }
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

/// The name of the temporary file named after `temporary`, in the target's directory.
fn temporary_name(temporary: &str) -> String {
    format!(".{temporary}.{}.tmp", std::process::id())
}

/// Removes the file at its path when dropped, so that no failure leaves it behind; after a rename
/// there is nothing at the path and the removal finds nothing.
struct Temporary(PathBuf);

impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
