//! The `action` strings of the operator login's audit entries (`docs/design/operator-login.md`
//! section 8), of the MDM setup's (`docs/design/coordinator-mdm.md`, section 4.9, the `mdm_*`
//! ones), of the console's (`docs/design/console-broker.md`, decision D4), of the Xcode store's
//! (`xcode_put`, `xcode_remove`) and of the maintenance runs' (`maintenance_schedule`,
//! `maintenance_cancel`, and the coordinator's `maintenance_start` and `maintenance_step`). Each is an additive string of the existing `action` field; `by` and
//! `subject` say who did it and whom or what it concerned.
//!
//! The console's lines change no operations state: they are appended with
//! [`crate::ops::OpsStore::append_audit_only`], which takes their action as an [`AuditOnly`], and
//! stay out of the ring that `/v1/status` shows.

use crate::ops::{AuditBy, AuditEntry};

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
/// The coordinator made the CA of the Macs' identities (the first `/enroll` or `/scep` needing it).
pub const MDM_CA_CREATED: &str = "mdm_ca_created";
/// `/enroll` declined the serial `subject`: it is not in the ABM table. Audited once for a while.
pub const MDM_ENROLL_DECLINED: &str = "mdm_enroll_declined";
/// `/enroll` could not verify the blob of the serial `subject` and holds the request for an
/// operator; the reason is the verifier's.
pub const MDM_ENROLL_HELD: &str = "mdm_enroll_held";
/// An operator confirmed the held request of the serial `subject`, after the ABM table listed it.
pub const MDM_ENROLL_CONFIRMED: &str = "mdm_enroll_confirmed";

/// An operator opened a console session on the host `node_id`; written before the device is
/// contacted.
pub const CONSOLE_OPEN: &str = "console_open";
/// A console session on the host `node_id` ended, by the operator who closed it or by
/// `coordinator`.
pub const CONSOLE_CLOSE: &str = "console_close";
/// A console call of an operator named in the console operators file was refused; the reason
/// names the cause.
pub const CONSOLE_DENIED: &str = "console_denied";
/// An operator sent a power command to the BMC of the host `node_id`.
pub const POWER_ACTION: &str = "power_action";
/// An operator set the boot device of the host `node_id` at its BMC.
pub const BOOT_DEVICE_SET: &str = "boot_device_set";
/// The coordinator started, by `coordinator`: every console session open before it ended there.
pub const CONSOLE_COORDINATOR_START: &str = "console_coordinator_start";

/// An operator's upload of an Xcode archive was stored (`PUT /v1/xcode/<sha256>`): the reason names
/// the release, the archive's SHA-256 and its size.
pub const XCODE_PUT: &str = "xcode_put";
/// An Xcode archive was removed from the store: by an operator's `Xcode.Remove`, whose reason is
/// the operator's, or by `coordinator`, which prunes the releases the store no longer keeps and
/// whose reason names the release and the archive's SHA-256.
pub const XCODE_REMOVE: &str = "xcode_remove";

/// An operator's `Maintenance.Schedule` stored a run: the reason is the operator's followed by
/// `run <id>`, and the entry's `host_group` the group the run names, if it names one.
pub const MAINTENANCE_SCHEDULE: &str = "maintenance_schedule";
/// An operator's `Maintenance.Cancel` cancelled a run: the reason is the operator's followed by
/// `run <id>`.
pub const MAINTENANCE_CANCEL: &str = "maintenance_cancel";
/// The coordinator started a host's turn of a run: it sent the host the run's `maintain` and
/// stored its step as `staging` (`crate::decide_maintenance`). By `coordinator`, with the host as
/// the entry's `node_id`, its group as its `host_group` when it is in one, and the reason
/// `run <id>`.
pub const MAINTENANCE_START: &str = "maintenance_start";
/// A host's daemon reported its turn of a run at another step, or with other kinds applied or
/// another reason, and the coordinator stored it. By `coordinator`, with the host as the entry's
/// `node_id`, its group as its `host_group` when it is in one, and the reason `run <id> <step>`.
pub const MAINTENANCE_STEP: &str = "maintenance_step";

/// The action of an audit line that changes no operations state: one of the console's, which
/// [`crate::ops::OpsStore::append_audit_only`] appends outside the `/v1/status` ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuditOnly {
    /// [`CONSOLE_OPEN`].
    ConsoleOpen,
    /// [`CONSOLE_CLOSE`].
    ConsoleClose,
    /// [`CONSOLE_DENIED`].
    ConsoleDenied,
    /// [`POWER_ACTION`].
    PowerAction,
    /// [`BOOT_DEVICE_SET`].
    BootDeviceSet,
    /// [`CONSOLE_COORDINATOR_START`].
    ConsoleCoordinatorStart,
}

impl AuditOnly {
    /// Every audit-only action, in the order of the design.
    pub const ALL: [AuditOnly; 6] = [
        AuditOnly::ConsoleOpen,
        AuditOnly::ConsoleClose,
        AuditOnly::ConsoleDenied,
        AuditOnly::PowerAction,
        AuditOnly::BootDeviceSet,
        AuditOnly::ConsoleCoordinatorStart,
    ];

    /// The action's string, as the audit line carries it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            AuditOnly::ConsoleOpen => CONSOLE_OPEN,
            AuditOnly::ConsoleClose => CONSOLE_CLOSE,
            AuditOnly::ConsoleDenied => CONSOLE_DENIED,
            AuditOnly::PowerAction => POWER_ACTION,
            AuditOnly::BootDeviceSet => BOOT_DEVICE_SET,
            AuditOnly::ConsoleCoordinatorStart => CONSOLE_COORDINATOR_START,
        }
    }

    /// Whether `action` is the string of an audit-only action: a line that a store which
    /// rebuilds the ring from its audit leaves out of it.
    #[must_use]
    pub fn is_audit_only(action: &str) -> bool {
        Self::ALL.iter().any(|only| only.as_str() == action)
    }
}

/// The entry of `action` by `by` concerning `subject`.
#[must_use]
pub fn about(by: &(impl AuditBy + ?Sized), action: &str, subject: &str) -> AuditEntry {
    let mut entry = AuditEntry::new(by, action);
    entry.subject = Some(subject.to_owned());
    entry
}

/// The entry of `action` by `by` that came from the peer `source`.
#[must_use]
pub fn from_peer(by: &(impl AuditBy + ?Sized), action: &str, source: &str) -> AuditEntry {
    let mut entry = AuditEntry::new(by, action);
    entry.source = Some(source.to_owned());
    entry
}
