//! `stage` (packages-repositories.md section 4.3, packages-native.md section 10.2's order of
//! checks): every check before the next reads a byte, and nothing outside the work directory
//! and the staging directory is written.

use std::fs;
use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use super::outcome::{Failure, Reason};
use super::tree::{self, Rules};
use super::{Order, RepositoryInstaller, Staged, io_failure, record_failure};

/// The directory of the staged tree.
pub const TREE: &str = "tree";

/// The directory of the work directory the control members are extracted into.
pub const CONTROL: &str = "control";

/// The only service whose package may carry maintainer scripts: hostkeeper's own, for the
/// service `hostkeeper` (packages-native.md section 10.4).
pub const OWN: &str = "hostkeeper";

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
        let control = work.join(CONTROL);
        fs::create_dir(&control).map_err(|e| io_failure("the control directory", e))?;
        let members = backend.control_members(tools, &file, &control, bound)?;
        let own = (order.service.as_str() == OWN) & (package == OWN);
        let scripts: Vec<&String> = members
            .iter()
            .filter(|m| !backend.allowed_members().contains(&m.as_str()))
            .collect();
        if !scripts.is_empty() && !own {
            let names: Vec<&str> = scripts.iter().map(|s| s.as_str()).collect();
            return Err(fail(
                Reason::PackageScripts,
                format!(
                    "the package carries the maintainer scripts {}",
                    names.join(", ")
                ),
            ));
        }
        let tree_dir = staging.join(TREE);
        fs::create_dir(&tree_dir).map_err(|e| io_failure("the tree", e))?;
        backend.extract(tools, &file, &tree_dir, bound)?;
        let rules = Rules {
            service: order.service.as_str(),
            package,
            run: &order.run,
            boot: order.boot.as_deref(),
            params: order.params,
        };
        let hook = tree::check(&tree_dir, &rules)
            .map_err(|e| fail(Reason::ManifestInvalid, e.to_string()))?;
        super::enable::wanted(&tree_dir, &order.run)
            .map_err(|e| fail(Reason::ManifestInvalid, e.to_string()))?;
        let meta = staging.join(backend.meta_dir());
        backend
            .keep(&work, &file, &meta, &order.repository)
            .map_err(|e| io_failure("the version's copy of the index", e))?;
        Ok(Staged { hook })
    }
}
