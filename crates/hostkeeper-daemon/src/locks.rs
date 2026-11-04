//! The daemon's locks (design section 8.1): one per service, taken for the whole of an order and
//! never waited for, and one step lock for the host, held while a step runs.
//!
//! The order is service, then step, and the types keep it: the step lock is taken only through
//! [`ServiceGuard::step`] ([`StepLock`] has no other way to be taken), so no code can hold it
//! without holding a service's lock first, and the step guard cannot outlive the service guard
//! it was taken through.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use hostkeeper_config::names::{ServiceName, Version};

use crate::verb::{HookedVerb, PackageDigest};

/// What a service's lock is held for: the verb and the package of the order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Package {
    pub verb: HookedVerb,
    pub version: Version,
    pub digest: PackageDigest,
}

/// What a service's lock is held for: an order of a package, or the service's uninstall.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Holder {
    Package(Package),
    Uninstall,
}

impl fmt::Display for Holder {
    /// `an order to update it to 2.0.0`, `an order to uninstall it`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Holder::Package(package) => write!(
                f,
                "an order to {} it to {}",
                package.verb.name(),
                package.version
            ),
            Holder::Uninstall => f.write_str("an order to uninstall it"),
        }
    }
}

/// Who holds a service's lock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    pub holder: Holder,
    /// The order's step could not be stopped: the lock stays held until `abort` clears it.
    pub stuck: bool,
}

/// One lock per service, made when a service's first order comes. A clone shares the locks: a
/// post-success watcher's thread keeps a service locked through it.
#[derive(Clone, Debug, Default)]
pub struct ServiceLocks {
    held: Arc<Mutex<BTreeMap<String, Held>>>,
}

impl ServiceLocks {
    fn map(&self) -> MutexGuard<'_, BTreeMap<String, Held>> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Takes the lock of `service` for `package`; never waits. `Err` says who holds it.
    pub fn take(&self, service: &ServiceName, package: Package) -> Result<ServiceGuard<'_>, Held> {
        self.take_for(service, Holder::Package(package))
    }

    /// Takes the lock of `service` for `holder`; never waits. `Err` says who holds it.
    pub fn take_for(
        &self,
        service: &ServiceName,
        holder: Holder,
    ) -> Result<ServiceGuard<'_>, Held> {
        let mut map = self.map();
        if let Some(held) = map.get(service.as_str()) {
            return Err(held.clone());
        }
        let held = Held {
            holder,
            stuck: false,
        };
        map.insert(service.to_string(), held);
        Ok(ServiceGuard {
            locks: self,
            service: service.clone(),
            kept: false,
        })
    }

    /// Who holds the lock of `service` now, `None` when nobody does.
    pub fn holder(&self, service: &ServiceName) -> Option<Held> {
        self.map().get(service.as_str()).cloned()
    }
}

/// A service's lock, held until the guard is dropped (or for good, [`ServiceGuard::keep`]).
#[derive(Debug)]
pub struct ServiceGuard<'a> {
    locks: &'a ServiceLocks,
    service: ServiceName,
    kept: bool,
}

impl ServiceGuard<'_> {
    /// Takes the host's step lock, waiting for it. Only a service guard takes it, so the lock
    /// order is always service then step.
    pub fn step<'s>(&'s self, lock: &'s StepLock) -> StepGuard<'s> {
        StepGuard {
            _held: lock.0.lock().unwrap_or_else(PoisonError::into_inner),
        }
    }

    /// Leaves the lock held, marked stuck, after this guard is gone: the order's step could not
    /// be stopped, and the service takes no other order until `abort` clears it.
    pub fn keep(mut self) {
        self.kept = true;
        self.locks
            .map()
            .entry(self.service.to_string())
            .and_modify(|held| held.stuck = true);
    }
}

impl Drop for ServiceGuard<'_> {
    fn drop(&mut self) {
        if !self.kept {
            self.locks.map().remove(self.service.as_str());
        }
    }
}

/// The host's step lock: one step runs at a time.
#[derive(Debug, Default)]
pub struct StepLock(Mutex<()>);

/// The step lock, held while a step runs.
#[derive(Debug)]
pub struct StepGuard<'s> {
    _held: MutexGuard<'s, ()>,
}
