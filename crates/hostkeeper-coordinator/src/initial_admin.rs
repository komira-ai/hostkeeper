//! The initial-admin file: how the first operator user is made, and how a lost password is
//! recovered (`docs/design/operator-login.md`, sections 4 and 6.4).
//!
//! The deploy step writes a file, `--initial-admin-file`, TOML with four keys: an `id` (random,
//! one for each file), the `user`, a `password` in plain text, and `valid_until` in Unix seconds.
//! The file is read under the rule of a secret file (a regular file owned by the coordinator's
//! user, mode `0600` or `0400`) at start and on every `--tls-reload-secs` tick
//! ([`watch_every`]); each look is [`InitialAdmin::look`]:
//!
//! - no file: nothing;
//! - `valid_until` is not in the future: the file is deleted, the user's initial credential is
//!   voided when this file's `id` is the one the record adopted and the password was not yet
//!   changed, and `initial_password_expired` is audited;
//! - the record already holds this `id`: nothing, so a restart never resets a password twice;
//! - otherwise the password is hashed with Argon2id and the user is made, or, for a user that
//!   exists, given the new hash, enabled, and made to change it (the break-glass of section 6.4);
//!   `user_bootstrap` is audited. A file that asks for more than `--initial-password-secs` past
//!   the coordinator's clock is refused: the file cannot grant more than the flag.
//!
//! The clock is the wall clock of the coordinator, injected ([`UnixClock`]); the file's own
//! `valid_until` is compared with it and with nothing else. No error, no log line and no
//! [`Debug`](std::fmt::Debug) text holds the password or its hash.
//!
//! This module makes the record and nothing of the sign-in: the session table, the login routes
//! and the password change are [`crate::login`] and [`crate::login_routes`]; a password change
//! deletes the file its user's password came from ([`InitialAdmin::delete_if_adopted`]).

use std::convert::Infallible;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use clap::Args;
use hostkeeper_wire::secret::{SecretError, read_secret};
use serde::Deserialize;

use crate::accounts::{AccountStore, AccountsError, User};
use crate::audit_actions::{INITIAL_PASSWORD_EXPIRED, USER_BOOTSTRAP, about};
use crate::audit_file::UnixClock;
use crate::login::LoginSettings;
use crate::ops::{OpsError, OpsStore, Write};
use crate::password::{PasswordError, PasswordSettings, Passwords, PolicyError};
use crate::reconcile::COORDINATOR;
use crate::service::Coordinator;
use crate::session::{LimiterSettings, SessionSettings};
use crate::tokens::{TokenName, TokenNameError};

/// The flag that names the file, which the texts of its errors name instead of its path.
pub const FLAG: &str = "--initial-admin-file";

/// The longest an initial password is good, whatever the flag says: the owner's rule is one
/// hour.
pub const MAX_VALID_SECS: u64 = 3600;

/// The most bytes of the file.
const MAX_FILE_BYTES: u64 = 4096;

/// The most bytes of an `id`.
const MAX_ID_BYTES: usize = 128;

