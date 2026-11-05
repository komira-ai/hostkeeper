//! The nonces a `Poll` is signed over (the header of `hostkeeper/agent/v1/agent.proto`).
//!
//! The coordinator chooses each nonce, [`NONCE_BYTES`] random bytes, gives it in the answer to a
//! `Poll`, and remembers it for the request it was given for. A signed `Poll` must carry one of
//! them: it is taken when the `Poll` is answered, whatever the answer, so it is good for one
//! `Poll`. A request keeps its newest [`NONCES_PER_REQUEST`] nonces; an older one is forgotten.
//! Nonces are kept in memory only: after a restart a daemon's signed `Poll` is refused
//! `bad_signature`, and it starts again with an unsigned one, as it does after an answer it lost.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Mutex, PoisonError};

use hostkeeper_enrollment::names::KeyHash;

/// The length of a nonce the coordinator gives, in bytes (the header allows 16 to 64).
pub const NONCE_BYTES: usize = 32;

/// How many nonces one request keeps: enough for a daemon that lost an answer or two.
pub const NONCES_PER_REQUEST: usize = 4;

/// Fills a buffer with random bytes; `false` when it could not.
pub type Random = Box<dyn Fn(&mut [u8]) -> bool + Send + Sync>;

/// The operating system's random bytes.
#[must_use]
pub fn os_random() -> Random {
    Box::new(|buffer| getrandom::fill(buffer).is_ok())
}

/// The nonces given and not yet taken, per request.
pub struct Nonces {
    random: Random,
    given: Mutex<BTreeMap<KeyHash, VecDeque<Vec<u8>>>>,
}

impl Nonces {
    /// No nonce given yet; new ones are drawn from `random`.
    #[must_use]
    pub fn new(random: Random) -> Self {
        Self {
            random,
            given: Mutex::new(BTreeMap::new()),
        }
    }

    /// A new nonce for `request`, remembered as good for one `Poll`; the request's oldest is
    /// forgotten when it has more than [`NONCES_PER_REQUEST`]. `None` when no random bytes could
    /// be had.
    pub fn give(&self, request: &KeyHash) -> Option<Vec<u8>> {
        let mut nonce = vec![0; NONCE_BYTES];
        if !(self.random)(&mut nonce) {
            return None;
        }
        let mut given = self.given.lock().unwrap_or_else(PoisonError::into_inner);
        let kept = given.entry(*request).or_default();
        kept.push_back(nonce.clone());
        if kept.len() > NONCES_PER_REQUEST {
            kept.pop_front();
        }
        Some(nonce)
    }

    /// Whether `nonce` was given for `request` and not yet taken. It is taken now: a second
    /// `Poll` with it is refused.
    pub fn take(&self, request: &KeyHash, nonce: &[u8]) -> bool {
        let mut given = self.given.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(kept) = given.get_mut(request) else {
            return false;
        };
        let Some(at) = kept.iter().position(|one| one == nonce) else {
            return false;
        };
        kept.remove(at);
        if kept.is_empty() {
            given.remove(request);
        }
        true
    }
}
