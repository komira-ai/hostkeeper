//! `Releases.WaitConverged`: a read of a service's convergence that the coordinator holds until
//! it ends the wait.
//!
//! The call is answered at once when the convergence it reads ends the wait already, and then
//! after every request the router answers ([`Waits::changed`]): a heartbeat, an operator's change
//! and CI's set each reach the coordinator through [`crate::grpc::route`], so no change of the
//! fleet, the desired state or the ops state passes unseen. A read ends the wait with
//! `SUPERSEDED` when the desired version is not the caller's (the first check, as the
//! `hostkeeper ci` of older releases makes it), `CONVERGED` when the state is converged, and
//! `HALTED` when the service is halted; `BLOCKED` and `IN_PROGRESS` keep it waiting. At
//! `wait_secs` it is read once more and answered with what that read says, `DEADLINE` when it is
//! still `BLOCKED` or `IN_PROGRESS`. A wait reads memory only and writes nothing, no audit line
//! among it.
//!
//! The limits: at most [`MAX_WAIT_SECS`] a call, and at most [`MAX_WAITS`] held at once by a
//! coordinator, which serves the operator API on its one private listener; the next is refused.
//! A held call outlives the two bounds every other call has: its gRPC deadline is moved to its
//! `wait_secs` plus one patience once it is held ([`CallLimit`]), and an HTTP/2 connection that
//! carries it is closed one patience after the last such call on it ends ([`ConnectionWaits`])
//! rather than two patiences after its handshake.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use hostkeeper_proto::hostkeeper::operator::v1::WaitOutcome;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio::time::Instant;

use crate::converge::{Convergence, State};
use crate::service::Coordinator;
use crate::services::ServiceDef;

/// The longest a caller may ask a wait to be held: 30 minutes, the default budget of
/// `set-desired.yml`'s wait (`max_polls` 180, in tens of seconds).
pub const MAX_WAIT_SECS: u32 = 1800;

/// The most waits a coordinator holds at once: as many calls as one HTTP/2 connection may carry
/// ([`crate::listen::MAX_STREAMS`]), a small share of a listener's connections.
pub const MAX_WAITS: usize = crate::listen::MAX_STREAMS as usize;

/// Why a wait was not held.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WaitRefused {
    /// The coordinator manages no such service.
    #[error("no such service")]
    UnknownService,
    /// [`MAX_WAITS`] waits are held already.
    #[error("too many waits are held")]
    TooMany,
}

/// The waits of one coordinator: the signal that something may have changed, and the slots.
pub(crate) struct Waits {
    /// Counts the requests answered; a held wait reads again at every count.
    changes: watch::Sender<u64>,
    /// One permit per held wait.
    slots: Arc<Semaphore>,
    /// Told, by a test, the state each read of a wait found when the wait goes on: a progress
    /// event, so a test makes its change after the wait has read rather than waiting for it.
    #[cfg(test)]
    reads: std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<State>>>,
}

impl Default for Waits {
    fn default() -> Self {
        Self {
            changes: watch::Sender::new(0),
            slots: Arc::new(Semaphore::new(MAX_WAITS)),
            #[cfg(test)]
            reads: std::sync::Mutex::new(None),
        }
    }
}

impl Waits {
    /// Tells every held wait to read its convergence again.
    pub(crate) fn changed(&self) {
        self.changes.send_modify(|n| *n = n.wrapping_add(1));
    }

    /// Tells `reads` the state each read of a wait finds when the wait goes on.
    #[cfg(test)]
    pub(crate) fn tell_reads(&self, reads: tokio::sync::mpsc::UnboundedSender<State>) {
        *self
            .reads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(reads);
    }

    #[cfg(test)]
    fn read(&self, state: State) {
        let reads = self
            .reads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(reads) = reads.as_ref() {
            let _ = reads.send(state);
        }
    }
}

/// A wait the coordinator holds: its service and its slot, given back when it is dropped.
pub(crate) struct Held<'a> {
    def: &'a ServiceDef,
    _slot: OwnedSemaphorePermit,
}

/// What a read of `convergence` says of a wait for `version`: its end, or `None` to go on.
pub(crate) fn outcome_of(convergence: &Convergence, version: &str) -> Option<WaitOutcome> {
    if convergence.desired.as_ref().map(|v| v.as_str()) != Some(version) {
        return Some(WaitOutcome::Superseded);
    }
    match convergence.state {
        State::Converged => Some(WaitOutcome::Converged),
        State::Halted => Some(WaitOutcome::Halted),
        State::Blocked | State::InProgress => None,
    }
}

