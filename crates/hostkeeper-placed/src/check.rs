//! The `path_conflict` refusal (design section 4.7, "Placed paths"; section 8.10, "Two packages
//! claim one placed path"): before anything is switched, stopped or linked, a package that claims
//! a path another service owns, or a path occupied by something `placed.json` does not list as
//! the claimant's, is refused. [`check`] reads and writes nothing else, so a caller runs it alone
//! before anything changes.

use std::path::Path;

use crate::claim::Claims;
use crate::error::{Owner, PlacedError, io_at};
use crate::fs::{FileKind, Fs};
use crate::store::PlacedRecord;

/// What is at `path` on disk, as the [`Owner`] a conflict would name: `None` when nothing is.
pub(crate) fn occupant(fs: &dyn Fs, path: &Path) -> Result<Option<Owner>, PlacedError> {
    let kind = fs.kind(path).map_err(io_at("inspect", path))?;
    Ok(match kind {
        None => None,
        Some(FileKind::Symlink) => Some(Owner::Link(
            fs.read_link(path).map_err(io_at("read the link", path))?,
        )),
        Some(FileKind::File) => Some(Owner::File),
        Some(FileKind::Dir) => Some(Owner::Directory),
        Some(FileKind::Other) => Some(Owner::Other),
    })
}

/// Checks one path `service` claims.
pub(crate) fn check_path(
    service: &str,
    path: &Path,
    record: &PlacedRecord,
    fs: &dyn Fs,
) -> Result<(), PlacedError> {
    let conflict = |owner: Owner| PlacedError::PathConflict {
        path: path.to_path_buf(),
        owner,
    };
    // Another service's path, or one above or below it: a directory linked at one would
    // receive the other.
    let foreign = record.paths.iter().find(|(other, entry)| {
        (entry.service != service) & (other.starts_with(path) | path.starts_with(other))
    });
    if let Some((_, entry)) = foreign {
        return Err(conflict(Owner::Service(entry.service.clone())));
    }
    let listed = record.paths.get(path);
    match (occupant(fs, path)?, listed.is_some()) {
        (None, _) | (Some(Owner::Link(_)), true) => Ok(()),
        (Some(owner), _) => Err(conflict(owner)),
    }
}

/// Refuses `claims` with [`PlacedError::PathConflict`] when one of its paths is listed in
/// `record` for another service, or is above or below such a path, or is occupied on disk by
/// anything but a link `record` lists for the claimant: a regular file, a directory, another
/// kind of file, or a link the record does not list. A path the claimant already owns is
/// accepted, for a repeat or a new version. Nothing is written.
pub fn check(claims: &Claims, record: &PlacedRecord, fs: &dyn Fs) -> Result<(), PlacedError> {
    let service = claims.service().as_str();
    claims
        .iter()
        .try_for_each(|claim| check_path(service, &claim.path, record, fs))
}
