//! The operator's browser sessions and the login limiter, in memory
//! (docs/design/operator-login.md, sections 5.2 and 5.4). No route uses them yet.
//!
//! [`Sessions`] holds one entry per signed-in browser. The session id is 32 random bytes; the
//! table keeps only its SHA-256 and a CSRF token of its own, so a copy of the coordinator's
//! memory does not hold a cookie that works. A session ends at an idle deadline
//! (`--session-idle-secs`, pushed out at each use) or an absolute one (`--session-max-secs`),
//! each judged by the injected [`UnixClock`] with a saturating compare, so a clock that steps
//! back lets a session live longer and never makes one with a past deadline. The table holds at
//! most `--session-max-count` sessions; a new one past the cap drops the one least recently
//! used. A restart ends every session, because nothing here is written anywhere.
//!
//! [`LoginLimiter`] counts consecutive failures per user name, locks a name that reaches
//! `--login-max-failures` for `--login-lockout-secs`, and hands out the password-hash slots of
//! [`HashSlots`] (`--login-max-hashes`). It counts every name it is told of, a user that does
//! not exist as much as one that does, so a lock never says which names are users. A count is
//! forgotten once a lockout length has passed since its last failure, which keeps the table
//! from growing with every name a guesser tries; the table also holds at most
//! `--login-max-tracked` names. Names are the canonical user key, which the caller makes.
//!
//! The flags that give these their settings are wired by the change that adds the login route.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hostkeeper_enrollment::validity::UnixClock;
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq;

use crate::enrollment::Random;
use crate::password::{HashSlot, HashSlots};

/// The random bytes of a session id and of a CSRF token.
const SECRET_BYTES: usize = 32;

/// The deadlines and the cap of the session table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionSettings {
    /// Seconds of no use after which a session ends (`--session-idle-secs`).
    pub idle_secs: u64,
    /// Seconds after creation at which a session ends however used (`--session-max-secs`).
    pub max_secs: u64,
    /// The most sessions held; at least 1 (`--session-max-count`).
    pub max_count: usize,
}

impl Default for SessionSettings {
    fn default() -> Self {
        Self {
            idle_secs: 1800,
            max_secs: 43200,
            max_count: 256,
        }
    }
}

/// The failure counts of the limiter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LimiterSettings {
    /// Consecutive failures that lock a name; at least 1 (`--login-max-failures`).
    pub max_failures: u32,
    /// Seconds a lock lasts (`--login-lockout-secs`).
    pub lockout_secs: u64,
    /// Most names the limiter tracks at once; at least 1 (`--login-max-tracked`).
    pub max_tracked: usize,
}

impl Default for LimiterSettings {
    fn default() -> Self {
        Self {
            max_failures: 5,
            lockout_secs: 900,
            max_tracked: 4096,
        }
    }
}

/// Why a session cannot be made.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SessionError {
    /// The operating system gave no random bytes.
    #[error("no random bytes for a session id")]
    NoRandom,
}

/// A session id: the value of the cookie. It is shown to the browser once and is not in any
/// debug text.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionId(String);

impl SessionId {
    /// The cookie value, base64url without padding.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionId(<redacted>)")
    }
}

/// What a live session says about itself: the answer of `GET /v1/session` and what a request is
/// checked against.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionView {
    /// The user the session signed in.
    pub username: String,
    /// The user must change the password before anything else.
    pub must_change: bool,
    /// When a `must_change` session may no longer change the password (unix seconds).
    pub must_change_until: Option<u64>,
    csrf: String,
}

impl SessionView {
    /// The session's CSRF token, for the body of the login and session answers.
    #[must_use]
    pub fn csrf(&self) -> &str {
        &self.csrf
    }

    /// Whether `presented` is the session's CSRF token, compared in constant time.
    #[must_use]
    pub fn csrf_matches(&self, presented: &str) -> bool {
        bool::from(self.csrf.as_bytes().ct_eq(presented.as_bytes()))
    }
}

impl fmt::Debug for SessionView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionView")
            .field("username", &self.username)
            .field("must_change", &self.must_change)
            .field("must_change_until", &self.must_change_until)
            .field("csrf", &"<redacted>")
            .finish()
    }
}

struct Entry {
    view: SessionView,
    created: u64,
    last_used: u64,
    /// The order of making, to tell apart sessions made and used in one second.
    serial: u64,
}

/// The session table.
pub struct Sessions {
    clock: Arc<dyn UnixClock>,
    random: Random,
    settings: SessionSettings,
    serial: AtomicU64,
    table: Mutex<BTreeMap<[u8; 32], Entry>>,
}