/// The flags of the operator login that this build reads.
#[derive(Clone, Debug, Args)]
pub struct LoginFlags {
    /// The initial-admin file: TOML with `id`, `user`, `password` and `valid_until` (Unix
    /// seconds), mode 0600 or 0400, owned by the coordinator's user. The coordinator makes the
    /// user from it, with that password, which must be changed at the first login, and deletes
    /// the file when the password has expired. It is read at start and every
    /// `--tls-reload-secs`. Taken instead of, or beside, `--tokens-file` or
    /// `--operator-token-file`; the start rule is that one of the three is given.
    #[arg(long, value_name = "FILE")]
    pub initial_admin_file: Option<PathBuf>,
    /// How long an initial password is good, in seconds, 1 to 3600: a file whose `valid_until`
    /// is further than this past the coordinator's clock is refused.
    #[arg(
        long,
        default_value_t = MAX_VALID_SECS,
        value_parser = clap::value_parser!(u64).range(1..=3600)
    )]
    pub initial_password_secs: u64,
    /// The Argon2id memory of a password hash, in KiB (the least of OWASP's guidance by
    /// default). A stored hash made under less is replaced at a right password.
    #[arg(long, default_value_t = PasswordSettings::default().memory_kib)]
    pub password_memory_kib: u32,
    /// The Argon2id passes of a password hash.
    #[arg(long, default_value_t = PasswordSettings::default().iterations)]
    pub password_iterations: u32,
    /// The Argon2id lanes of a password hash.
    #[arg(long, default_value_t = PasswordSettings::default().parallelism)]
    pub password_parallelism: u32,
    /// The fewest characters of a new password, 1 to 256.
    #[arg(
        long,
        default_value_t = PasswordSettings::default().min_length,
        value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(1..=256)
    )]
    pub password_min_length: usize,
    /// Seconds of no use after which a browser session ends; each use pushes it out.
    #[arg(
        long,
        default_value_t = SessionSettings::default().idle_secs,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    pub session_idle_secs: u64,
    /// Seconds after sign-in at which a browser session ends however it is used.
    #[arg(
        long,
        default_value_t = SessionSettings::default().max_secs,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    pub session_max_secs: u64,
    /// The most browser sessions held; a sign-in past it ends the session least recently used.
    #[arg(
        long,
        default_value_t = SessionSettings::default().max_count,
        value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(1..)
    )]
    pub session_max_count: usize,
    /// Consecutive failed logins of one user name that lock it.
    #[arg(
        long,
        default_value_t = LimiterSettings::default().max_failures,
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    pub login_max_failures: u32,
    /// Seconds a locked user name stays locked, and after which a run of failures is forgotten.
    #[arg(
        long,
        default_value_t = LimiterSettings::default().lockout_secs,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    pub login_lockout_secs: u64,
    /// The most password hashes that run at once (each takes about the memory of
    /// `--password-memory-kib`); a login past it is told to try later and starts no hash.
    #[arg(
        long,
        default_value_t = LoginSettings::default().max_hashes,
        value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(1..)
    )]
    pub login_max_hashes: usize,
    /// The most user names the login limiter counts failures of at once.
    #[arg(
        long,
        default_value_t = LimiterSettings::default().max_tracked,
        value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(1..)
    )]
    pub login_max_tracked: usize,
}

impl LoginFlags {
    /// The settings of the sign-in the flags give.
    #[must_use]
    pub fn login_settings(&self) -> LoginSettings {
        LoginSettings {
            sessions: SessionSettings {
                idle_secs: self.session_idle_secs,
                max_secs: self.session_max_secs,
                max_count: self.session_max_count,
            },
            limiter: LimiterSettings {
                max_failures: self.login_max_failures,
                lockout_secs: self.login_lockout_secs,
                max_tracked: self.login_max_tracked,
            },
            max_hashes: self.login_max_hashes,
            passwords: self.password_settings(),
            temporary_secs: self.initial_password_secs,
        }
    }

    /// The password settings the flags give.
    #[must_use]
    pub fn password_settings(&self) -> PasswordSettings {
        PasswordSettings {
            memory_kib: self.password_memory_kib,
            iterations: self.password_iterations,
            parallelism: self.password_parallelism,
            min_length: self.password_min_length,
        }
    }
}

