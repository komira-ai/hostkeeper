//! Step 18 of a `force` (design sections 4.7 and 5.6): once a force moved `current`, every
//! version directory of the service but the ones `current` and `previous` name is deleted, with
//! its seal, under the service's lock, whatever the force's verdict.
//!
//! The prune itself is not in this repository yet. Until it is, a force calls this port at that
//! one place, with the service's lock held, and the production port, [`NoPrune`], deletes
//! nothing. A prune that fails never changes the force's verdict.

use hostkeeper_config::names::ServiceName;

use crate::locks::ServiceGuard;

/// Why a prune failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the old versions of {service} could not be pruned: {why}")]
pub struct PruneError {
    pub service: String,
    pub why: String,
}

/// Deletes the version directories a switch of `current` leaves behind.
pub trait Prune: Send + Sync {
    /// Deletes every version directory of `service`, with its seal, but the ones `current` and
    /// `previous` name. `held` is the service's lock, which the caller holds for the whole call.
    fn prune(&self, service: &ServiceName, held: &ServiceGuard<'_>) -> Result<(), PruneError>;
}

/// The prune of this build: it deletes nothing.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoPrune;

impl Prune for NoPrune {
    fn prune(&self, _service: &ServiceName, _held: &ServiceGuard<'_>) -> Result<(), PruneError> {
        Ok(())
    }
}
