//! The packages hostkeeper itself installs, which a maintenance look never reports due
//! ([`crate::maintenance_collect::Look::own_packages`]): hostkeeper's own native package, and each
//! package the repository installer recorded in `placed.json` as installed for a service. They
//! are updated by hostkeeper's own orders, never by a maintenance run.

use std::collections::BTreeSet;
use std::sync::Arc;

use hostkeeper_placed::{Fs, Roots, Store};

use crate::Placement;

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

/// The packages of the daemon's `placed.json`, and hostkeeper's own.
pub struct PlacedPackages {
    roots: Roots,
    fs: Arc<dyn Fs + Send + Sync>,
}

impl PlacedPackages {
    /// The packages recorded in the `placed.json` of `placement`, the daemon's own: in the state
    /// directory of its roots, read through its filesystem, as the repository installer writes it.
    #[must_use]
    pub fn new(placement: &Placement) -> PlacedPackages {
        PlacedPackages {
            roots: placement.roots.clone(),
            fs: Arc::clone(&placement.fs),
        }
    }
}

impl OwnPackages for PlacedPackages {
    /// [`OWN_PACKAGE`] and the `package` of every `installed` entry that names one; a state
    /// directory with no `placed.json` holds [`OWN_PACKAGE`] alone.
    fn names(&self) -> Result<BTreeSet<String>, String> {
        let record = Store::new(&*self.fs, self.roots.state_dir())
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
