//! [`stage_filled`]: a staging directory the caller filled, sealed and published as a version
//! directory exactly as an unpacked archive is (docs/design/packages-lifecycle.md section 6.1).
//!
//! A repository installer makes its version directory itself: the core creates the staging
//! directory, the caller writes into its real path, and the core makes the tree durable, seals
//! it (the same `.seal` file, with the tree digest of what was written and the package digest
//! the order names), renames it in and makes it 0555. Prune, the boot reseal and the step
//! runner's checks then treat it like any version. No manifest is read: what the tree holds is
//! the caller's.

use std::fmt::{Debug, Display};
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use hostkeeper_config::daemon::Sha256Hex;
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_steps::{ArtifactError, Layout, read_tree};

use crate::error::UnpackError;
use crate::fs::PackageFs;
use crate::publish::publish;
use crate::unpack::{begin, check_name, clean_up, make_seal, sealed, settle_if_open};

/// What a filled stage is for.
#[derive(Clone, Copy, Debug)]
pub struct FillOrder<'a> {
    pub service: &'a ServiceName,
    pub version: &'a Version,
    /// The digest the seal records as `package_digest`.
    pub package_digest: &'a Sha256Hex,
}

/// A version directory published from a filled stage, or found sealed for the same digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filled {
    /// The real path of `packages/<service>/<version>`.
    pub dir: PathBuf,
    /// `false` when the version was on the host already, sealed for the same digest.
    pub fresh: bool,
}

/// Why there is no version directory.
#[derive(Debug, thiserror::Error)]
pub enum FillError<E: Debug + Display> {
    /// The core's own failure, with its outcome as an unpack's.
    #[error("{0}")]
    Unpack(UnpackError),
    /// The caller's fill, or its check of a version already there, refused.
    #[error("{0}")]
    Fill(E),
}

/// Publishes `<service>/<version>` from a stage the caller fills, through `fs`.
///
/// In order: the version-name check, the service directory, its lock and its orphans, as
/// [`crate::unpack`] does them. When the version is on the host, its seal must be there, must be
/// for `order.package_digest`, and the tree must have the seal's digest
/// ([`UnpackError::ArtifactChanged`] otherwise); then `same` is called with the real path of the
/// version (its error is [`FillError::Fill`], and nothing is changed), the directory is made 0555
/// if a crash left it 0700, and the result is `fresh: false`. Otherwise a staging directory is
/// made and `fill` is called with its real path (mode 0700, empty); its error is
/// [`FillError::Fill`]. The filled tree is made durable (every regular file and directory
/// synced, links neither followed nor synced), its tree digest is read, and the seal and the
/// tree are published as [`crate::unpack`] publishes them. On any error after the stage was
/// made, the staging directory, the temporary seal and a seal without its version are removed.
pub fn stage_filled<E: Debug + Display>(
    order: &FillOrder<'_>,
    fs: &dyn PackageFs,
    same: &mut dyn FnMut(&Path) -> Result<(), E>,
    fill: &mut dyn FnMut(&Path) -> Result<(), E>,
) -> Result<Filled, FillError<E>> {
    let service = Path::new(order.service.as_str());
    let version = order.version.as_str();
    let layout = Layout::package(&fs.real_path(service));
    check_name(&layout, version).map_err(FillError::Unpack)?;
    let _lock = begin(service, fs).map_err(FillError::Unpack)?;
    let dir = layout.version_path(version);
    let found = sealed(
        &layout,
        order.service.as_str(),
        version,
        order.package_digest,
        fs,
    )
    .map_err(FillError::Unpack)?;
    if found.is_some() {
        same(&dir).map_err(FillError::Fill)?;
        settle_if_open(fs, order.service.as_str(), version, &dir).map_err(FillError::Unpack)?;
        return Ok(Filled { dir, fresh: false });
    }
    let stage = fs
        .create_stage(service)
        .map_err(|e| UnpackError::of_io("cannot create the staging directory", e))
        .map_err(FillError::Unpack)?;
    let result = fill_and_publish(order, fs, fill, &stage);
    if result.is_err() {
        clean_up(fs, service, version, &stage);
    }
    result.map(|()| Filled { dir, fresh: true })
}

/// The fill, the durability of what it wrote, the seal and the publish, of the stage `stage`.
fn fill_and_publish<E: Debug + Display>(
    order: &FillOrder<'_>,
    fs: &dyn PackageFs,
    fill: &mut dyn FnMut(&Path) -> Result<(), E>,
    stage: &Path,
) -> Result<(), FillError<E>> {
    let real = fs.real_path(stage);
    fill(&real).map_err(FillError::Fill)?;
    let durable =
        |e| FillError::Unpack(UnpackError::of_io("cannot make the filled tree durable", e));
    sync_tree(&real).map_err(durable)?;
    fs.sync_dir(stage).map_err(durable)?;
    let tree = read_tree(&real).map_err(|e| {
        FillError::Unpack(match e {
            ArtifactError::Io(e) => UnpackError::of_io("cannot read the filled tree", e),
            other => UnpackError::Io {
                what: "cannot read the filled tree",
                source: io::Error::other(other.to_string()),
            },
        })
    })?;
    let seal = make_seal(&tree.digest, order.package_digest).map_err(FillError::Unpack)?;
    let service = Path::new(order.service.as_str());
    publish(fs, service, stage, order.version.as_str(), &seal).map_err(FillError::Unpack)
}

/// Syncs every regular file and every directory at and below `dir`, contents first. Links are
/// not followed and not synced; nor is anything that is neither a file nor a directory.
fn sync_tree(dir: &Path) -> io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            sync_tree(&entry.path())?;
        } else if kind.is_file() {
            File::open(entry.path())?.sync_all()?;
        }
    }
    File::open(dir)?.sync_all()
}
