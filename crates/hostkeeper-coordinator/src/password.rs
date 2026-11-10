//! The operator's passwords: Argon2id hashes, the policy, and the cap on hashes in flight
//! (docs/design/operator-login.md, section 6.1).
//!
//! A hash is an Argon2id PHC string (`$argon2id$v=19$m=...,t=...,p=...$salt$hash`) with a 16
//! byte random salt from the operating system. The cost parameters are in each string, so
//! [`Passwords::verify`] checks a hash made under any setting, and says to store a new one
//! ([`Verdict::Right`] with a `rehash`) when the stored one is weaker than the current
//! [`PasswordSettings`]. SHA-256 is not used for a password anywhere.
//!
//! A password is NFKC normalised before it is hashed, compared or measured, so the same text
//! typed two ways is one password. The policy is a length and nothing else
//! ([`Passwords::check_policy`]): at least `min_length` characters, at most
//! [`MAX_PASSWORD_BYTES`] bytes, not the user's name, not the current password.
//!
//! A login for a user that cannot log in (unknown, disabled, expired initial password) still
//! spends one verify against a fixed hash of the same cost ([`Passwords::verify_absent`]), so
//! the time does not say which case it was; [`Passwords::verifies`] counts the verifies so a
//! test can say so without timing anything. [`HashSlots`] is the cap on hashes at once: each
//! Argon2id run holds its memory, so a flood of logins must not take the coordinator's.
//!
//! The module is pure computation: it reads no file, no clock and no flag. The settings are
//! the values the flags `--password-memory-kib`, `--password-iterations`,
//! `--password-parallelism` and `--password-min-length` will give it.

use std::fmt;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use argon2::password_hash::phc::PasswordHash as Phc;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use argon2::{Algorithm, Argon2, Params, Version};
use unicode_normalization::UnicodeNormalization;

/// The most bytes of a password, as typed.
pub const MAX_PASSWORD_BYTES: usize = 256;

/// The Argon2id parameters and the length rule: the defaults are the least OWASP's guidance
/// gives for Argon2id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PasswordSettings {
    /// Memory in KiB (`--password-memory-kib`).
    pub memory_kib: u32,
    /// Passes over the memory (`--password-iterations`).
    pub iterations: u32,
    /// Lanes (`--password-parallelism`).
    pub parallelism: u32,
    /// The fewest characters of a new password (`--password-min-length`).
    pub min_length: usize,
}

impl Default for PasswordSettings {
    fn default() -> Self {
        Self {
            memory_kib: 19456,
            iterations: 2,
            parallelism: 1,
            min_length: 12,
        }
    }
}

/// Why the passwords cannot be set up or a hash cannot be made.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum PasswordError {
    /// The Argon2id parameters are outside what the algorithm allows.
    #[error("the password hash parameters are refused: {0}")]
    Params(argon2::Error),
    /// The hash could not be made (the system's random bytes failed).
    #[error("a password could not be hashed: {0}")]
    Hash(argon2::password_hash::Error),
}

/// Why a new password is refused. The texts name the rule, never the password.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    /// Fewer than the minimum characters.
    #[error("the password must be at least {min} characters")]
    TooShort {
        /// The setting `--password-min-length`.
        min: usize,
    },
    /// More than [`MAX_PASSWORD_BYTES`] bytes.
    #[error("the password must be at most {MAX_PASSWORD_BYTES} bytes")]
    TooLong,
    /// The same as the user's name.
    #[error("the password must not be the user name")]
    SameAsUserName,
    /// The same as the password in use.
    #[error("the new password must differ from the current one")]
    SameAsCurrent,
}

/// A stored password hash: the PHC string, which holds no password.
#[derive(Clone, PartialEq, Eq)]
pub struct StoredHash(String);

impl StoredHash {
    /// The PHC string as stored.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for StoredHash {
    /// A hash read back from the store. It is parsed when it is verified, not here.
    fn from(phc: String) -> Self {
        Self(phc)
    }
}

impl fmt::Debug for StoredHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StoredHash(..)")
    }
}

/// The answer of [`Passwords::verify`].
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The password is wrong, or the stored hash is not one this can read.
    Wrong,
    /// The password is right. `rehash` is a new hash to store when the stored one is weaker
    /// than the current settings (or not Argon2id version 0x13).
    Right {
        /// The hash to store in place of the old one.
        rehash: Option<StoredHash>,
    },
}

/// Hashing, verifying and the policy, under one [`PasswordSettings`].
pub struct Passwords {
    settings: PasswordSettings,
    argon2: Argon2<'static>,
    pub(crate) dummy: StoredHash,
    verifies: AtomicU64,
}

impl fmt::Debug for Passwords {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Passwords")
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

fn normalise(password: &str) -> String {
    password.nfkc().collect()
}

impl Passwords {
    /// Passwords under `settings`. Makes the fixed hash [`Passwords::verify_absent`] verifies
    /// against, so it costs one hash.
    ///
    /// # Errors
    /// [`PasswordError::Params`] when the parameters are outside the algorithm's range,
    /// [`PasswordError::Hash`] when the system's random bytes fail.
    pub fn new(settings: PasswordSettings) -> Result<Self, PasswordError> {
        let params = Params::new(
            settings.memory_kib,
            settings.iterations,
            settings.parallelism,
            None,
        )
        .map_err(PasswordError::Params)?;
        let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        // One byte past the longest password a login takes, so no login can match it.
        let unmatchable = "x".repeat(MAX_PASSWORD_BYTES + 1);
        let dummy = hash_with(&argon2, &unmatchable)?;
        Ok(Self {
            settings,
            argon2,
            dummy,
            verifies: AtomicU64::new(0),
        })
    }

