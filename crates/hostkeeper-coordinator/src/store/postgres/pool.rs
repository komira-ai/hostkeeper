//! The pool of writing sessions (`docs/design/coordinator-multi-instance.md`, section 4,
//! "Sessions"; `--database-pool`): one [`Writer`] on each of its own threads, and the writers that
//! are free, so a write runs on a free session and waits only while every session is busy. A
//! write takes the writer that has been free longest, so the writers take turns and a session
//! that the server ended is found by the next writes, not left unused.

use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use super::session::Worker;
use super::writer::Writer;

/// The writers and which of them are free. The last writer is kept for heartbeats' decisions
/// ([`super::decide`]): a decision holds it to its end, and a write the deciding thread makes
/// meanwhile (a record of the enrollment, what a host runs) takes one of the others, so it never
/// waits for the writer its own decision holds. A coordinator decides one heartbeat at a time.
pub(crate) struct Pool {
    writers: Vec<Worker<Writer>>,
    free: Mutex<VecDeque<usize>>,
    /// The decisions' writer, while no decision holds it.
    deciding: Mutex<Option<usize>>,
    /// What a decision waiting for its writer is told by when the writer comes back.
    returned: Condvar,
    freed: Condvar,
    /// How many calls have waited for a free writer: what a test sees a call wait by.
    #[cfg(test)]
    waited: std::sync::atomic::AtomicUsize,
}

/// A writer taken from the pool, given back when the call is over.
struct Taken<'a> {
    pool: &'a Pool,
    index: usize,
}

impl Drop for Taken<'_> {
    fn drop(&mut self) {
        self.pool.give_back(self.index);
    }
}

/// A writer taken from the pool for an edit ([`super::edit`]), given back when the lease is
/// dropped: the edit's job runs on it, and its caller talks to the job while it holds the lease.
pub(crate) struct Lease {
    pool: Arc<Pool>,
    index: usize,
    /// Whether it is the decisions' writer.
    deciding: bool,
}

impl Drop for Lease {
    fn drop(&mut self) {
        match self.deciding {
            true => {
                *self
                    .pool
                    .deciding
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = Some(self.index);
                self.pool.returned.notify_one();
            }
            false => self.pool.give_back(self.index),
        }
    }
}

impl Pool {
    /// A pool of `writers`, each on a thread of its own named after `name`, the last kept for
    /// heartbeats' decisions.
    ///
    /// # Errors
    /// A thread could not be made.
    pub(crate) fn spawn(name: &str, writers: Vec<Writer>) -> io::Result<Pool> {
        let count = writers.len();
        let writers = writers
            .into_iter()
            .enumerate()
            .map(|(i, writer)| Worker::spawn(&format!("{name}-{i}"), writer))
            .collect::<io::Result<Vec<_>>>()?;
        Ok(Pool {
            writers,
            free: Mutex::new((0..count.saturating_sub(1)).collect()),
            deciding: Mutex::new(count.checked_sub(1)),
            returned: Condvar::new(),
            freed: Condvar::new(),
            #[cfg(test)]
            waited: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    fn free(&self) -> MutexGuard<'_, VecDeque<usize>> {
        self.free.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The writer that has been free longest, taken, waiting while none is.
    fn take(&self) -> usize {
        let mut free = self.free();
        loop {
            if let Some(index) = free.pop_front() {
                return index;
            }
            #[cfg(test)]
            self.waited
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            free = self
                .freed
                .wait(free)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Gives the writer `index` back.
    fn give_back(&self, index: usize) {
        self.free().push_back(index);
        // A decision's wait for its own writer shares the condition.
        self.freed.notify_one();
    }

    /// Runs `job` on the writer that has been free longest, waiting while none is, and waits for
    /// its answer; `None` when that writer's thread is gone.
    pub(crate) fn call<R: Send + 'static>(
        &self,
        job: impl FnOnce(&mut Writer) -> R + Send + 'static,
    ) -> Option<R> {
        let taken = Taken {
            pool: self,
            index: self.take(),
        };
        self.writers[taken.index].call(job)
    }

    /// Takes the writer that has been free longest for an edit, waiting while none is, and hands
    /// it `job` without waiting for it: false when that writer's thread is gone. The writer is
    /// the edit's until the lease is dropped.
    pub(crate) fn lease(
        pool: &Arc<Pool>,
        job: impl FnOnce(&mut Writer) + Send + 'static,
    ) -> Option<Lease> {
        let lease = Lease {
            pool: Arc::clone(pool),
            index: pool.take(),
            deciding: false,
        };
        pool.writers[lease.index].post(job).then_some(lease)
    }

    /// [`Self::lease`] of the decisions' writer, waiting while another decision holds it.
    pub(crate) fn lease_deciding(
        pool: &Arc<Pool>,
        job: impl FnOnce(&mut Writer) + Send + 'static,
    ) -> Option<Lease> {
        let mut free = pool.deciding.lock().unwrap_or_else(PoisonError::into_inner);
        let index = loop {
            if let Some(index) = free.take() {
                break index;
            }
            free = pool
                .returned
                .wait(free)
                .unwrap_or_else(PoisonError::into_inner);
        };
        drop(free);
        let lease = Lease {
            pool: Arc::clone(pool),
            index,
            deciding: true,
        };
        pool.writers[lease.index].post(job).then_some(lease)
    }

    /// Hands `job` to every writer, behind the jobs each has already, without waiting: false when
    /// a writer's thread is gone (a job of it panicked), which then never runs it.
    pub(crate) fn post_each(&self, job: fn(&mut Writer)) -> bool {
        // Every writer is handed the job, so none is skipped after one that is gone.
        let delivered = self
            .writers
            .iter()
            .filter(|writer| writer.post(job))
            .count();
        delivered == self.writers.len()
    }

    /// Runs `job` on every writer, behind the jobs each has already, and waits for each answer:
    /// `None` when a writer's thread is gone.
    #[cfg(test)]
    pub(crate) fn call_each<R: Send + 'static>(
        &self,
        job: impl Fn(&mut Writer) -> R + Clone + Send + 'static,
    ) -> Option<Vec<R>> {
        self.writers
            .iter()
            .map(|writer| writer.call(job.clone()))
            .collect()
    }

    /// How many calls have waited for a free writer.
    #[cfg(test)]
    pub(crate) fn waited(&self) -> usize {
        self.waited.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Stops every writer's thread once the jobs handed to it have run, its session dropped (its
    /// lock freed first, [`Writer`]'s drop).
    pub(crate) fn stop(&self) {
        for writer in &self.writers {
            writer.stop();
        }
    }
}