impl Coordinator {
    /// A slot for a wait on `service`.
    ///
    /// # Errors
    /// [`WaitRefused::UnknownService`]: the coordinator manages no such service;
    /// [`WaitRefused::TooMany`]: [`MAX_WAITS`] waits are held already.
    pub(crate) fn hold_wait(&self, service: &str) -> Result<Held<'_>, WaitRefused> {
        let def = self
            .services
            .get(service)
            .ok_or(WaitRefused::UnknownService)?;
        let slot = Arc::clone(&self.waits.slots)
            .try_acquire_owned()
            .map_err(|_| WaitRefused::TooMany)?;
        Ok(Held { def, _slot: slot })
    }

    /// Waits, for at most `wait` (on tokio's clock), until a read of the convergence of the
    /// service `held` waits on ends the wait for `version` ([`outcome_of`]), and answers how it
    /// ended with the convergence that read found. At `wait` it reads once more: `DEADLINE`
    /// unless that read ends the wait.
    pub(crate) async fn wait_converged(
        &self,
        held: &Held<'_>,
        version: &str,
        wait: Duration,
    ) -> (WaitOutcome, Convergence) {
        let deadline = Instant::now() + wait;
        // Subscribed before the first read: a change after it is seen by the next.
        let mut changes = self.waits.changes.subscribe();
        loop {
            let convergence = self.convergence_of_def(held.def);
            if let Some(outcome) = outcome_of(&convergence, version) {
                return (outcome, convergence);
            }
            #[cfg(test)]
            self.waits.read(convergence.state);
            tokio::select! {
                Ok(()) = changes.changed() => {}
                () = tokio::time::sleep_until(deadline) => {
                    let convergence = self.convergence_of_def(held.def);
                    let outcome = outcome_of(&convergence, version).unwrap_or(WaitOutcome::Deadline);
                    return (outcome, convergence);
                }
            }
        }
    }
}

/// The limit of one gRPC call: one patience from when it came in, moved to its wait plus one
/// patience when it is held ([`Self::hold_for`]). The router puts it in the request's
/// extensions; a call that does not move it is cut at one patience, as every call is.
#[derive(Clone)]
pub(crate) struct CallLimit {
    started: Instant,
    patience: Duration,
    until: Arc<watch::Sender<Instant>>,
}

impl CallLimit {
    /// The limit of a call that comes in now: one `patience`.
    pub(crate) fn new(patience: Duration) -> Self {
        let started = Instant::now();
        Self {
            started,
            patience,
            until: Arc::new(watch::Sender::new(started + patience)),
        }
    }

    /// Moves the limit to `wait` plus one patience from when the call came in.
    pub(crate) fn hold_for(&self, wait: Duration) {
        self.until.send_replace(self.started + wait + self.patience);
    }

    /// The output of `serving`, or `None` when the limit, as it stands then, passes first.
    pub(crate) async fn bound<F: Future>(&self, serving: F) -> Option<F::Output> {
        let mut until = self.until.subscribe();
        let mut serving = std::pin::pin!(serving);
        loop {
            let limit = *until.borrow_and_update();
            tokio::select! {
                answered = &mut serving => return Some(answered),
                () = tokio::time::sleep_until(limit) => return None,
                Ok(()) = until.changed() => {}
            }
        }
    }
}

/// The held waits of one connection: the listener puts it in every request's extensions, and a
/// held wait takes a [`ConnectionHold`] of it, so the connection is kept while it holds one.
#[derive(Clone, Default)]
pub(crate) struct ConnectionWaits(Arc<watch::Sender<usize>>);

/// One held wait of a connection, counted until it is dropped.
pub(crate) struct ConnectionHold(Arc<watch::Sender<usize>>);

impl ConnectionWaits {
    /// Counts one more held wait, until the hold is dropped.
    pub(crate) fn hold(&self) -> ConnectionHold {
        self.0.send_modify(|n| *n += 1);
        ConnectionHold(Arc::clone(&self.0))
    }

    /// Ends once the connection has held no wait for `patience` together: at once and for good
    /// after `patience` for a connection that never held one, `patience` after its last wait
    /// ended otherwise.
    pub(crate) async fn quiet_for(&self, patience: Duration) {
        let mut held = self.0.subscribe();
        loop {
            drop(held.wait_for(|n| *n == 0).await);
            tokio::select! {
                () = tokio::time::sleep(patience) => return,
                Ok(()) = held.changed() => {}
            }
        }
    }
}

impl Drop for ConnectionHold {
    fn drop(&mut self) {
        self.0.send_modify(|n| *n -= 1);
    }
}

/// The output of `serving`, or `None` when `cut` ends first.
pub(crate) async fn unless<F: Future>(
    cut: impl Future<Output = ()>,
    serving: F,
) -> Option<F::Output> {
    tokio::select! {
        biased;
        answered = serving => Some(answered),
        () = cut => None,
    }
}

#[cfg(test)]
#[path = "wait_tests.rs"]
mod tests;
