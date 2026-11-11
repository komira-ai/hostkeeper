//! The lease the MDM timers run under (`docs/design/coordinator-mdm.md`, sections 4.5, 4.9 and
//! 4.10, task CM19): of the coordinators on one database only the one that leads the store acts
//! on the outside world for the MDM, so that two do not both push to APNs, sync ABM or renew the
//! certificate.
//!
//! **What the lease is.** The store's own: the coordinator that holds the database's advisory
//! lock at its epoch (`docs/design/coordinator-storage.md`, sections 4.4 and 5) leads, and its
//! writing session is [`Health::Up`]. A lost session ([`Health::Lost`]) or a store another
//! coordinator led ([`Health::Deposed`], after which the coordinator exits) is a lease not held.
//! No second lock is made: the lease that fences the store's writes is the one that fences the
//! timers, so the two cannot disagree. The files have one writer at all times (`flock`), so their
//! lease is always held.
//!
//! **What a holder does.** [`Lease::held`] is asked before each act that reaches outside: a push
//! to APNs ([`crate::mdm_server::queue`]), a pass of the ABM sync ([`crate::abm::sync_every`]),
//! and the renewal of the TLS certificate (CM17 asks it too, when it lands). A node that does not
//! hold it does nothing and asks again at the next tick, so a node whose session is revived goes
//! on by itself, and one that is not the leader stays quiet. An act that began while the lease
//! was held is not cut short: the writes it makes to the store are fenced by the database
//! (`lease_fenced`), what it sent to Apple is idempotent (a device list read, a profile that
//! is defined again by digest, a push that is a hint), and the next tick finds the lease gone.
//!
//! The secrets a second node needs are in the store, sealed under the one key file that every
//! node is given (`--mdm-secrets-key-file`, section 4.9): a node without it refuses to start when
//! the store holds a sealed row ([`crate::mdm_secrets::MasterKey::load`]).

use std::fmt;
use std::sync::Arc;

use crate::store::{Health, Shared};

/// Whether this node may act for the MDM now.
#[derive(Clone)]
pub(crate) struct Lease(Arc<dyn Fn() -> bool + Send + Sync>);

impl Lease {
    /// A lease that is held while `held` answers true.
    pub(crate) fn new(held: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        Self(Arc::new(held))
    }

    /// The store's lease: held while `backend` takes writes.
    pub(crate) fn of_store(backend: Shared) -> Self {
        Self::new(move || backend.health() == Health::Up)
    }

    /// Whether the lease is held now.
    pub(crate) fn held(&self) -> bool {
        (self.0)()
    }
}

impl fmt::Debug for Lease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Lease").finish_non_exhaustive()
    }
}

#[cfg(test)]
pub(crate) mod seat;

#[cfg(test)]
#[path = "mdm_lease_tests.rs"]
mod tests;
