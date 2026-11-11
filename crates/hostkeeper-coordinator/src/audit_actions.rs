//! The `action` strings of the operator login's audit entries (`docs/design/operator-login.md`
//! section 8) and of the MDM setup's (`docs/design/coordinator-mdm.md`, section 4.9, the `mdm_*`
//! ones). Each is an additive string of the existing `action` field; `by` and `subject` say who
//! did it and whom or what it concerned.

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

/// The push request was made (`mdm_push_csr`): its key and encryption key were sealed.
pub const MDM_PUSH_CSR: &str = "mdm_push_csr";
/// The reply of mdmcert.download was uploaded.
pub const MDM_PUSH_REPLY_PUT: &str = "mdm_push_reply_put";
/// The push certificate, or the key of an existing one, was stored.
pub const MDM_PUSH_CERT_PUT: &str = "mdm_push_cert_put";
/// The public ABM certificate was downloaded.
pub const MDM_ABM_CERT_GET: &str = "mdm_abm_cert_get";
/// The ABM token was stored.
pub const MDM_ABM_TOKEN_PUT: &str = "mdm_abm_token_put";
/// The MDM name was set.
pub const MDM_NAME_SET: &str = "mdm_name_set";
/// The TLS certificate or its key was stored.
pub const MDM_TLS_PUT: &str = "mdm_tls_put";
/// The ACME account key or the DNS credential was stored.
pub const MDM_ACME_CREDENTIAL_PUT: &str = "mdm_acme_credential_put";
/// Another kind of MDM secret was stored (the ABM key and certificate, the MDM CA key, the
/// encryption key of the push request).
pub const MDM_SECRET_PUT: &str = "mdm_secret_put";
/// An MDM setting was set.
pub const MDM_SETTING_SET: &str = "mdm_setting_set";
/// The MDM setup was verified (`Mdm.Verify`): `subject` the enrollment a test push went to, if any.
pub const MDM_VERIFY: &str = "mdm_verify";

/// A new enrollment replaced the live enrollment of the serial `subject` (owner decision B):
/// the serial is in the ABM table, or an operator confirmed the identity.
pub const MDM_ENROLLMENT_REPLACED: &str = "mdm_enrollment_replaced";
/// A new enrollment of the serial `subject` was refused: a live enrollment stands, the serial is not
/// in the ABM table and no operator confirmed the identity.
pub const MDM_REPLACE_REFUSED: &str = "mdm_replace_refused";
/// An `Authenticate` named another serial than the identity's, which is `subject`.
pub const MDM_SERIAL_MISMATCH: &str = "mdm_serial_mismatch";

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
