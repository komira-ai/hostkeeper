//! The in-process command queue that replaces NanoHUB's (`docs/design/coordinator-mdm.md`,
//! sections 4.3 and 5.2): the gate puts a command in ([`CommandQueue`]), the Mac takes it on
//! `/mdm` ([`MdmQueue::claim`]), its answer comes back ([`MdmQueue::answer`]), and a push wakes a
//! Mac that is asleep.
//!
//! The rules are the ones the model check of the queue (`crates/hostkeeper-mdm-model`, CM4) holds
//! over every schedule it explores. The table store ([`MdmStore`]) keeps the ones about a row (a
//! claim takes only a `Queued` command of a live enrollment, an answer changes only a `Sent`
//! command through the enrollment it was sent to, an enqueue of an id that exists changes nothing,
//! an enqueue for an enrollment that is not live is refused); this keeps the ones about the
//! connection and the push:
//!
//! - a claim skips the commands already tried on the connection (the caller keeps the list);
//! - `NotNow` keeps the command and arms the push floor, so it is pushed again after `push_retry_s`
//!   and never inside it;
//! - a command the timeout returned to the queue is pushed again;
//! - a request on an enrollment tells the push policy the Mac is awake, so the next command is
//!   pushed at once.
//!
//! Delivery is at least once: a slow Mac whose command the timeout returned sees it twice.
//!
//! Nothing here reads a clock of its own: the times come from the injected clock.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};

use futures::future::BoxFuture;
use hostkeeper_enrollment::validity::UnixClock;
use hostkeeper_mdm_gate::{
    Command as GateCommand, CommandQueue, EnqueueError, EnrollmentId, Queued,
};
use hostkeeper_mdm_push::apns::PushError;
use hostkeeper_mdm_push::pusher::{Outcome, PushSettings, Pusher, Target};
use hostkeeper_mdm_push::token::DeviceToken;

use crate::mdm_setup::MdmSetup;
use crate::store::mdm::{
    Answer, Answered, Command, CommandState, Enqueued, Enrollment, MdmError, MdmStore, NewCommand,
};

/// Where the queue finds the pusher: the one for the push certificate in use, none while there is
/// none.
pub(crate) type Pushers = Arc<dyn Fn() -> Option<Arc<Pusher>> + Send + Sync>;

/// The push settings when the settings cannot be read: the defaults of `mdm_settings`.
const DEFAULT_PUSH: PushSettings = PushSettings {
    retry_s: 3600,
    expiry_s: 86_400,
    priority: 10,
};

/// The queue of the commands for the Macs.
pub(crate) struct MdmQueue {
    store: Arc<dyn MdmStore>,
    clock: Arc<dyn UnixClock>,
    pushers: Pushers,
    setup: Arc<MdmSetup>,
    timeout_s: u64,
    /// The enrollments whose push floor may be armed: the sweep looks at them.
    armed: Mutex<BTreeSet<String>>,
}

impl std::fmt::Debug for MdmQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MdmQueue")
            .field("timeout_s", &self.timeout_s)
            .finish_non_exhaustive()
    }
}

impl MdmQueue {
    /// A queue over `store`. A `Sent` command with no answer after `timeout_s` seconds on
    /// `clock` is queued again by [`Self::sweep`]; `pushers` says who pushes, and the push
    /// settings are read from `setup`.
    pub(crate) fn new(
        store: Arc<dyn MdmStore>,
        clock: Arc<dyn UnixClock>,
        pushers: Pushers,
        setup: Arc<MdmSetup>,
        timeout_s: u64,
    ) -> Self {
        Self {
            store,
            clock,
            pushers,
            setup,
            timeout_s,
            armed: Mutex::new(BTreeSet::new()),
        }
    }

    fn now(&self) -> u64 {
        self.clock.now_unix()
    }

    fn now_i64(&self) -> i64 {
        i64::try_from(self.now()).unwrap_or(i64::MAX)
    }

    fn settings(&self) -> PushSettings {
        self.setup
            .push_settings()
            .map_or(DEFAULT_PUSH, |(settings, _)| settings)
    }

