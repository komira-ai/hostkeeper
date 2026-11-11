//! `Users` and `ApiTokens` of `hostkeeper.operator.v1`, as plain functions over their messages, as
//! the other operator calls are ([`crate::operator`]): the operator's users and the API tokens
//! that the store keeps ([`crate::accounts`]; `docs/design/operator-login.md`, section 7).
//!
//! Every call takes an operator: a bearer of the tokens file or of the store, or the session of a
//! signed-in user. A valid CI token is `forbidden`, and no credential `unauthorized`, before the
//! request is looked at. A user and a token are one namespace, and the tokens file's names are in
//! it: a name that is already a user's, a stored token's or the tokens file's is `name_taken`.
//!
//! A change is judged first, on the accounts as they are, so that a call that would be refused
//! writes nothing; then audited (a call whose audit line cannot be written changes nothing:
//! `audit_unavailable`); then made under the store's own lock, which judges it again. A change the
//! store then cannot write is `store_failed`, and leaves the audit entry that was written before it,
//! the order of the password change too.
//!
//! A password or a token the coordinator makes is shown once, in the answer, and only its hash
//! (Argon2id) or its SHA-256 is kept. The temporary password of `Create` and `ResetPassword` is
//! 24 characters of an alphabet without look-alikes, drawn from the sign-in's random bytes, good
//! for `--initial-password-secs` and to be changed at the first sign-in, as the first admin's is.
//! A reset also deletes the initial-admin file that made the user's password, if it is still
//! there, so its expiry cannot void the new one. A disabled or deleted user's sessions end at
//! once.
//!
//! [`Coordinator::operator`] resolves a bearer against the tokens file and the store's tokens in
//! one constant-time pass ([`crate::tokens::OperatorTokens::name_of_among`]).

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::ApiTokensRefusal as TokenRefusal;
use hostkeeper_proto::hostkeeper::operator::v1::UsersRefusal as UserRefusal;

use crate::accounts::{Accounts, AccountsError, ApiToken, User};
use crate::audit_actions::{
    API_TOKEN_CREATE, API_TOKEN_REVOKE, PASSWORD_RESET, USER_CREATE, USER_DELETE, USER_DISABLE,
    USER_ENABLE, about,
};
use crate::login::Login;
use crate::operator::Caller;
use crate::ops::{AuditEntry, OpsError};
use crate::ops_file::digest;
use crate::service::{Coordinator, Unauthorized};
use crate::tokens::TokenName;

/// The characters of a temporary password: letters and digits but `0`, `1`, `I`, `O` and `l`.
const ALPHABET: &[u8; 57] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";

/// How many characters a temporary password has.
const PASSWORD_CHARS: usize = 24;

/// The first byte value that is not used: 228 is four times the alphabet, so a byte below it
/// picks every character with the same chance.
const FIRST_UNUSED_BYTE: u8 = 228;

/// How many random bytes a token has.
const TOKEN_BYTES: usize = 32;

/// The prefix of every token made here, which lets a secret scanner find a leaked one.
pub const TOKEN_PREFIX: &str = "hkt_";

/// A temporary password of [`PASSWORD_CHARS`] characters, from the bytes `fill` gives; none if it
/// gives none. A byte that would favour a character is dropped and another drawn.
fn random_password(fill: impl Fn(&mut [u8]) -> bool) -> Option<String> {
    let mut password = String::with_capacity(PASSWORD_CHARS);
    let mut bytes = [0u8; 2 * PASSWORD_CHARS];
    while password.len() < PASSWORD_CHARS {
        if !fill(&mut bytes) {
            return None;
        }
        for byte in bytes.into_iter().filter(|byte| *byte < FIRST_UNUSED_BYTE) {
            if password.len() < PASSWORD_CHARS {
                password.push(char::from(ALPHABET[usize::from(byte) % ALPHABET.len()]));
            }
        }
    }
    Some(password)
}

/// A temporary password and its hash, drawn and made under `login`.
fn temporary(login: &Login) -> Option<(String, String)> {
    random_password(|bytes| login.fill(bytes)).and_then(|password| {
        login
            .passwords
            .hash(&password)
            .ok()
            .map(|hash| (password, hash.as_str().to_owned()))
    })
}

