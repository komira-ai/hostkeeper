//! What every publish of `service` does around the version: the version-name check, the service
//! directory and its lock, the orphans a crashed publish left, and the check of a version that is
//! on the host already.

use std::collections::BTreeSet;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use hostkeeper_config::daemon::Sha256Hex;
use hostkeeper_config::names::ServiceName;
use hostkeeper_steps::{Layout, Seal, SealError, read_seal};

use super::error::UnpackError;
use super::fs::{PackageFs, ServiceLock};
use super::publish::settle;
use super::seal;

/// Refuses a version name that is one of the package directory's own names, or too long.
pub(crate) fn check_name(layout: &Layout, version: &str) -> Result<(), UnpackError> {
    if let Err(refused) = layout.check_name(version) {
        let version = version.to_owned();
        return Err(match refused.kind() {
            io::ErrorKind::InvalidFilename => UnpackError::VersionTooLong { version },
            _ => UnpackError::ReservedVersion { version },
        });
    }
    Ok(())
}

/// The start of every publish of `service`: creates its directory when it is missing, takes its
/// lock (held until the returned guard is dropped) and removes its orphans.
pub(crate) fn begin(service: &Path, fs: &dyn PackageFs) -> Result<ServiceLock, UnpackError> {
    fs.create_service(service)
        .map_err(|e| UnpackError::of_io("cannot create the service directory", e))?;
    let guard = lock(service, fs)?;
    remove_orphans_locked(service, fs)?;
    Ok(guard)
}

/// What a failed publish removes: the staging directory, the temporary seal and a seal without
/// its version. What a failed cleanup leaves, the next remove_orphans removes. The seal goes
/// only when the version is known to be absent: after the tree's rename it is the version's.
pub(crate) fn clean_up(fs: &dyn PackageFs, service: &Path, version: &str, stage: &Path) {
    let _ = fs.remove_tree(stage);
    let _ = fs.remove_tree(&seal::temporary(service, version));
    if let Ok(false) = fs.exists(&service.join(version)) {
        let _ = fs.remove_tree(&seal::placed(service, version));
    }
}

/// The service's publish lock.
fn lock(service: &Path, fs: &dyn PackageFs) -> Result<ServiceLock, UnpackError> {
    fs.lock(service)
        .map_err(|e| UnpackError::of_io("cannot take the lock of the service", e))
}

/// Removes what a crashed publish of `service` left in `packages/<service>`: every `.stage-*`
/// directory, every `.<version>.seal.tmp`, and every `<version>.seal` with no `<version>`
/// beside it. Nothing else is touched. A service with no directory has nothing to remove. It
/// takes the service's lock, so it never removes the staging directory or the seal of a publish
/// under way: while one runs, it is an [`UnpackError::Io`] to try again.
pub fn remove_orphans(service: &ServiceName, fs: &dyn PackageFs) -> Result<(), UnpackError> {
    let dir = Path::new(service.as_str());
    let present = fs
        .exists(dir)
        .map_err(|e| UnpackError::of_io("cannot look at the package directory", e))?;
    if !present {
        return Ok(());
    }
    let _lock = lock(dir, fs)?;
    remove_orphans_locked(dir, fs)
}

/// [`remove_orphans`] under a lock the caller holds.
fn remove_orphans_locked(dir: &Path, fs: &dyn PackageFs) -> Result<(), UnpackError> {
    let names = fs
        .read_dir(dir)
        .map_err(|e| UnpackError::of_io("cannot list the package directory", e))?;
    let present: BTreeSet<&[u8]> = names.iter().map(|name| name.as_bytes()).collect();
    for name in &names {
        let bytes = name.as_bytes();
        let stage = bytes.starts_with(b".stage-");
        let tmp = bytes.starts_with(b".") & bytes.ends_with(b".seal.tmp");
        let lone = bytes
            .strip_suffix(b".seal")
            .is_some_and(|version| !present.contains(version));
        if stage | tmp | lone {
            fs.remove_tree(&dir.join(name))
                .map_err(|e| UnpackError::of_io("cannot remove an orphan", e))?;
        }
    }
    Ok(())
}

/// The seal of `version` when it is on the host: `None` when there is no such directory, the seal
/// when it is for the package digest `digest` and the tree has its digest, an error otherwise.
pub(crate) fn sealed(
    layout: &Layout,
    service: &str,
    version: &str,
    digest: &Sha256Hex,
    fs: &dyn PackageFs,
) -> Result<Option<Seal>, UnpackError> {
    let present = fs
        .exists(&Path::new(service).join(version))
        .map_err(|e| UnpackError::of_io("cannot look at the version", e))?;
    if !present {
        return Ok(None);
    }
    let changed = |why: String| UnpackError::ArtifactChanged {
        version: version.to_owned(),
        why,
    };
    let unreadable = |why: String| UnpackError::SealUnreadable {
        version: version.to_owned(),
        why,
    };
    let bytes = match read_seal(&layout.seal_path(version)) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(changed("has no seal".to_owned()));
        }
        read => read.map_err(|e| unreadable(e.to_string()))?,
    };
    let seal = Seal::parse(&bytes).map_err(|e| match e {
        SealError::Schema(found) => UnpackError::SealSchema {
            version: version.to_owned(),
            found,
        },
        other => unreadable(other.to_string()),
    })?;
    if seal.package_digest() != digest.as_str() {
        return Err(changed(format!(
            "was sealed for the package {}, not {}",
            seal.package_digest(),
            digest.as_str()
        )));
    }
    match layout.check_tree(version) {
        Err(e) if e.kind() == io::ErrorKind::InvalidData => return Err(changed(e.to_string())),
        checked => checked.map_err(|e| UnpackError::of_io("cannot read the version", e))?,
    }
    Ok(Some(seal))
}

/// Makes the directory `dir` of `version` 0555 unless it is.
pub(crate) fn settle_if_open(
    fs: &dyn PackageFs,
    service: &str,
    version: &str,
    dir: &Path,
) -> Result<(), UnpackError> {
    // A crash between the publish's rename of the tree and the mode after it leaves the version
    // sealed and whole but still 0700; this is where it is made 0555. The mode of the version
    // directory is not part of the tree digest, so the seal is untouched; a version already 0555
    // is not written to (one that cannot be looked at is settled, and the settle says why).
    let read_only = std::fs::symlink_metadata(dir)
        .is_ok_and(|meta| meta.permissions().mode() & 0o7777 == 0o555);
    if !read_only {
        settle(fs, Path::new(service), version)?;
    }
    Ok(())
}

/// The seal of a tree with the digest `tree` for the package digest `package`. Both are 64
/// lower-case hex digits (`read_tree`'s and `Sha256Hex`'s), so the seal is always made; the error
/// is built eagerly so that no code path exists only for it.
pub(crate) fn make_seal(tree: &str, package: &Sha256Hex) -> Result<Seal, UnpackError> {
    let unsealable = UnpackError::Io {
        what: "cannot seal",
        source: io::Error::other("a digest that is not 64 lower-case hex digits"),
    };
    Seal::new(tree, package.as_str()).ok().ok_or(unsealable)
}
