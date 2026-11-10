//! The real package store: the fetch of a `macos-pkg` entry's `.pkg` by its pin behind the
//! [`Packages`] port.
//!
//! A `macos-pkg` entry carries no attestation, and its `.pkg` is not unpacked here:
//! [`RealPackages::pinned`] fetches it from the entry's locations and the host's mirrors, through
//! the host's stores ([`hostkeeper_package::stores::load`]; a host without the file has none),
//! by its pin alone ([`hostkeeper_package::entry::fetch_pinned`]) into a staging file of the
//! service's package directory, removed when the file is dropped, and the `macos-pkg` installer's
//! `stage` checks its signature ([`crate::macos_pkg`]). Each store that gave other bytes is logged
//! as `store_digest_mismatch`. It sweeps nothing: it runs inside a stage, whose staging directory
//! the daemon's start sweeps.
//!
//! Every failure of the fetch reaches the daemon as one [`PackageError::Refused`] built by
//! [`refusal_of`], the one table that maps the library's reason token onto a [`Refusal`]. The
//! outcome and the class are the library's own.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_config::daemon::Bounds;
use hostkeeper_config::names::ServiceName;
use hostkeeper_package::entry::{PinnedEnv, fetch_pinned};
use hostkeeper_package::ports::Fetcher;
use hostkeeper_package::stores::Stores;
use hostkeeper_package::{
    FETCH_FAILED, NO_SPACE, NO_STORE, PACKAGE_DIGEST_MISMATCH, PackageFailure,
    STORE_DIGEST_MISMATCH,
};
use hostkeeper_proto::hostkeeper::release::v1::Target;
use hostkeeper_steps::Clock;

use crate::packages::{PackageError, PackageRefusal, Packages, Pinned};
use crate::refusal::Refusal;

/// The library token of a fetch, and the refusal it is reported as.
const PACKAGE_TOKENS: &[(&str, Refusal)] = &[
    (PACKAGE_DIGEST_MISMATCH, Refusal::DigestMismatch),
    (FETCH_FAILED, Refusal::FetchFailed),
    (NO_SPACE, Refusal::NoSpace),
    (NO_STORE, Refusal::NoStore),
    (STORE_DIGEST_MISMATCH, Refusal::StoreDigestMismatch),
];

/// The one mapping of a failure of the fetch onto the daemon's closed set: the refusal from the
/// library's reason token (`package_failed` for a token the library added since), the outcome and
/// the class from the library.
pub fn refusal_of(failed: PackageFailure) -> PackageRefusal {
    let refusal = PACKAGE_TOKENS
        .iter()
        .find(|(known, _)| *known == failed.reason)
        .map_or(Refusal::PackageFailed, |(_, refusal)| *refusal);
    PackageRefusal {
        refusal,
        outcome: failed.outcome,
        class: failed.class,
        detail: failed.to_string(),
    }
}

/// Where the store writes a line for the log: a store that gave other bytes than an entry names.
pub type Log = Arc<dyn Fn(&str) + Send + Sync>;

/// Fetches an entry's artifact by its pin with the real library.
pub struct RealPackages {
    fetcher: Box<dyn Fetcher + Send + Sync>,
    coordinator: Option<Box<dyn Fetcher + Send + Sync>>,
    clock: Arc<dyn Clock>,
    bounds: Bounds,
    stores: Stores,
    log: Log,
    root: PathBuf,
}

impl RealPackages {
    /// A store over the packages directory `root`, which exists, resolving the locations of an
    /// entry through `stores`, the host's `stores.toml`, within the `bounds` of the daemon config.
    pub fn new(
        fetcher: Box<dyn Fetcher + Send + Sync>,
        clock: Arc<dyn Clock>,
        bounds: Bounds,
        stores: Stores,
        root: &Path,
    ) -> RealPackages {
        RealPackages {
            fetcher,
            coordinator: None,
            clock,
            bounds,
            stores,
            log: Arc::new(|line| eprintln!("hostkeeper daemon: {line}")),
            root: root.to_path_buf(),
        }
    }

    /// The same store, asking the locations of the `coordinator` store through `fetcher`
    /// ([`crate::coordinator_store::CoordinatorFetcher`]); without it such a location is
    /// `no_store`.
    pub fn with_coordinator(self, fetcher: Box<dyn Fetcher + Send + Sync>) -> RealPackages {
        RealPackages {
            coordinator: Some(fetcher),
            ..self
        }
    }

    /// The same store, writing its log lines (a `store_digest_mismatch` for each store that gave
    /// other bytes than an entry names) through `log`; they go to standard error otherwise.
    pub fn with_log(self, log: Log) -> RealPackages {
        RealPackages { log, ..self }
    }
}

impl Packages for RealPackages {
    fn pinned(&self, service: &ServiceName, entry: &Target) -> Result<Pinned, PackageError> {
        let env = PinnedEnv {
            fetcher: &*self.fetcher,
            clock: &*self.clock,
            bounds: &self.bounds,
            coordinator: self
                .coordinator
                .as_deref()
                .map(|fetcher| fetcher as &dyn Fetcher),
            stores: &self.stores,
            packages_root: &self.root,
        };
        let pinned = fetch_pinned(&env, service, entry)
            .map_err(|failed| PackageError::Refused(refusal_of(failed)))?;
        for store in &pinned.other_bytes {
            (self.log)(&format!(
                "{STORE_DIGEST_MISMATCH}: the store {store} gave other bytes than the entry names for {service}"
            ));
        }
        let path = pinned.path().to_path_buf();
        Ok(Pinned::new(&path, Box::new(pinned)))
    }
}