/// A token of [`TOKEN_BYTES`] random bytes from `login`, with its prefix; none if there are none.
fn new_secret(login: &Login) -> Option<String> {
    let mut bytes = [0u8; TOKEN_BYTES];
    login
        .fill(&mut bytes)
        .then(|| format!("{TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes)))
}

/// A user as the answer says it: never the hash.
fn user_info(user: &User) -> proto::UserInfo {
    proto::UserInfo {
        name: user.name.clone(),
        enabled: user.enabled,
        must_change: user.must_change,
        created_by: user.created_by.clone(),
        created_unix: user.created_at,
        last_login_unix: user.last_login_at,
        password_valid_until_unix: user.initial_expires_at.filter(|_| user.must_change),
    }
}

/// A token of the store as the answer says it: never the digest.
fn stored_info(token: &ApiToken) -> proto::ApiTokenInfo {
    proto::ApiTokenInfo {
        name: token.name.clone(),
        source: proto::ApiTokenSource::Store.into(),
        created_by: token.created_by.clone(),
        created_unix: token.created_at,
    }
}

/// Why a change of the accounts did not happen.
enum Stopped {
    /// The change was refused, or the store could not write it.
    Accounts(AccountsError),
    /// The audit line could not be written.
    Audit(OpsError),
}

impl Stopped {
    /// The refusal of `Users`.
    fn user(self) -> UserRefusal {
        match self {
            Self::Audit(why) => why.into(),
            Self::Accounts(AccountsError::Taken(_)) => UserRefusal::NameTaken,
            Self::Accounts(AccountsError::NoUser(_)) => UserRefusal::UnknownUser,
            Self::Accounts(AccountsError::LastEnabled) => UserRefusal::LastEnabled,
            Self::Accounts(why) => {
                tracing::error!(%why, "a change of the users was not written: nothing changed");
                UserRefusal::StoreFailed
            }
        }
    }

    /// The refusal of `ApiTokens`.
    fn token(self) -> TokenRefusal {
        match self {
            Self::Audit(why) => why.into(),
            Self::Accounts(AccountsError::Taken(_)) => TokenRefusal::NameTaken,
            Self::Accounts(AccountsError::NoToken(_)) => TokenRefusal::UnknownToken,
            Self::Accounts(why) => {
                tracing::error!(%why, "a change of the API tokens was not written: nothing changed");
                TokenRefusal::StoreFailed
            }
        }
    }
}

impl Coordinator {
    /// The name of the operator token in `bearer`: the token is compared by its SHA-256 in
    /// constant time with each token of the tokens file and each API token of the store. CI's
    /// token is no operator token and an operator token is no CI token.
    ///
    /// # Errors
    /// [`Unauthorized`]: no bearer, or not a token.
    pub fn operator(&self, bearer: Option<&str>) -> Result<TokenName, Unauthorized> {
        bearer
            .and_then(|presented| {
                self.accounts().read(|accounts| {
                    let stored = accounts
                        .tokens()
                        .map(|token| (token.name.as_str(), token.sha256.as_str()));
                    self.tokens.name_of_among(presented, stored)
                })
            })
            .ok_or(Unauthorized)
    }

    /// Makes the change `edit` of the accounts, audited as `entry` first.
    fn change_accounts(
        &self,
        entry: AuditEntry,
        edit: impl Fn(&Accounts) -> Result<Accounts, AccountsError>,
    ) -> Result<(), Stopped> {
        edit(&self.accounts().snapshot()).map_err(Stopped::Accounts)?;
        self.audit(entry).map_err(Stopped::Audit)?;
        self.accounts().change(edit).map_err(Stopped::Accounts)
    }

    /// The sign-in a call that makes a password or a token needs. A coordinator has one unless it
    /// was built without; the call then changes nothing.
    fn signing(&self) -> Option<&Login> {
        let login = self.login();
        if login.is_none() {
            tracing::error!("the coordinator has no sign-in: no password or token can be made");
        }
        login
    }

    /// Ends every session of `name`.
    fn end_sessions(&self, name: &str) {
        if let Some(login) = self.login() {
            login.sessions.end_user(name, None);
        }
    }

    /// The user `name` as the answer says it: none if a call that removed them came between.
    fn user_now(&self, name: &str) -> Option<proto::UserInfo> {
        self.accounts()
            .read(|accounts| accounts.user(name).map(user_info))
    }

    /// `Users.List`: every user, by name. Changes nothing.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`.
    pub fn users_list(
        &self,
        caller: &Caller,
        _request: &proto::ListUsersRequest,
    ) -> Result<proto::ListUsersResponse, UserRefusal> {
        caller.operator()?;
        Ok(proto::ListUsersResponse {
            users: self
                .accounts()
                .read(|accounts| accounts.users().map(user_info).collect()),
        })
    }

    /// `Users.Create`: a user with a temporary password, which is in the answer and nowhere else.
    /// Audited as `user_create`.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (the name), `name_taken`, `store_failed`
    /// (the store, or no random bytes), `audit_unavailable`.
    pub fn users_create(
        &self,
        caller: &Caller,
        request: &proto::CreateUserRequest,
    ) -> Result<proto::CreateUserResponse, UserRefusal> {
        let by = caller.operator()?;
        let name: TokenName = request
            .name
            .parse()
            .map_err(|_| UserRefusal::InvalidRequest)?;
        if self.tokens.has_name(name.as_str()) {
            return Err(UserRefusal::NameTaken);
        }
        let login = self.signing().ok_or(UserRefusal::StoreFailed)?;
        let (password, hash) = temporary(login).ok_or(UserRefusal::StoreFailed)?;
        let now = login.now();
        let user = User {
            name: name.as_str().to_owned(),
            hash,
            enabled: true,
            must_change: true,
            initial_expires_at: Some(now.saturating_add(login.temporary_secs())),
            created_by: by.to_owned(),
            created_at: now,
            ..User::default()
        };
        self.change_accounts(about(by, USER_CREATE, name.as_str()), |held| {
            held.with_user(user.clone())
        })
        .map_err(Stopped::user)?;
        Ok(proto::CreateUserResponse {
            user: Some(user_info(&user)),
            password,
        })
    }

    /// Enables or disables the user `name`, audited as `action`.
    fn set_enabled(
        &self,
        caller: &Caller,
        name: &str,
        enabled: bool,
        action: &str,
    ) -> Result<Option<proto::UserInfo>, UserRefusal> {
        let by = caller.operator()?;
        self.change_accounts(about(by, action, name), |held| {
            held.with_enabled(name, enabled)
        })
        .map_err(Stopped::user)?;
        Ok(self.user_now(name))
    }

    /// `Users.Disable`: the user cannot sign in, and their sessions end at once. Audited as
    /// `user_disable`.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `unknown_user`, `last_enabled`, `store_failed`,
    /// `audit_unavailable`.
    pub fn users_disable(
        &self,
        caller: &Caller,
        request: &proto::DisableUserRequest,
    ) -> Result<proto::DisableUserResponse, UserRefusal> {
        let user = self.set_enabled(caller, &request.name, false, USER_DISABLE)?;
        self.end_sessions(&request.name);
        Ok(proto::DisableUserResponse { user })
    }

    /// `Users.Enable`: the user may sign in again. Audited as `user_enable`.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `unknown_user`, `store_failed`, `audit_unavailable`.
    pub fn users_enable(
        &self,
        caller: &Caller,
        request: &proto::EnableUserRequest,
    ) -> Result<proto::EnableUserResponse, UserRefusal> {
        self.set_enabled(caller, &request.name, true, USER_ENABLE)
            .map(|user| proto::EnableUserResponse { user })
    }

    /// `Users.ResetPassword`: a new temporary password, in the answer and nowhere else; the user
    /// must change it at the next sign-in, within the hour, and their sessions end. Audited as
    /// `password_reset`, by the caller, about the user.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `own_password` (the caller's own name), `unknown_user`,
    /// `store_failed`, `audit_unavailable`.
    pub fn users_reset_password(
        &self,
        caller: &Caller,
        request: &proto::ResetPasswordRequest,
    ) -> Result<proto::ResetPasswordResponse, UserRefusal> {
        let by = caller.operator()?;
        let name = request.name.as_str();
        if by == name {
            return Err(UserRefusal::OwnPassword);
        }
        let login = self.signing().ok_or(UserRefusal::StoreFailed)?;
        let (password, hash) = temporary(login).ok_or(UserRefusal::StoreFailed)?;
        let until = login.now().saturating_add(login.temporary_secs());
        let adopted = self
            .accounts()
            .read(|accounts| accounts.user(name).and_then(|user| user.initial_id.clone()));
        self.change_accounts(about(by, PASSWORD_RESET, name), |held| {
            held.with_user_edited(name, |user| {
                user.hash.clone_from(&hash);
                user.must_change = true;
                user.initial_expires_at = Some(until);
                user.initial_void = false;
            })
        })
        .map_err(Stopped::user)?;
        login.sessions.end_user(name, None);
        if let Some(id) = adopted
            && let Some(file) = &self.initial_admin
            && let Err(why) = file.delete_if_adopted(name, &id)
        {
            tracing::warn!(%why, "the initial-admin file was not deleted");
        }
        Ok(proto::ResetPasswordResponse {
            user: self.user_now(name),
            password,
        })
    }

    /// `Users.Delete`: the user is removed and their sessions end. Audited as `user_delete`.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `unknown_user`, `last_enabled`, `store_failed`,
    /// `audit_unavailable`.
    pub fn users_delete(
        &self,
        caller: &Caller,
        request: &proto::DeleteUserRequest,
    ) -> Result<proto::DeleteUserResponse, UserRefusal> {
        let by = caller.operator()?;
        let name = request.name.as_str();
        self.change_accounts(about(by, USER_DELETE, name), |held| held.without_user(name))
            .map_err(Stopped::user)?;
        self.end_sessions(name);
        Ok(proto::DeleteUserResponse {})
    }

    /// `ApiTokens.List`: the tokens of the tokens file, which are read-only, then those of the
    /// store, each by name. Changes nothing.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`.
    pub fn api_tokens_list(
        &self,
        caller: &Caller,
        _request: &proto::ListApiTokensRequest,
    ) -> Result<proto::ListApiTokensResponse, TokenRefusal> {
        caller.operator()?;
        let mut tokens: Vec<proto::ApiTokenInfo> = self
            .tokens
            .names()
            .into_iter()
            .map(|name| proto::ApiTokenInfo {
                name,
                source: proto::ApiTokenSource::TokensFile.into(),
                ..proto::ApiTokenInfo::default()
            })
            .collect();
        tokens.extend(
            self.accounts()
                .read(|accounts| accounts.tokens().map(stored_info).collect::<Vec<_>>()),
        );
        Ok(proto::ListApiTokensResponse { tokens })
    }

    /// `ApiTokens.Create`: a token, which is in the answer and nowhere else. Audited as
    /// `api_token_create`.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (the name), `name_taken`, `store_failed`
    /// (the store, no random bytes, or a token that is another's), `audit_unavailable`.
    pub fn api_tokens_create(
        &self,
        caller: &Caller,
        request: &proto::CreateApiTokenRequest,
    ) -> Result<proto::CreateApiTokenResponse, TokenRefusal> {
        let by = caller.operator()?;
        let name: TokenName = request
            .name
            .parse()
            .map_err(|_| TokenRefusal::InvalidRequest)?;
        if self.tokens.has_name(name.as_str()) {
            return Err(TokenRefusal::NameTaken);
        }
        let login = self.signing().ok_or(TokenRefusal::StoreFailed)?;
        let secret = new_secret(login).ok_or(TokenRefusal::StoreFailed)?;
        let sha256 = digest(secret.as_bytes());
        if self.tokens.has_digest(&sha256) {
            tracing::error!("a token made here is one of the tokens file's: none was made");
            return Err(TokenRefusal::StoreFailed);
        }
        let token = ApiToken {
            name: name.as_str().to_owned(),
            sha256,
            created_by: by.to_owned(),
            created_at: login.now(),
        };
        self.change_accounts(about(by, API_TOKEN_CREATE, name.as_str()), |held| {
            held.with_token(token.clone())
        })
        .map_err(Stopped::token)?;
        Ok(proto::CreateApiTokenResponse {
            token: Some(stored_info(&token)),
            secret,
        })
    }

    /// `ApiTokens.Revoke`: the token is refused from now on. Audited as `api_token_revoke`.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `unknown_token` (none in the store: a token of the tokens
    /// file is removed by editing the file), `store_failed`, `audit_unavailable`.
    pub fn api_tokens_revoke(
        &self,
        caller: &Caller,
        request: &proto::RevokeApiTokenRequest,
    ) -> Result<proto::RevokeApiTokenResponse, TokenRefusal> {
        let by = caller.operator()?;
        let name = request.name.as_str();
        self.change_accounts(about(by, API_TOKEN_REVOKE, name), |held| {
            held.without_token(name)
        })
        .map_err(Stopped::token)?;
        Ok(proto::RevokeApiTokenResponse {})
    }
}

#[cfg(test)]
#[path = "operator_accounts_tests.rs"]
mod tests;
#[cfg(test)]
#[path = "operator_accounts_tokens_tests.rs"]
mod tokens_tests;
