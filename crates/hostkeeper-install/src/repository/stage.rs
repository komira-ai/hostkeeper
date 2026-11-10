//! `stage` (packages-repositories.md section 4.3, packages-native.md section 10.2): every check
//! before the next reads a byte, and nothing outside the work directory and the staging
//! directory is written. The one rule on the package's files is that the hook program the
//! manifest names is an executable regular file of it.

use std::fs;
use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use super::outcome::{Failure, Reason};
use super::{Order, RepositoryInstaller, Staged, io_failure, record_failure};
use crate::program;

/// The directory of the staged tree.
pub const TREE: &str = "tree";

/// The SHA-256 of the file at `path`, lower-case hex.
pub fn file_sha256(path: &Path) -> std::io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(hex::encode(hasher.finalize()));
        }
        hasher.update(&buffer[..read]);
    }
}

impl RepositoryInstaller<'_> {
    /// Makes `order`'s version in `staging`, an empty directory the core made: `tree/` and the
    /// backend's copy of the index and the package.
    pub fn stage(&self, order: &Order, staging: &Path) -> Result<Staged, Failure> {
        let fail = Failure::new;
        let holder = self
            .records
            .holder(&order.package, &order.service)
            .map_err(record_failure)?;
        if let Some(other) = holder {
            return Err(fail(
                Reason::PackageConflict,
                format!(
                    "the package {} is the service {}'s",
                    order.package,
                    other.as_str()
                ),
            ));
        }
        let repository = self.repositories.get(&order.repository).ok_or_else(|| {
            fail(
                Reason::RepositoryUnknown,
                format!(
                    "the repository {:?} is not an entry of this host's configuration",
                    order.repository
                ),
            )
        })?;
        let (backend, tools, bound) = (self.backend, self.tools, order.bounds.stage);
        let work = self.fresh_work(&order.service)?;
        backend
            .prepare(&work, repository)
            .map_err(|e| io_failure("the work directory", e))?;
        backend.update(tools, &work, bound)?;
        let architecture = backend.architecture(tools, bound)?;
        let (package, version) = (order.package.as_str(), order.package_version.as_str());
        let stanza = backend
            .stanza(tools, &work, package, version, &architecture, bound)?
            .ok_or_else(|| {
                fail(
                    Reason::NotInRepository,
                    format!(
                        "the repository {} lists no {package} {version} for {architecture}",
                        order.repository
                    ),
                )
            })?;
        if stanza.sha256 != order.pin {
            return Err(fail(
                Reason::PackageDigestMismatch,
                format!(
                    "the index's SHA-256 of {package} {version} is {}, not the order's {}",
                    stanza.sha256, order.pin
                ),
            ));
        }
        if stanza.size > order.max_package_bytes {
            return Err(fail(
                Reason::PackageTooLarge,
                format!(
                    "the index's size of {package} {version} is {} bytes, over {}",
                    stanza.size, order.max_package_bytes
                ),
            ));
        }
        {
            let _held = self
                .lock
                .hold()
                .map_err(|e| io_failure("the installer lock", e))?;
            backend.simulate(tools, &work, package, version, bound)?;
        }
        let file = backend.download(tools, &work, package, version, bound)?;
        let found = file_sha256(&file).map_err(|e| io_failure("the downloaded package", e))?;
        if found != order.pin {
            return Err(fail(
                Reason::StoreDigestMismatch,
                format!(
                    "the downloaded {package} {version} has the SHA-256 {found}, not the pin {}",
                    order.pin
                ),
            ));
        }
        let fields = backend.fields(tools, &file, bound)?;
        let identity = (fields.package == package)
            & (fields.version == version)
            & ((fields.architecture == architecture) | (fields.architecture == "all"));
        if !identity {
            return Err(fail(
                Reason::ManifestInvalid,
                format!(
                    "the package is {} {} for {}, not {package} {version} for {architecture} or all",
                    fields.package, fields.version, fields.architecture
                ),
            ));
        }
        let tree_dir = staging.join(TREE);
        fs::create_dir(&tree_dir).map_err(|e| io_failure("the tree", e))?;
        backend.extract(tools, &file, &tree_dir, bound)?;
        normalize(staging, &tree_dir).map_err(|e| io_failure("the tree", e))?;
        let hook = match order.hook.as_deref() {
            Some(program) => {
                program::check(&tree_dir, program)
                    .map_err(|e| fail(Reason::ManifestInvalid, format!("the hook program: {e}")))?;
                true
            }
            None => false,
        };
        let meta = staging.join(backend.meta_dir());
        backend
            .keep(&work, &file, &meta, &order.repository)
            .map_err(|e| io_failure("the version's copy of the index", e))?;
        Ok(Staged { hook })
    }
}

/// Makes the extracted tree one the steps' trust check of the sealed tree accepts, the tree
/// directory itself included (dpkg gives it the package's `./` entry): every entry that is not a
/// link loses its setuid, setgid and group and other write bits; an entry another user owns
/// than the staging directory goes to the staging directory's owner and group; and an entry that
/// is neither a file, a directory nor a link (a fifo, a socket, a device) is left out of the copy,
/// which the seal cannot hold. dpkg installs from the kept package, not from this copy, so nothing
/// installed changes.
fn normalize(staging: &Path, tree: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt, lchown};
    let owner = fs::metadata(staging)?;
    let (uid, gid) = (owner.uid(), owner.gid());
    let mut todo = vec![tree.to_path_buf()];
    let mut first = true;
    while let Some(dir) = todo.pop() {
        let entries: Vec<std::path::PathBuf> = match first {
            true => vec![dir],
            false => fs::read_dir(&dir)?
                .map(|entry| entry.map(|e| e.path()))
                .collect::<std::io::Result<_>>()?,
        };
        first = false;
        for path in entries {
            let meta = fs::symlink_metadata(&path)?;
            let kind = meta.file_type();
            if !(kind.is_symlink() | kind.is_dir() | kind.is_file()) {
                fs::remove_file(&path)?;
                continue;
            }
            if (meta.uid() != uid) | (meta.gid() != gid) {
                lchown(&path, Some(uid), Some(gid))?;
            }
            if kind.is_symlink() {
                continue;
            }
            let mode = meta.permissions().mode() & 0o7777;
            let clean = mode & !0o6022;
            if clean != mode {
                fs::set_permissions(&path, fs::Permissions::from_mode(clean))?;
            }
            if kind.is_dir() {
                todo.push(path);
            }
        }
    }
    Ok(())
}