impl fmt::Debug for Sessions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sessions")
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

fn id_hash(id: &str) -> [u8; 32] {
    Sha256::digest(id.as_bytes()).into()
}

impl Sessions {
    /// An empty table. Ids and CSRF tokens are drawn from `random`.
    #[must_use]
    pub fn new(settings: SessionSettings, clock: Arc<dyn UnixClock>, random: Random) -> Self {
        Self {
            clock,
            random,
            settings: SessionSettings {
                max_count: settings.max_count.max(1),
                ..settings
            },
            serial: AtomicU64::new(0),
            table: Mutex::new(BTreeMap::new()),
        }
    }

    fn table(&self) -> MutexGuard<'_, BTreeMap<[u8; 32], Entry>> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn secret(&self) -> Result<String, SessionError> {
        let mut bytes = [0u8; SECRET_BYTES];
        if !(self.random)(&mut bytes) {
            return Err(SessionError::NoRandom);
        }
        Ok(URL_SAFE_NO_PAD.encode(bytes))
    }

    /// A new session for `username`, with its id (for the cookie) and its view. Past the cap the
    /// session least recently used is dropped first; expired sessions go before that.
    ///
    /// # Errors
    ///
    /// [`SessionError::NoRandom`] when the id or the CSRF token cannot be drawn; the table is
    /// then unchanged.
    pub fn create(
        &self,
        username: &str,
        must_change: bool,
        must_change_until: Option<u64>,
    ) -> Result<(SessionId, SessionView), SessionError> {
        let id = self.secret()?;
        let csrf = self.secret()?;
        let now = self.clock.now_unix();
        let view = SessionView {
            username: username.to_owned(),
            must_change,
            must_change_until,
            csrf,
        };
        let mut table = self.table();
        table.retain(|_, entry| !self.expired(entry, now));
        if table.len() >= self.settings.max_count {
            let oldest = table
                .iter()
                .min_by_key(|(_, entry)| (entry.last_used, entry.created, entry.serial))
                .map(|(key, _)| *key);
            oldest.iter().for_each(|key| {
                table.remove(key);
            });
        }
        table.insert(
            id_hash(&id),
            Entry {
                view: view.clone(),
                created: now,
                last_used: now,
                serial: self.serial.fetch_add(1, Ordering::SeqCst),
            },
        );
        Ok((SessionId(id), view))
    }

    /// Whether the session is past its idle or absolute deadline at `now`. The elapsed time
    /// saturates at 0, so a clock that stepped back keeps a session alive and never ends it.
    fn expired(&self, entry: &Entry, now: u64) -> bool {
        now.saturating_sub(entry.last_used) >= self.settings.idle_secs
            || now.saturating_sub(entry.created) >= self.settings.max_secs
    }

    /// The live session of a cookie value, its idle deadline pushed out; None for an unknown,
    /// ended or expired one (which is then forgotten).
    #[must_use]
    pub fn lookup(&self, id: &str) -> Option<SessionView> {
        let now = self.clock.now_unix();
        let key = id_hash(id);
        let mut table = self.table();
        let entry = table.get_mut(&key)?;
        if self.expired(entry, now) {
            table.remove(&key);
            return None;
        }
        entry.last_used = entry.last_used.max(now);
        Some(entry.view.clone())
    }

    /// Ends the session of a cookie value; whether there was one.
    pub fn end(&self, id: &str) -> bool {
        self.table().remove(&id_hash(id)).is_some()
    }

    /// Ends every session of `username`, except the one `keep` names when it is given; how many
    /// ended. A disabled user, a password change or reset, and a user's removal end sessions
    /// this way.
    pub fn end_user(&self, username: &str, keep: Option<&str>) -> usize {
        let kept = keep.map(id_hash);
        let mut table = self.table();
        let before = table.len();
        table.retain(|key, entry| entry.view.username != username || Some(*key) == kept);
        before - table.len()
    }

    /// How many sessions are held, expired ones not yet forgotten included.
    #[must_use]
    pub fn len(&self) -> usize {
        self.table().len()
    }

    /// Whether no session is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.table().is_empty()
    }
}

/// Why a login may not start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    /// The name is locked; the lock ends by itself.
    #[error("the user is locked")]
    Locked,
    /// The most hashes that may run at once are running.
    #[error("no password hash slot is free")]
    Busy,
}