/// Why the file is refused or a look could not finish. The texts name the flag, never the path,
/// the password or its hash.
#[derive(Debug, thiserror::Error)]
pub enum InitialAdminError {
    /// The file cannot be read, or breaks the rule of a secret file.
    #[error(transparent)]
    Read(#[from] SecretError),
    /// The file is over [`MAX_FILE_BYTES`].
    #[error("--initial-admin-file: it is over {MAX_FILE_BYTES} bytes")]
    TooLong,
    /// The file is not UTF-8.
    #[error("--initial-admin-file: it is not UTF-8")]
    NotText,
    /// The text is not TOML holding the four keys and no other.
    #[error("--initial-admin-file: it is not TOML holding id, user, password and valid_until")]
    Shape,
    /// The `id` is empty, over [`MAX_ID_BYTES`] bytes, or not visible ASCII.
    #[error("--initial-admin-file: id is 1 to {MAX_ID_BYTES} bytes of visible ASCII")]
    Id,
    /// The `user` is not a name.
    #[error("--initial-admin-file: user: {0}")]
    User(#[source] TokenNameError),
    /// The `password` breaks the policy.
    #[error("--initial-admin-file: password: {0}")]
    Policy(#[source] PolicyError),
    /// `valid_until` is further than `--initial-password-secs` past the clock.
    #[error(
        "--initial-admin-file: valid_until is more than --initial-password-secs ({secs}) from now"
    )]
    TooFar {
        /// The setting.
        secs: u64,
    },
    /// The password could not be hashed, or the settings were refused.
    #[error("--initial-admin-file: {0}")]
    Hash(#[from] PasswordError),
    /// The user could not be stored.
    #[error("--initial-admin-file: the user was not stored: {0}")]
    Store(#[from] AccountsError),
    /// The expired file could not be deleted.
    #[error("--initial-admin-file: the expired file was not deleted: {0}")]
    Delete(io::ErrorKind),
    /// The look did not finish: it panicked (`true`) or was cancelled (`false`).
    #[error("--initial-admin-file: the look did not finish ({})", if *.0 { "it panicked" } else { "it was cancelled" })]
    Panicked(bool),
    /// The audit entry could not be written.
    #[error("--initial-admin-file: the audit entry was not written: {0}")]
    Audit(#[from] OpsError),
}

/// What one look did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// There is no file.
    Absent,
    /// The file's user was made, or given the file's password.
    Adopted,
    /// The record already holds this file's `id`: nothing was done.
    AlreadyAdopted,
    /// The file had expired: it was deleted.
    Expired,
}

/// The file as written. Not `Debug`: it holds the password.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    id: String,
    user: String,
    password: String,
    valid_until: u64,
}

/// The initial-admin file, the clock it is judged by and the passwords it is hashed with.
pub struct InitialAdmin {
    path: PathBuf,
    max_secs: u64,
    passwords: Passwords,
    clock: UnixClock,
}

impl std::fmt::Debug for InitialAdmin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InitialAdmin")
            .field("max_secs", &self.max_secs)
            .finish_non_exhaustive()
    }
}

/// The file's content, read and checked as far as it can be without the accounts.
fn read(path: &Path) -> Result<Option<RawFile>, InitialAdminError> {
    let bytes = match read_secret(path, FLAG, MAX_FILE_BYTES) {
        Err(SecretError::Io {
            kind: io::ErrorKind::NotFound,
            ..
        }) => return Ok(None),
        other => other?,
    };
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(InitialAdminError::TooLong);
    }
    let text = String::from_utf8(bytes).map_err(|_| InitialAdminError::NotText)?;
    let file: RawFile = toml::from_str(&text).map_err(|_| InitialAdminError::Shape)?;
    if file.id.is_empty()
        || file.id.len() > MAX_ID_BYTES
        || !file.id.bytes().all(|b| b.is_ascii_graphic())
    {
        return Err(InitialAdminError::Id);
    }
    file.user
        .parse::<TokenName>()
        .map_err(InitialAdminError::User)?;
    Ok(Some(file))
}

impl InitialAdmin {
    /// The file at `path`, judged by `clock`, good for at most `max_secs` (capped at
    /// [`MAX_VALID_SECS`]); passwords are hashed under `settings`.
    ///
    /// # Errors
    /// [`InitialAdminError::Hash`]: the settings are outside what Argon2id takes.
    pub fn new(
        path: PathBuf,
        max_secs: u64,
        settings: PasswordSettings,
        clock: UnixClock,
    ) -> Result<Self, InitialAdminError> {
        Ok(Self {
            path,
            max_secs: max_secs.min(MAX_VALID_SECS),
            passwords: Passwords::new(settings)?,
            clock,
        })
    }

