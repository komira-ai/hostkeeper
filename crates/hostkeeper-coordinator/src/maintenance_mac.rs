//! What a Mac must report before it is ordered `macos` (docs/design/maintenance-updates.md,
//! sections 3, 5.1, 5.5 and 5.8), read from the report its daemon sent in its last heartbeat
//! ([`crate::maintenance::Kept`]). `Maintenance.Schedule` reads it when it takes a host in a run
//! ([`crate::maintenance_service`]), and the heartbeat decision again when the host would be
//! started ([`crate::decide_maintenance`]), so a report that changed while the host waited is the
//! one that counts.
//!
//! [`macos_skip`] says why a host may not be ordered `macos`, the first of, in this order:
//!
//! 1. **`mdm_conflict`:** the Mac reports a device-management setting that must not be set on it
//!    (section 5.8), any one of `MacUpdateFacts.mdm_conflicts`.
//! 2. **`no_update_account`:** its update account is absent, not an administrator, or holds no
//!    Secure Token (section 5.5); or the report carries no Mac facts at all, so nothing shows the
//!    account. The coordinator's one path is `daemon` (`--macos-update-via` refuses
//!    `apple-business`), which installs with that account, so this rule always applies.
//! 3. **`filevault_not_ready`:** FileVault is not reported off. The design lets a Mac with
//!    FileVault on be ordered once its update account is a FileVault user and `fdesetup
//!    supportsauthrestart` says yes; the report says the second (`authrestart_supported`) but not
//!    the first, so no Mac with FileVault on, being turned on or off, or not read, is ordered
//!    `macos` until the report carries it. This also keeps a major upgrade, whose authenticated
//!    restart is still to be shown on such a Mac, off every Mac with FileVault on.
//!
//! The skip is for `macos` alone: a host's other kinds of the same run (an `xcode` install on that
//! Mac) still go, and the reason is stored on the host's turn as `skipped_because`, with the step
//! `skipped` only when `macos` was all its turn held.
//!
//! [`counts_as_due`] is the group's `macos_major`: a due item that is a major upgrade of macOS
//! (its `from` and `to` differ in their first component, `26.1` to `27.0`) counts as due only when
//! the group allows one. The `maintain` order carries the same setting (`Maintain.macos_major`),
//! so a daemon of a group that does not allow one stages only updates within its major version.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;

use crate::maintenance_runs::Skip;

/// Why the host whose kept report is `report` (none: the coordinator keeps none) may not be
/// ordered `macos`, the first rule of the module's it fails; none when it may.
pub(crate) fn macos_skip(report: Option<&agent::MaintenanceReport>) -> Option<Skip> {
    let Some(mac) = report.and_then(|report| report.mac.as_ref()) else {
        return Some(Skip::NoUpdateAccount);
    };
    if !mac.mdm_conflicts.is_empty() {
        return Some(Skip::MdmConflict);
    }
    let ready = mac
        .update_account
        .as_ref()
        .is_some_and(|account| account.present && account.admin && account.secure_token);
    if !ready {
        return Some(Skip::NoUpdateAccount);
    }
    if mac.filevault() != agent::FileVaultState::Off {
        return Some(Skip::FilevaultNotReady);
    }
    None
}

/// Whether the due item `item` counts as due in a group whose `macos_major` is `macos_major`:
/// every item but a major upgrade of macOS, which counts only when the group allows one.
pub(crate) fn counts_as_due(item: &agent::DueItem, macos_major: bool) -> bool {
    macos_major || item.kind != i32::from(agent::MaintenanceKind::Macos) || !is_major(item)
}

/// Whether `item` moves macOS to another major version: it names the version the Mac has and the
/// first component of that differs from the first of the version it would have.
fn is_major(item: &agent::DueItem) -> bool {
    !item.from.is_empty() && major(&item.from) != major(&item.to)
}

/// The first component of the version `version` (`27` of `27.0.1`).
fn major(version: &str) -> &str {
    version.split('.').next().unwrap_or(version)
}

#[cfg(test)]
#[path = "maintenance_mac_tests.rs"]
mod tests;
