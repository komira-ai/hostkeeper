//! A seat for tests: one lease that two or more fake nodes contend for, held by the node the test
//! names, and taken away when the test says the holder's link was lost.

use std::sync::{Arc, Mutex, PoisonError};

use super::Lease;

/// The one lease of a database, and who holds it.
#[derive(Clone, Debug, Default)]
pub(crate) struct Seat(Arc<Mutex<Option<&'static str>>>);

impl Seat {
    /// A seat nobody holds.
    pub(crate) fn empty() -> Self {
        Self::default()
    }

    /// The lease of the node `name`: held while the seat is that node's.
    pub(crate) fn node(&self, name: &'static str) -> Lease {
        let seat = Arc::clone(&self.0);
        Lease::new(move || *seat.lock().unwrap_or_else(PoisonError::into_inner) == Some(name))
    }

    /// `name` leads from now on, whoever did before.
    pub(crate) fn give_to(&self, name: &'static str) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(name);
    }

    /// Nobody leads: the holder's link was lost.
    pub(crate) fn lose(&self) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}
