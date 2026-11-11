//! The push policy with a sender and a clock: what the queue (CM11a) calls when a command is
//! queued, a command times out or answers `NotNow`, the floor elapses, or a Mac connects.
//!
//! The policy decides ([`crate::policy`]); this sends, with the priority and the expiration of
//! the settings. `apns-expiration` is an absolute time: APNs keeps a push until it, so the header
//! is the clock's now plus `push_expiry_s`, and `0` (try once, keep nothing) when the setting is 0.
//! Whether a push that failed is tried again is the policy's: it left the floor armed.

use std::sync::{Arc, Mutex, PoisonError};

use crate::apns::{Delivered, Push, PushError, PushSender};
use crate::clock::Clock;
use crate::policy::PushPolicy;
use crate::token::DeviceToken;

/// The settings a push reads: `push_retry_s`, `push_expiry_s` and `push_priority`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PushSettings {
    /// The floor between pushes to a Mac that has not connected, in seconds.
    pub retry_s: u64,
    /// How long APNs keeps a push, in seconds; 0 for once.
    pub expiry_s: u64,
    /// `apns-priority`: 10 or 5.
    pub priority: u8,
}

/// Who a push is for.
#[derive(Clone, Copy, Debug)]
pub struct Target<'a> {
    /// The enrollment's token.
    pub token: &'a DeviceToken,
    /// Its `PushMagic`.
    pub magic: &'a str,
}

/// What a call did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A push was sent and APNs took it.
    Sent(Delivered),
    /// The policy sent none: one is outstanding, or the floor is not due.
    Held,
    /// A push was sent and failed. The caller acts on the error (a dead token, the setup alert).
    Failed(PushError),
}

/// The pusher.
pub struct Pusher {
    sender: Arc<dyn PushSender>,
    clock: Arc<dyn Clock>,
    topic: String,
    policy: Mutex<PushPolicy>,
}

impl Pusher {
    /// A pusher that sends through `sender` to the topic `topic`, by `clock`.
    #[must_use]
    pub fn new(sender: Arc<dyn PushSender>, clock: Arc<dyn Clock>, topic: &str) -> Self {
        Self {
            sender,
            clock,
            topic: topic.to_owned(),
            policy: Mutex::new(PushPolicy::new(0)),
        }
    }

    fn policy(&self, settings: PushSettings) -> std::sync::MutexGuard<'_, PushPolicy> {
        let mut policy = self.policy.lock().unwrap_or_else(PoisonError::into_inner);
        policy.set_retry_s(settings.retry_s);
        policy
    }

    /// A command was queued for `id`: pushed at once unless a push is outstanding.
    pub async fn command_queued(
        &self,
        id: &str,
        target: Target<'_>,
        settings: PushSettings,
    ) -> Outcome {
        let now = self.clock.now_unix();
        let push = self.policy(settings).command_queued(id, now);
        self.finish(push, id, target, settings, now).await
    }

    /// A command of `id` was returned to the queue by the timeout: pushed again.
    pub async fn command_timed_out(
        &self,
        id: &str,
        target: Target<'_>,
        settings: PushSettings,
    ) -> Outcome {
        let now = self.clock.now_unix();
        let push = self.policy(settings).command_timed_out(id, now);
        self.finish(push, id, target, settings, now).await
    }

    /// The floor of `id` may have elapsed (`has_queued`: a command is queued): pushed again when
    /// it has.
    pub async fn floor_check(
        &self,
        id: &str,
        target: Target<'_>,
        has_queued: bool,
        settings: PushSettings,
    ) -> Outcome {
        let now = self.clock.now_unix();
        let push = self.policy(settings).floor_elapsed(id, now, has_queued);
        self.finish(push, id, target, settings, now).await
    }

    /// `id` answered `NotNow`: the floor is armed.
    pub fn command_deferred(&self, id: &str, settings: PushSettings) {
        let now = self.clock.now_unix();
        self.policy(settings).command_deferred(id, now);
    }

    /// A request of `id` reached `/mdm`: its push, if any, is answered.
    pub fn mac_connected(&self, id: &str) {
        self.policy
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .mac_connected(id);
    }

    /// The enrollment `id` is gone.
    pub fn forget(&self, id: &str) {
        self.policy
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .forget(id);
    }

    /// When the first armed floor elapses, for the timer of the caller.
    #[must_use]
    pub fn next_floor(&self) -> Option<u64> {
        self.policy
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .next_floor()
    }

    /// One push to `target` that the policy knows nothing of: it neither waits for a floor nor arms
    /// one, and leaves no push outstanding. It is how the setup proves the push certificate (the
    /// topic, the environment and a token) without disturbing the pushes of the queue. APNs keeps
    /// it for no time (`apns-expiration` 0): a test push that the Mac gets later proves nothing.
    ///
    /// # Errors
    /// [`PushError`]: what APNs, or the connection to it, answered.
    pub async fn test_push(
        &self,
        target: Target<'_>,
        settings: PushSettings,
    ) -> Result<Delivered, PushError> {
        self.sender
            .send(&Push {
                token: target.token.clone(),
                magic: target.magic.to_owned(),
                topic: self.topic.clone(),
                priority: settings.priority,
                expiration_unix: 0,
            })
            .await
    }

    /// Sends the push the policy allowed (`push`), or says it held.
    async fn finish(
        &self,
        push: bool,
        id: &str,
        target: Target<'_>,
        settings: PushSettings,
        now: u64,
    ) -> Outcome {
        if !push {
            return Outcome::Held;
        }
        let request = Push {
            token: target.token.clone(),
            magic: target.magic.to_owned(),
            topic: self.topic.clone(),
            priority: settings.priority,
            expiration_unix: if settings.expiry_s == 0 {
                0
            } else {
                now.saturating_add(settings.expiry_s)
            },
        };
        match self.sender.send(&request).await {
            Ok(delivered) => Outcome::Sent(delivered),
            Err(why) => {
                self.policy(settings).push_failed(id);
                Outcome::Failed(why)
            }
        }
    }
}

#[cfg(test)]
#[path = "pusher_tests.rs"]
mod tests;
