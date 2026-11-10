//! The operator users and the API tokens made in the UI, which the store keeps
//! (`docs/design/operator-login.md`, sections 6, 7 and 9): each [`User`] with the hash of the
//! password, and each [`ApiToken`] with the SHA-256 of the token, never the token or a password.
//!
//! The accounts are a part of the store of their own, beside the desired state, the operations
//! state and the verdicts. They are written whole, when a commit changes them: `<state-file>.accounts`
//! on the file store ([`encode_file`], [`parse_file`]), the tables `users` and `api_tokens`
//! (migration `0009`, `may-run`) on Postgres. An older coordinator never reads either, and a
//! coordinator that finds neither starts with no user and no stored token, which is the state it
//! starts in.
//!
//! The file is JSON and is read as the file of the verdicts is: a key it does not know is skipped,
//! so a later build may add one, and a value that breaks a rule of [`Accounts::new`] stops the
//! start, naming the file. A file that is not there holds none.
//!
//! The names of users and of API tokens are one namespace, as the audit's `by` is the name of
//! either ([`TokenName`]). The tokens file's names are not in it: whoever creates a user or a
//! token checks them against the tokens file, which this module does not read.
//!
//! - [`Accounts::with_user`] and [`Accounts::with_token`] make the accounts a creation would
//!   leave, and refuse a name that clashes or a hash that is not what it says.
//! - [`Accounts::with_user_edited`] changes one user's record; [`Accounts::with_enabled`] and
//!   [`Accounts::without_user`] refuse to leave a farm with no enabled user.
//! - [`AccountStore`] is the accounts and the backend a change of them is committed to.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};

use crate::desired::StoreError;
use crate::store::{BackendError, Commit, Fake, Files, Landing, Shared};
use crate::tokens::{TokenName, TokenNameError};

/// The most bytes of a password hash a user record holds: a PHC string is a hundred or so.
const MAX_HASH_BYTES: usize = 512;

/// The file beside the state file `state`: `<state>.accounts`.
#[must_use]
pub fn accounts_path(state: &Path) -> PathBuf {
    let mut path = state.as_os_str().to_owned();
    path.push(".accounts");
    PathBuf::from(path)
}

/// Why the accounts were refused, or not written.
#[derive(Debug, thiserror::Error)]
pub enum AccountsError {
    /// The file could not be read or written.
    #[error(transparent)]
    Io(#[from] StoreError),
    /// The file or a stored record holds a value this build cannot read.
    #[error(
        "{path}: {why}; the coordinator does not guess which users and tokens exist, so fix or remove the file to start"
    )]
    Invalid {
        /// The file, or the table.
        path: PathBuf,
        /// Why.
        why: String,
    },
    /// A name is refused as a name of a user or a token.
    #[error("{name}: {source}")]
    Name {
        /// The name.
        name: String,
        /// Why.
        source: TokenNameError,
    },
    /// The name is already a user's or a token's.
    #[error("the name {0} is already a user's or a token's")]
    Taken(String),
    /// No user has the name.
    #[error("there is no user named {0}")]
    NoUser(String),
    /// No API token has the name.
    #[error("there is no API token named {0}")]
    NoToken(String),
    /// The change would leave no enabled user.
    #[error("the last enabled user cannot be disabled or deleted")]
    LastEnabled,
    /// A password hash is not a PHC string of a sensible length.
    #[error("the password hash of {0} is not a PHC string of at most {MAX_HASH_BYTES} bytes")]
    Hash(String),
    /// The `sha256` of a token is not 64 lowercase hex digits.
    #[error("the sha256 of the token {0} is not 64 lowercase hex digits")]
    Digest(String),
    /// Two tokens have the same `sha256`: one secret would answer to two names.
    #[error("the tokens {first} and {second} have the same sha256")]
    SharedDigest {
        /// The token whose name sorts first.
        first: String,
        /// The other.
        second: String,
    },
}

