//! The daemon's locks (design sections 8.1 and 8.7): one per service, taken for the whole of an
//! order and never waited for, and one step lock for the host, held while a step runs. A `force`
//! takes its service's lock only, never the step lock.
//!
//! The order is service, then step, and the types keep it: the step lock is taken only through
//! [`ServiceGuard::step`] ([`StepLock`] has no other way to be taken), so no code can hold it
//! without holding a service's lock first, and the step guard cannot outlive the service guard
//! it was taken through.

use std::collections::BTreeMap;
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

/// Who holds a service's lock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    pub package: Package,
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
        let mut map = self.map();
        if let Some(held) = map.get(service.as_str()) {
            return Err(held.clone());
        }
        let held = Held {
            package,
            stuck: false,
        };
        map.insert(service.to_string(), held);
        Ok(ServiceGuard {
            locks: self,
            service: service.clone(),
            kept: false,
        })
    }

    /// Takes the lock of `service` for `package` when it is free, or when it is held for good by
    /// a step that could not be stopped ([`ServiceGuard::keep`]): a `force` that stopped that
    /// step's unit takes the lock over (design section 5.6, steps 4 to 7). Never waits; `Err`
    /// says who holds it otherwise.
    pub fn take_over(
        &self,
        service: &ServiceName,
        package: Package,
    ) -> Result<ServiceGuard<'_>, Held> {
        let mut map = self.map();
        if let Some(held) = map.get(service.as_str()).filter(|held| !held.stuck) {
            return Err(held.clone());
        }
        let held = Held {
            package,
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
