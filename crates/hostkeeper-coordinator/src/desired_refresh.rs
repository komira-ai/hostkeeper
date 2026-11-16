//! How the desired state in memory takes in what the database holds that it does not: the commit
//! of unknown outcome that turned out landed, and the state another coordinator committed
//! (`docs/design/coordinator-multi-instance.md`, section 5).

use std::sync::PoisonError;

use super::{DesiredStore, Held};

impl DesiredStore {
    /// Takes the desired state of a commit of unknown outcome that `landing` found landed into
    /// memory, with what memory changed since laid over it (`docs/design/coordinator-storage.md`,
    /// section 4.4).
    pub(crate) fn take_landed(&self, landing: &crate::store::Landing) {
        let _writer = self.writing.lock().unwrap_or_else(PoisonError::into_inner);
        let taken = landing.desired_over(&self.lock());
        if let Some(taken) = taken {
            *self.lock() = taken;
        }
    }

    /// Takes `fresh`, the desired state the rows hold, in place of memory when `also` agrees: it
    /// is run under the writers' lock, which a change of the desired state holds from before its
    /// commit to after its memory, so no such change is half done here, and it takes the
    /// operations state in the same step (the lock order is this store's, then the ops store's,
    /// as in a change that writes both). Answers whether memory was replaced.
    pub(crate) fn take_rows(&self, fresh: Held, also: Box<dyn FnOnce() -> bool + '_>) -> bool {
        let _writer = self.writing.lock().unwrap_or_else(PoisonError::into_inner);
        if !also() {
            return false;
        }
        *self.lock() = fresh;
        true
    }
}
