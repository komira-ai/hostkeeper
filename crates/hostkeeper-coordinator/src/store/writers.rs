//! A store's writers' lock, which the thread that holds it may take again
//! (`docs/design/coordinator-multi-instance.md`, section 5.2).
//!
//! A store makes each change under its writers' lock and only then begins its edit, which may
//! wait for the rollout gate. A heartbeat's deciding transaction holds the gate while the stores
//! make the changes the decision names, so it takes the writers' locks of the stores it may
//! change (the desired store's, then the operations store's, the order every change takes them
//! in) before it asks for the gate: no thread then holds the gate while it waits for a lock that
//! a thread waiting for the gate holds. The stores' own changes inside the decision take the
//! same locks again on the same thread, which this lock lets them do.

use std::sync::{Condvar, Mutex, PoisonError};
use std::thread::{self, ThreadId};

/// Who holds the lock, and how many times over.
#[derive(Debug, Default)]
struct Held {
    by: Option<(ThreadId, usize)>,
    /// Threads waiting for it, which a test reads to know one waits.
    waiting: usize,
}

/// A writers' lock that its holder may take again.
#[derive(Debug, Default)]
pub(crate) struct Writers {
    held: Mutex<Held>,
    freed: Condvar,
}

/// The lock, held until this is dropped.
#[derive(Debug)]
pub(crate) struct Writing<'a> {
    writers: &'a Writers,
}

impl Writers {
    /// Takes the lock, waiting while another thread holds it; at once when this thread does.
    pub(crate) fn lock(&self) -> Writing<'_> {
        let me = thread::current().id();
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        while held.by.is_some_and(|(by, _)| by != me) {
            held.waiting += 1;
            held = self
                .freed
                .wait(held)
                .unwrap_or_else(PoisonError::into_inner);
            held.waiting -= 1;
        }
        Self::take(&mut held, me);
        Writing { writers: self }
    }

    /// Takes the lock when no thread holds it, this one included: a store's read of its part
    /// again is skipped while any change holds the lock, and a change on this very thread may be
    /// between its read and its commit.
    pub(crate) fn try_lock(&self) -> Option<Writing<'_>> {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        if held.by.is_some() {
            return None;
        }
        Self::take(&mut held, thread::current().id());
        Some(Writing { writers: self })
    }

    fn take(held: &mut Held, me: ThreadId) {
        let depth = held.by.map_or(0, |(_, depth)| depth);
        held.by = Some((me, depth + 1));
    }

    /// How many threads wait for the lock.
    #[cfg(test)]
    pub(crate) fn waiting(&self) -> usize {
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .waiting
    }
}

impl Drop for Writing<'_> {
    fn drop(&mut self) {
        let mut held = self
            .writers
            .held
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        held.by = match held.by {
            Some((by, depth)) if depth > 1 => Some((by, depth - 1)),
            _ => None,
        };
        if held.by.is_none() {
            self.writers.freed.notify_all();
        }
    }
}

#[cfg(test)]
#[path = "writers_tests.rs"]
mod tests;