/// A user as the store keeps it. `name` is the key of the record and is not in its JSON.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    /// The name, which the audit's `by` is.
    #[serde(skip)]
    pub name: String,
    /// The password's hash, a PHC string.
    pub hash: String,
    /// Whether the user may sign in.
    pub enabled: bool,
    /// Whether the user must choose a new password before anything else.
    #[serde(default)]
    pub must_change: bool,
    /// The second until which the password may log in, for an initial or a reset password.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_expires_at: Option<u64>,
    /// The id of the initial-admin file this record adopted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_id: Option<String>,
    /// Whether the initial credential was voided: the record stays, the password does not work.
    #[serde(default)]
    pub initial_void: bool,
    /// Who made the user: a user's or a token's name, or `coordinator`.
    pub created_by: String,
    /// When, in Unix seconds.
    pub created_at: u64,
    /// When the user last signed in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_login_at: Option<u64>,
}

/// An API token as the store keeps it. `name` is the key of the record and is not in its JSON.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiToken {
    /// The name, which the audit's `by` is.
    #[serde(skip)]
    pub name: String,
    /// The SHA-256 of the token as 64 lowercase hex digits.
    pub sha256: String,
    /// Who made the token.
    pub created_by: String,
    /// When, in Unix seconds.
    pub created_at: u64,
}

/// Everything the store holds of the operator's users and API tokens, by name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accounts {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    users: BTreeMap<String, User>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    tokens: BTreeMap<String, ApiToken>,
}

/// The name `name` as a name of a user or a token.
fn named(name: &str) -> Result<(), AccountsError> {
    name.parse::<TokenName>()
        .map(drop)
        .map_err(|source| AccountsError::Name {
            name: name.to_owned(),
            source,
        })
}

impl Accounts {
    /// The accounts of `users` and `tokens`, each judged: a name that is not a name, a name that is
    /// a user's and a token's, a hash that is not a PHC string, a digest that is not 64 lowercase
    /// hex digits, or one digest on two tokens is refused.
    ///
    /// # Errors
    /// [`AccountsError`]: the first rule broken.
    pub fn new(
        users: BTreeMap<String, User>,
        tokens: BTreeMap<String, ApiToken>,
    ) -> Result<Self, AccountsError> {
        for (name, user) in &users {
            named(name)?;
            if tokens.contains_key(name) {
                return Err(AccountsError::Taken(name.clone()));
            }
            if !user.hash.starts_with('$') || user.hash.len() > MAX_HASH_BYTES {
                return Err(AccountsError::Hash(name.clone()));
            }
        }
        let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
        for (name, token) in &tokens {
            named(name)?;
            let hex = token.sha256.len() == 64
                && token
                    .sha256
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
            if !hex {
                return Err(AccountsError::Digest(name.clone()));
            }
            if let Some(first) = seen.insert(&token.sha256, name) {
                return Err(AccountsError::SharedDigest {
                    first: first.to_owned(),
                    second: name.clone(),
                });
            }
        }
        let mut users = users;
        let mut tokens = tokens;
        for (name, user) in &mut users {
            name.clone_into(&mut user.name);
        }
        for (name, token) in &mut tokens {
            name.clone_into(&mut token.name);
        }
        Ok(Self { users, tokens })
    }