    /// Whether `id` was not armed before.
    fn arm(&self, id: &str) -> bool {
        self.armed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id.to_owned())
    }

    fn disarm(&self, id: &str) {
        self.armed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
    }

    /// Takes the oldest queued command of the live enrollment `enrollment_id` that is not in
    /// `tried`, the commands already delivered or deferred on the connection asking.
    ///
    /// # Errors
    /// [`MdmError::Store`].
    pub(crate) fn claim(
        &self,
        enrollment_id: &str,
        tried: &[String],
    ) -> Result<Option<Command>, MdmError> {
        self.store.claim(enrollment_id, tried, self.now_i64())
    }

    /// A request of `enrollment_id` reached `/mdm`: the Mac is awake, so its push, if one was
    /// outstanding, is answered.
    ///
    /// # Errors
    /// [`MdmError::Store`].
    pub(crate) fn contact(&self, enrollment_id: &str) -> Result<(), MdmError> {
        self.store.record_contact(enrollment_id, self.now_i64())?;
        if let Some(pusher) = (self.pushers)() {
            pusher.mac_connected(enrollment_id);
        }
        Ok(())
    }

    /// The Mac's answer to command `id`, sent to `enrollment_id`. An answer to a command that is
    /// not `Sent` to this enrollment is dropped. `NotNow` keeps the command, queued again, and arms
    /// the push floor.
    ///
    /// # Errors
    /// [`MdmError::Store`].
    pub(crate) fn answer(
        &self,
        enrollment_id: &str,
        id: &str,
        answer: Answer,
        response: &[u8],
    ) -> Result<Answered, MdmError> {
        let taken = self
            .store
            .answer(enrollment_id, id, answer, Some(response), self.now_i64())?;
        if taken == Answered::Taken && answer == Answer::NotNow {
            if let Some(pusher) = (self.pushers)() {
                pusher.command_deferred(enrollment_id, self.settings());
            }
            self.arm(enrollment_id);
        }
        Ok(taken)
    }

    /// The enrollment `enrollment_id` ended: the push policy forgets it.
    pub(crate) fn forget(&self, enrollment_id: &str) {
        self.disarm(enrollment_id);
        if let Some(pusher) = (self.pushers)() {
            pusher.forget(enrollment_id);
        }
    }

    /// Pushes `enrollment_id` for the reason `why`, when the policy allows and the Mac has a push
    /// token. The text of a failure is the answer.
    async fn push(&self, enrollment_id: &str, why: Why) -> Option<String> {
        let pusher = (self.pushers)()?;
        let enrollment = match self.store.enrollment(enrollment_id) {
            Ok(Some(enrollment)) => enrollment,
            Ok(None) => return None,
            Err(why) => return Some(why.to_string()),
        };
        let (token, magic) = target_of(&enrollment)?;
        let target = Target {
            token: &token,
            magic: &magic,
        };
        let settings = self.settings();
        let outcome = match why {
            Why::Queued => pusher.command_queued(enrollment_id, target, settings).await,
            Why::TimedOut => {
                pusher
                    .command_timed_out(enrollment_id, target, settings)
                    .await
            }
            Why::Floor(has_queued) => {
                pusher
                    .floor_check(enrollment_id, target, has_queued, settings)
                    .await
            }
        };
        self.arm(enrollment_id);
        match outcome {
            Outcome::Sent(_) => {
                self.record_push(enrollment_id);
                None
            }
            Outcome::Held => None,
            Outcome::Failed(why) => Some(failure(&why)),
        }
    }

    fn record_push(&self, enrollment_id: &str) {
        if let Err(why) = self.store.record_push(enrollment_id, self.now_i64()) {
            tracing::error!(%why, "a push was sent and could not be recorded");
        }
    }

    /// One look at what is due: the commands the timeout returns to the queue are pushed again,
    /// and a Mac whose push floor elapsed with a command still queued is pushed again. The
    /// coordinator calls it every `--mdm-queue-sweep-s` seconds; a test calls it at the time it
    /// has set.
    ///
    /// # Errors
    /// [`MdmError::Store`]: the timed-out commands could not be returned.
    pub(crate) async fn sweep(&self) -> Result<(), MdmError> {
        let before = self
            .now_i64()
            .saturating_sub(i64::try_from(self.timeout_s).unwrap_or(i64::MAX));
        let returned = self.store.requeue_timed_out(before)?;
        let enrollments: BTreeSet<&str> = returned.iter().map(|(_, e)| e.as_str()).collect();
        for enrollment_id in enrollments {
            if let Some(why) = self.push(enrollment_id, Why::TimedOut).await {
                tracing::warn!(enrollment_id, %why, "a timed-out command could not be pushed");
            }
        }
        // The store, not memory, says who waits: after a restart nothing is armed, and a sleeping
        // Mac calls only when pushed.
        match self.store.enrollments_with_queued() {
            Ok(waiting) => {
                for enrollment_id in &waiting {
                    // Newly known to this process: the push policy has no floor for it either,
                    // so it is armed there too, and the push comes one floor from now.
                    if self.arm(enrollment_id)
                        && let Some(pusher) = (self.pushers)()
                    {
                        pusher.command_deferred(enrollment_id, self.settings());
                    }
                }
            }
            Err(why) => {
                tracing::warn!(%why, "the enrollments with queued commands could not be read")
            }
        }
        let armed: Vec<String> = self
            .armed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .cloned()
            .collect();
        for enrollment_id in armed {
            self.floor(&enrollment_id).await;
        }
        Ok(())
    }

    /// The floor of `enrollment_id`: pushed again when it elapsed and a command is queued;
    /// dropped from the look once the enrollment is gone or has nothing queued.
    async fn floor(&self, enrollment_id: &str) {
        let commands = match self.store.commands(enrollment_id) {
            Ok(commands) => commands,
            Err(why) => {
                tracing::warn!(enrollment_id, %why, "the commands could not be read for a push");
                return;
            }
        };
        let live = matches!(
            self.store.enrollment(enrollment_id),
            Ok(Some(Enrollment {
                state: crate::store::mdm::EnrollmentState::Live,
                ..
            }))
        );
        let has_queued = commands.iter().any(|c| c.state == CommandState::Queued);
        if !live {
            self.forget(enrollment_id);
            return;
        }
        if let Some(why) = self.push(enrollment_id, Why::Floor(has_queued)).await {
            tracing::warn!(enrollment_id, %why, "a push could not be sent");
        }
        if !has_queued {
            self.disarm(enrollment_id);
        }
    }
}

