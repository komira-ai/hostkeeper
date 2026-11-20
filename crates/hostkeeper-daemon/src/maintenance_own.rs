//! The packages hostkeeper itself installs, which a maintenance look never reports due
//! ([`crate::maintenance_collect::Look::own_packages`]): hostkeeper's own native package, and each
//! package the repository installer recorded in `placed.json` as installed for a service. They
//! are updated by hostkeeper's own orders, never by a maintenance run.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use hostkeeper_placed::{RealFs, Store};

/// The name of hostkeeper's own native package (`packaging/linux/nfpm.yaml`).
pub const OWN_PACKAGE: &str = "hostkeeper";

/// Where a maintenance look learns which packages are hostkeeper's own.
pub trait OwnPackages: Send + Sync {
    /// The package manager's names of the packages hostkeeper installs.
    ///
    /// # Errors
    /// Why they could not be read, as the look's failed reason words it after `own packages: `.
    fn names(&self) -> Result<BTreeSet<String>, String>;
}

/// The packages of `placed.json` in the daemon's state directory, and hostkeeper's own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlacedPackages {
    /// The state directory `placed.json` is in, as an absolute path of the host.
    pub dir: PathBuf,
}

impl PlacedPackages {
    /// The packages recorded in the state directory `dir`.
    #[must_use]
    pub fn new(dir: &Path) -> PlacedPackages {
        PlacedPackages {
            dir: dir.to_owned(),
        }
    }
}

impl OwnPackages for PlacedPackages {
    /// [`OWN_PACKAGE`] and the `package` of every `installed` entry that names one; a state
    /// directory with no `placed.json` holds [`OWN_PACKAGE`] alone.
    fn names(&self) -> Result<BTreeSet<String>, String> {
        let fs = RealFs::new(Path::new("/"));
        let record = Store::new(&fs, &self.dir)
            .load()
            .map_err(|error| error.to_string())?;
        let mut names = BTreeSet::from([OWN_PACKAGE.to_owned()]);
        names.extend(
            record
                .installed
                .values()
                .filter_map(|entry| entry.package.clone()),
        );
        Ok(names)
    }
}

#[cfg(test)]
#[path = "maintenance_own_tests.rs"]
mod tests;