    /// The settings these passwords were made under.
    #[must_use]
    pub fn settings(&self) -> &PasswordSettings {
        &self.settings
    }

    /// The hash of `password` under the current settings, with a fresh salt. The policy is
    /// [`Passwords::check_policy`]'s, not checked here.
    ///
    /// # Errors
    /// [`PasswordError::Hash`] when the system's random bytes fail.
    pub fn hash(&self, password: &str) -> Result<StoredHash, PasswordError> {
        hash_with(&self.argon2, &normalise(password))
    }

    /// Whether `new` may be the password of `username`, whose password in use is `current`
    /// (None when the user has none to compare: a first choice).
    ///
    /// # Errors
    /// The first rule `new` breaks, in the order of [`PolicyError`]'s variants.
    pub fn check_policy(
        &self,
        new: &str,
        username: &str,
        current: Option<&str>,
    ) -> Result<(), PolicyError> {
        if new.len() > MAX_PASSWORD_BYTES {
            return Err(PolicyError::TooLong);
        }
        let new = normalise(new);
        if new.chars().count() < self.settings.min_length {
            return Err(PolicyError::TooShort {
                min: self.settings.min_length,
            });
        }
        if new == normalise(username) {
            return Err(PolicyError::SameAsUserName);
        }
        if current.is_some_and(|current| new == normalise(current)) {
            return Err(PolicyError::SameAsCurrent);
        }
        Ok(())
    }

    /// Check `password` against `stored`. The comparison is the algorithm's, in constant
    /// time. A stored string that is not a readable PHC hash is [`Verdict::Wrong`].
    #[must_use]
    pub fn verify(&self, password: &str, stored: &StoredHash) -> Verdict {
        self.verifies.fetch_add(1, Ordering::Relaxed);
        let password = normalise(password);
        let Ok(parsed) = Phc::new(stored.as_str()) else {
            return Verdict::Wrong;
        };
        if self
            .argon2
            .verify_password(password.as_bytes(), &parsed)
            .is_err()
        {
            return Verdict::Wrong;
        }
        let rehash = if self.is_weaker(&parsed) {
            hash_with(&self.argon2, &password).ok()
        } else {
            None
        };
        Verdict::Right { rehash }
    }

    /// One verify against the fixed hash, for a login that cannot succeed, so it costs what a
    /// real one does. The answer is never "right": the fixed password is longer than a login
    /// can send.
    pub fn verify_absent(&self, password: &str) {
        let _ = self.verify(password, &self.dummy);
    }

    /// How many verifies have run, real and absent. Tests read it to say a case spent one.
    #[must_use]
    pub fn verifies(&self) -> u64 {
        self.verifies.load(Ordering::Relaxed)
    }

    fn is_weaker(&self, parsed: &Phc) -> bool {
        // The verify this follows read the parameters, so they are there; a hash whose are
        // not is counted weak all the same.
        parsed.algorithm.as_str() != Algorithm::Argon2id.as_str()
            || parsed.version != Some(Version::V0x13.into())
            || Params::try_from(parsed).ok().is_none_or(|stored| {
                stored.m_cost() < self.settings.memory_kib
                    || stored.t_cost() < self.settings.iterations
                    || stored.p_cost() < self.settings.parallelism
            })
    }
}

fn hash_with(argon2: &Argon2<'_>, normalised: &str) -> Result<StoredHash, PasswordError> {
    argon2
        .hash_password(normalised.as_bytes())
        .map(|phc| StoredHash(phc.to_string()))
        .map_err(PasswordError::Hash)
}

/// The cap on password hashes running at once (`--login-max-hashes`). A holder of a
/// [`HashSlot`] may run one hash; past the cap [`HashSlots::try_acquire`] says no and no hash
/// starts.
#[derive(Clone, Debug)]
pub struct HashSlots {
    max: usize,
    in_flight: Arc<Mutex<usize>>,
}

/// One hash's place under the cap, given back when it is dropped.
#[derive(Debug)]
pub struct HashSlot {
    in_flight: Arc<Mutex<usize>>,
}

impl HashSlots {
    /// A cap of `max` hashes at once.
    #[must_use]
    pub fn new(max: NonZeroUsize) -> Self {
        Self {
            max: max.get(),
            in_flight: Arc::new(Mutex::new(0)),
        }
    }

    /// A slot, or None when `max` hashes are already running.
    #[must_use]
    pub fn try_acquire(&self) -> Option<HashSlot> {
        let mut running = self
            .in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if *running >= self.max {
            return None;
        }
        *running += 1;
        Some(HashSlot {
            in_flight: Arc::clone(&self.in_flight),
        })
    }
}

impl Drop for HashSlot {
    fn drop(&mut self) {
        let mut running = self
            .in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *running = running.saturating_sub(1);
    }
}