    /// One look at the file, and what it asks for done to `accounts`, audited in `ops`.
    ///
    /// # Errors
    /// [`InitialAdminError`]: the file is refused (nothing is changed), the user could not be
    /// stored, the expired file could not be deleted, or the entry could not be audited.
    pub fn look(
        &self,
        accounts: &AccountStore,
        ops: &OpsStore,
    ) -> Result<Outcome, InitialAdminError> {
        let Some(file) = read(&self.path)? else {
            return Ok(Outcome::Absent);
        };
        let now = (self.clock)();
        if now >= file.valid_until {
            return self.expire(&file, accounts, ops);
        }
        let held = accounts.snapshot();
        let adopted = held
            .user(&file.user)
            .is_some_and(|user| user.initial_id.as_deref() == Some(file.id.as_str()));
        if adopted {
            return Ok(Outcome::AlreadyAdopted);
        }
        if file.valid_until - now > self.max_secs {
            return Err(InitialAdminError::TooFar {
                secs: self.max_secs,
            });
        }
        self.passwords
            .check_policy(&file.password, &file.user, None)
            .map_err(InitialAdminError::Policy)?;
        let hash = self.passwords.hash(&file.password)?.as_str().to_owned();
        // Audited first, as the expiry is: an audit that cannot be written leaves the user
        // unmade and the next look does both again, where an audit after the store would be
        // lost for good (the next look finds the id adopted). A store that cannot write after
        // the entry repeats the entry at the next look, until it can.
        audit(ops, USER_BOOTSTRAP, &file.user)?;
        accounts.change(|held| adopt(held, &file, hash, now))?;
        Ok(Outcome::Adopted)
    }

    /// Deletes the file if it is the one `id` names for `user`: the file a password the user has
    /// now changed came from. A file of another user or another `id` stays, and so does one that
    /// is not there. Called after the change, which has already voided what the file would grant.
    ///
    /// # Errors
    /// [`InitialAdminError`]: the file cannot be read, or the file that is the user's cannot be
    /// deleted. The expiry of the file deletes it at the next look in either case.
    pub fn delete_if_adopted(&self, user: &str, id: &str) -> Result<(), InitialAdminError> {
        let Some(file) = read(&self.path)? else {
            return Ok(());
        };
        if file.user != user || file.id != id {
            return Ok(());
        }
        self.delete()
    }

    /// Deletes the file; one that is already gone (another look, or a password change, deleted
    /// it) is deleted.
    fn delete(&self) -> Result<(), InitialAdminError> {
        match std::fs::remove_file(&self.path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                Err(InitialAdminError::Delete(e.kind()))
            }
            _ => Ok(()),
        }
    }

    /// The file has expired: its user's initial credential is voided if this file's `id` made it
    /// and the password was not changed, the expiry is audited unless the password had been
    /// changed already, and then the file is deleted. In that order, so that a store or an audit
    /// that cannot write leaves the file, and the next look does it again; a file that cannot be
    /// deleted is audited again at each look until it can.
    fn expire(
        &self,
        file: &RawFile,
        accounts: &AccountStore,
        ops: &OpsStore,
    ) -> Result<Outcome, InitialAdminError> {
        let held = accounts.snapshot();
        let (void, changed) = held.user(&file.user).map_or((false, false), |user| {
            let mine = user.initial_id.as_deref() == Some(file.id.as_str());
            (
                mine && user.initial_expires_at.is_some() && !user.initial_void,
                mine && user.initial_expires_at.is_none(),
            )
        });
        if void {
            accounts.change(|held| {
                held.with_user_edited(&file.user, |user| user.initial_void = true)
            })?;
        }
        if !changed {
            audit(ops, INITIAL_PASSWORD_EXPIRED, &file.user)?;
        }
        self.delete()?;
        Ok(Outcome::Expired)
    }
}