    /// Whether the accounts hold no user and no token.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.users.is_empty() && self.tokens.is_empty()
    }

    /// The user named `name`.
    #[must_use]
    pub fn user(&self, name: &str) -> Option<&User> {
        self.users.get(name)
    }

    /// Every user, by name.
    pub fn users(&self) -> impl Iterator<Item = &User> {
        self.users.values()
    }

    /// The API token named `name`.
    #[must_use]
    pub fn token(&self, name: &str) -> Option<&ApiToken> {
        self.tokens.get(name)
    }

    /// Every API token, by name.
    pub fn tokens(&self) -> impl Iterator<Item = &ApiToken> {
        self.tokens.values()
    }

    /// How many users are enabled.
    #[must_use]
    pub fn enabled_users(&self) -> usize {
        self.users.values().filter(|user| user.enabled).count()
    }

    /// The accounts with `user` added.
    ///
    /// # Errors
    /// [`AccountsError`]: the name is refused or taken, or the hash is not a PHC string.
    pub fn with_user(&self, user: User) -> Result<Self, AccountsError> {
        named(&user.name)?;
        if self.users.contains_key(&user.name) || self.tokens.contains_key(&user.name) {
            return Err(AccountsError::Taken(user.name));
        }
        let mut users = self.users.clone();
        users.insert(user.name.clone(), user);
        Self::new(users, self.tokens.clone())
    }

    /// The accounts with the user `name` changed by `edit`: the name is not changed.
    ///
    /// # Errors
    /// [`AccountsError::NoUser`]: there is none. [`AccountsError::Hash`]: `edit` left a hash that
    /// is not a PHC string.
    pub fn with_user_edited(
        &self,
        name: &str,
        edit: impl FnOnce(&mut User),
    ) -> Result<Self, AccountsError> {
        let mut users = self.users.clone();
        let user = users
            .get_mut(name)
            .ok_or_else(|| AccountsError::NoUser(name.to_owned()))?;
        edit(user);
        name.clone_into(&mut user.name);
        Self::new(users, self.tokens.clone())
    }

    /// The accounts with the user `name` enabled or disabled.
    ///
    /// # Errors
    /// [`AccountsError::NoUser`]: there is none. [`AccountsError::LastEnabled`]: it is the only
    /// enabled user and is to be disabled.
    pub fn with_enabled(&self, name: &str, enabled: bool) -> Result<Self, AccountsError> {
        let user = self
            .users
            .get(name)
            .ok_or_else(|| AccountsError::NoUser(name.to_owned()))?;
        if !enabled && user.enabled && self.enabled_users() == 1 {
            return Err(AccountsError::LastEnabled);
        }
        self.with_user_edited(name, |user| user.enabled = enabled)
    }

    /// The accounts without the user `name`.
    ///
    /// # Errors
    /// [`AccountsError::NoUser`]: there is none. [`AccountsError::LastEnabled`]: it is the only
    /// enabled user.
    pub fn without_user(&self, name: &str) -> Result<Self, AccountsError> {
        let user = self
            .users
            .get(name)
            .ok_or_else(|| AccountsError::NoUser(name.to_owned()))?;
        if user.enabled && self.enabled_users() == 1 {
            return Err(AccountsError::LastEnabled);
        }
        let mut users = self.users.clone();
        users.remove(name);
        Self::new(users, self.tokens.clone())
    }

    /// The accounts with `token` added.
    ///
    /// # Errors
    /// [`AccountsError`]: the name is refused or taken, or the digest is not 64 lowercase hex
    /// digits or is another token's.
    pub fn with_token(&self, token: ApiToken) -> Result<Self, AccountsError> {
        named(&token.name)?;
        if self.users.contains_key(&token.name) || self.tokens.contains_key(&token.name) {
            return Err(AccountsError::Taken(token.name));
        }
        let mut tokens = self.tokens.clone();
        tokens.insert(token.name.clone(), token);
        Self::new(self.users.clone(), tokens)
    }

    /// The accounts without the token `name`.
    ///
    /// # Errors
    /// [`AccountsError::NoToken`]: there is none.
    pub fn without_token(&self, name: &str) -> Result<Self, AccountsError> {
        let mut tokens = self.tokens.clone();
        tokens
            .remove(name)
            .ok_or_else(|| AccountsError::NoToken(name.to_owned()))?;
        Self::new(self.users.clone(), tokens)
    }

    /// The accounts the stored records `users` and `tokens` hold, each by its name, as the file's
    /// reader reads them.
    ///
    /// # Errors
    /// [`AccountsError::Invalid`] naming `place` and the record: one does not read, or the accounts
    /// break a rule of [`Accounts::new`].
    pub(crate) fn of_records(
        place: &Path,
        users: impl IntoIterator<Item = (String, serde_json::Value)>,
        tokens: impl IntoIterator<Item = (String, serde_json::Value)>,
    ) -> Result<Self, AccountsError> {
        let refused = |why: String| AccountsError::Invalid {
            path: place.to_owned(),
            why,
        };
        let mut kept_users = BTreeMap::new();
        for (name, value) in users {
            let user = serde_json::from_value(value)
                .map_err(|e| refused(format!("the record {name}: {e}")))?;
            kept_users.insert(name, user);
        }
        let mut kept_tokens = BTreeMap::new();
        for (name, value) in tokens {
            let token = serde_json::from_value(value)
                .map_err(|e| refused(format!("the record {name}: {e}")))?;
            kept_tokens.insert(name, token);
        }
        Self::new(kept_users, kept_tokens).map_err(|why| refused(why.to_string()))
    }
}