/// What a failure did to the count of its name, for the audit: the log holds the first failure
/// of a run and the lock, not every failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The first failure of a run: audit it.
    First,
    /// A later failure under the limit: not audited.
    Counted,
    /// This failure locked the name: audit the lock.
    Locked,
}

#[derive(Clone, Copy)]
struct Count {
    failures: u32,
    last_failure: u64,
    locked_until: Option<u64>,
}

/// The per-name failure counts, the locks and the hash slots of the login.
pub struct LoginLimiter {
    clock: Arc<dyn UnixClock>,
    settings: LimiterSettings,
    slots: HashSlots,
    counts: Mutex<BTreeMap<String, Count>>,
}

impl fmt::Debug for LoginLimiter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoginLimiter")
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl LoginLimiter {
    /// A limiter with no failure counted, handing out the slots of `slots`.
    #[must_use]
    pub fn new(settings: LimiterSettings, clock: Arc<dyn UnixClock>, slots: HashSlots) -> Self {
        Self {
            clock,
            settings: LimiterSettings {
                max_failures: settings.max_failures.max(1),
                max_tracked: settings.max_tracked.max(1),
                ..settings
            },
            slots,
            counts: Mutex::new(BTreeMap::new()),
        }
    }

    fn counts(&self) -> MutexGuard<'_, BTreeMap<String, Count>> {
        self.counts.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn locked(count: &Count, now: u64) -> bool {
        count.locked_until.is_some_and(|until| now < until)
    }

    /// Whether `username` is locked now. Like every `username` here, it is the canonical user
    /// key: the same normalised, length-checked name the user lookup uses, so that two spellings
    /// of one user share one count and a long name cannot make an entry large. The caller (the
    /// login route) canonicalises and checks the length before it calls the limiter.
    #[must_use]
    pub fn is_locked(&self, username: &str) -> bool {
        let now = self.clock.now_unix();
        self.counts()
            .get(username)
            .is_some_and(|count| Self::locked(count, now))
    }

    /// Leave to run one login's password hash: the slot, to hold while the hash runs. The lock
    /// is checked first and takes no slot.
    ///
    /// # Errors
    ///
    /// [`Refusal::Locked`] for a locked name, [`Refusal::Busy`] when no slot is free. Either
    /// answers 429 and starts no hash.
    pub fn admit(&self, username: &str) -> Result<HashSlot, Refusal> {
        if self.is_locked(username) {
            return Err(Refusal::Locked);
        }
        self.slots.try_acquire().ok_or(Refusal::Busy)
    }

    /// Counts a failed login of `username`. The count that reaches the limit locks the name for
    /// the lockout length. The table holds at most `max_tracked` names; a new name past that drops
    /// an unlocked entry with the oldest last failure first, a locked one only when all are.
    /// A failure that arrives while the name is locked (a hash that was
    /// already running) is counted as a failure and does not move the lock.
    pub fn failed(&self, username: &str) -> Failure {
        let now = self.clock.now_unix();
        let lockout = self.settings.lockout_secs;
        let mut counts = self.counts();
        counts.retain(|_, count| {
            Self::locked(count, now) || now.saturating_sub(count.last_failure) < lockout
        });
        if !counts.contains_key(username) && counts.len() >= self.settings.max_tracked {
            // Full: drop the entry that matters least, an unlocked one before a locked one and
            // the one with the oldest last failure first.
            let drop = counts
                .iter()
                .min_by_key(|(key, count)| (Self::locked(count, now), count.last_failure, *key))
                .map(|(key, _)| key.clone());
            if let Some(key) = drop {
                counts.remove(&key);
            }
        }
        let count = counts.entry(username.to_owned()).or_insert(Count {
            failures: 0,
            last_failure: now,
            locked_until: None,
        });
        if count.locked_until.is_some_and(|until| until <= now) {
            count.failures = 0;
            count.locked_until = None;
        }
        count.failures = count.failures.saturating_add(1);
        count.last_failure = count.last_failure.max(now);
        if Self::locked(count, now) {
            return Failure::Counted;
        }
        if count.failures >= self.settings.max_failures {
            count.locked_until = Some(now.saturating_add(lockout));
            Failure::Locked
        } else if count.failures == 1 {
            Failure::First
        } else {
            Failure::Counted
        }
    }

    /// A successful login of `username` clears its count and its lock.
    pub fn succeeded(&self, username: &str) {
        self.counts().remove(username);
    }

    /// How many names hold a count, for a test to see that old ones are forgotten.
    #[must_use]
    pub fn tracked(&self) -> usize {
        self.counts().len()
    }
}