/// The accounts with the user of `file` made, or, for one that exists, given `hash` as an
/// initial password to change: enabled, `must_change`, good until `file.valid_until`, and
/// recording the file's `id`.
fn adopt(
    held: &crate::accounts::Accounts,
    file: &RawFile,
    hash: String,
    now: u64,
) -> Result<crate::accounts::Accounts, AccountsError> {
    let user = User {
        name: file.user.clone(),
        hash,
        enabled: true,
        must_change: true,
        initial_expires_at: Some(file.valid_until),
        initial_id: Some(file.id.clone()),
        initial_void: false,
        created_by: COORDINATOR.to_owned(),
        created_at: now,
        last_login_at: None,
    };
    match held.user(&file.user) {
        None => held.with_user(user),
        Some(_) => held.with_user_edited(&file.user, |old| {
            old.hash = user.hash;
            old.enabled = true;
            old.must_change = true;
            old.initial_expires_at = user.initial_expires_at;
            old.initial_id = user.initial_id;
            old.initial_void = false;
        }),
    }
}

/// The entry of `action` by the coordinator about `user`.
fn audit(ops: &OpsStore, action: &str, user: &str) -> Result<(), OpsError> {
    ops.change(
        Write::BeforeMemory,
        about(COORDINATOR, action, user),
        |_| {},
    )
    .map(drop)
}

impl Coordinator {
    /// One look at the initial-admin file, if the coordinator has one ([`InitialAdmin`]).
    ///
    /// # Errors
    /// [`InitialAdminError`], as [`InitialAdmin::look`].
    pub fn look_at_initial_admin(&self) -> Result<Option<Outcome>, InitialAdminError> {
        self.initial_admin
            .as_ref()
            .map(|file| file.look(self.accounts(), &self.ops))
            .transpose()
    }
}

/// Looks at the initial-admin file of `coordinator` every `interval`, for as long as the future
/// is polled. A look that fails is logged (the text names the flag, never the password) and the
/// next one tries again. It never returns.
pub async fn watch_every(coordinator: Arc<Coordinator>, interval: Duration) -> Infallible {
    let (unheard, _) = tokio::sync::mpsc::unbounded_channel();
    watch(coordinator, interval, unheard).await
}

/// [`watch_every`], telling `done` after each look: a test's progress event, so it waits for a
/// look instead of for a time.
async fn watch(
    coordinator: Arc<Coordinator>,
    interval: Duration,
    done: tokio::sync::mpsc::UnboundedSender<()>,
) -> Infallible {
    let mut ticks = tokio::time::interval(interval);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The first look is the start's.
    ticks.tick().await;
    loop {
        ticks.tick().await;
        let looking = Arc::clone(&coordinator);
        // File reads, a hash and file writes: off the runtime's threads.
        let looked = tokio::task::spawn_blocking(move || looking.look_at_initial_admin()).await;
        report(joined(looked));
        let _ = done.send(());
    }
}

/// The result of a look that ran off the runtime's threads: one that panicked or was cancelled is
/// a refusal, so that it is logged.
fn joined(
    looked: Result<Result<Option<Outcome>, InitialAdminError>, tokio::task::JoinError>,
) -> Result<Option<Outcome>, InitialAdminError> {
    looked.unwrap_or_else(|e| Err(InitialAdminError::Panicked(e.is_panic())))
}

/// Logs what a look did: a change at info, a refusal at warn.
fn report(looked: Result<Option<Outcome>, InitialAdminError>) {
    match looked {
        Ok(Some(outcome @ (Outcome::Adopted | Outcome::Expired))) => {
            tracing::info!(?outcome, "the initial-admin file was looked at");
        }
        Err(why) => tracing::warn!(%why, "the initial-admin file was not applied"),
        Ok(_) => {}
    }
}

#[cfg(test)]
#[path = "initial_admin_tests.rs"]
mod tests;