/// The accounts as the text of the file `<state-file>.accounts`: JSON, a record for each user and
/// each token, keyed by name.
///
/// # Errors
/// The serialiser's, which a map of strings does not give.
pub(crate) fn encode_file(accounts: &Accounts) -> serde_json::Result<Vec<u8>> {
    let mut text = serde_json::to_vec_pretty(accounts)?;
    text.push(b'\n');
    Ok(text)
}

/// The accounts the file at `path` holds, in `bytes`.
///
/// # Errors
/// [`AccountsError::Invalid`]: it is not JSON of the shape, or breaks a rule of
/// [`Accounts::new`].
pub(crate) fn parse_file(path: &Path, bytes: &[u8]) -> Result<Accounts, AccountsError> {
    let refused = |why: String| AccountsError::Invalid {
        path: path.to_owned(),
        why,
    };
    let held: Accounts = serde_json::from_slice(bytes).map_err(|e| refused(e.to_string()))?;
    Accounts::new(held.users, held.tokens).map_err(|why| refused(why.to_string()))
}

/// The accounts stored, and the backend a change of them is committed to ([`crate::store`]).
pub struct AccountStore {
    held: Mutex<Accounts>,
    backend: Shared,
}

impl std::fmt::Debug for AccountStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountStore").finish_non_exhaustive()
    }
}

impl AccountStore {
    /// A store that keeps the accounts in memory only, for a coordinator with no state file: each
    /// write is taken as done.
    #[must_use]
    pub fn volatile() -> Self {
        Self::over(Arc::new(Fake::new()), Accounts::default())
    }

    /// The accounts stored in the file at `path`, none if it is not there; changes are written
    /// to it.
    ///
    /// # Errors
    /// [`AccountsError`]: the file cannot be read, or holds a value this build cannot read.
    pub fn open(path: &Path) -> Result<Self, AccountsError> {
        let files = Files::default().with_accounts(path);
        let held = files.load_accounts()?;
        Ok(Self::over(Arc::new(files), held))
    }

    /// The accounts `held`, as loaded from `backend`, to which their changes are committed.
    pub(crate) fn over(backend: Shared, held: Accounts) -> Self {
        Self {
            held: Mutex::new(held),
            backend,
        }
    }

    /// Takes the accounts of a commit of unknown outcome that `landing` found landed into memory,
    /// with what memory changed since laid over them.
    pub(crate) fn take_landed(&self, landing: &Landing) {
        let mut held = self.held();
        if let Some(taken) = landing.accounts_over(&held) {
            *held = taken;
        }
    }

    fn held(&self) -> MutexGuard<'_, Accounts> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The accounts as they are now.
    #[must_use]
    pub fn snapshot(&self) -> Accounts {
        self.held().clone()
    }

    /// Makes the accounts `edit` makes of the ones held, durable, and then the ones held. A change
    /// that leaves them as they were writes nothing.
    ///
    /// # Errors
    /// [`AccountsError`]: `edit` refused, or the backend did not take the change; nothing changed
    /// in memory.
    pub fn change(
        &self,
        edit: impl FnOnce(&Accounts) -> Result<Accounts, AccountsError>,
    ) -> Result<(), AccountsError> {
        let mut held = self.held();
        let next = edit(&held)?;
        if next == *held {
            return Ok(());
        }
        let commit = Commit {
            accounts: Some((&held, &next)),
            ..Commit::default()
        };
        self.backend.commit(&commit).map_err(|why| match why {
            BackendError::Accounts(why) => why,
            other => AccountsError::Io(other.into_store()),
        })?;
        *held = next;
        Ok(())
    }
}

#[cfg(test)]
#[path = "accounts_tests.rs"]
mod tests;
