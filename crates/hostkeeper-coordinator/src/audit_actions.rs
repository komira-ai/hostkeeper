//! The `action` strings of the operator login's audit entries (`docs/design/operator-login.md`
//! section 8). Each is an additive string of the existing `action` field; `by` and `subject` say
//! who did it and whom or what it concerned.

use crate::ops::AuditEntry;

/// A successful login, by the user.
pub const LOGIN: &str = "login";
/// The first failed login of a window, by `coordinator`, the name as `subject` when it is a valid
/// name.
pub const LOGIN_FAILED: &str = "login_failed";
/// The lock of a name starts, by `coordinator`.
pub const LOGIN_LOCKED: &str = "login_locked";
/// An explicit logout, by the user.
pub const LOGOUT: &str = "logout";
/// A user changed their own password.
pub const PASSWORD_CHANGED: &str = "password_changed";
/// An admin reset a user's password: `by` the admin, `subject` the user.
pub const PASSWORD_RESET: &str = "password_reset";
/// An initial-admin file was adopted, by `coordinator`.
pub const USER_BOOTSTRAP: &str = "user_bootstrap";
/// The hour of an initial password passed, by `coordinator`.
pub const INITIAL_PASSWORD_EXPIRED: &str = "initial_password_expired";
/// An admin created a user.
pub const USER_CREATE: &str = "user_create";
/// An admin disabled a user.
pub const USER_DISABLE: &str = "user_disable";
/// An admin enabled a user.
pub const USER_ENABLE: &str = "user_enable";
/// An admin deleted a user.
pub const USER_DELETE: &str = "user_delete";
/// An admin created an API token: `subject` the token's name.
pub const API_TOKEN_CREATE: &str = "api_token_create";
/// An admin revoked an API token: `subject` the token's name.
pub const API_TOKEN_REVOKE: &str = "api_token_revoke";

/// The entry of `action` by `by` concerning `subject`.
#[must_use]
pub fn about(by: &str, action: &str, subject: &str) -> AuditEntry {
    let mut entry = AuditEntry::new(by, action);
    entry.subject = Some(subject.to_owned());
    entry
}

/// The entry of `action` by `by` that came from the peer `source`.
#[must_use]
pub fn from_peer(by: &str, action: &str, source: &str) -> AuditEntry {
    let mut entry = AuditEntry::new(by, action);
    entry.source = Some(source.to_owned());
    entry
}