/// Why a push is asked for.
#[derive(Clone, Copy)]
enum Why {
    Queued,
    TimedOut,
    Floor(bool),
}

/// The push token and magic of an enrollment, when it has both and the token is a token.
fn target_of(enrollment: &Enrollment) -> Option<(DeviceToken, String)> {
    let token = DeviceToken::from_raw(enrollment.push_token.as_deref()?).ok()?;
    Some((token, enrollment.push_magic.clone()?))
}

/// The text of a failed push, for the log and for the gate's record of why the push failed.
fn failure(why: &PushError) -> String {
    why.to_string()
}

impl CommandQueue for MdmQueue {
    fn enqueue<'a>(
        &'a self,
        id: &'a EnrollmentId,
        command: &'a GateCommand,
    ) -> BoxFuture<'a, Result<Queued, EnqueueError>> {
        Box::pin(async move {
            if !command.is_allowed() {
                return Err(EnqueueError::NotAllowed(command.request_type().to_owned()));
            }
            let new = NewCommand {
                id: command.uuid().as_str().to_owned(),
                enrollment_id: id.as_str().to_owned(),
                command_type: command.request_type().to_owned(),
                payload: command.to_plist(),
                created_unix: self.now_i64(),
            };
            match self.store.enqueue(&new) {
                Ok(Enqueued::Queued) => Ok(Queued {
                    push_error: self.push(id.as_str(), Why::Queued).await,
                }),
                Ok(Enqueued::Exists) => Ok(Queued { push_error: None }),
                Ok(Enqueued::NotLive) => Err(EnqueueError::NotQueued(
                    "the enrollment is not live".to_owned(),
                )),
                Err(why) => Err(EnqueueError::NotQueued(why.to_string())),
            }
        })
    }
}

#[cfg(test)]
#[path = "queue_tests.rs"]
mod tests;
