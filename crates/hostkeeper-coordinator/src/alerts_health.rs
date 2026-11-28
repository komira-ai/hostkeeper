//! The host alerts derived from a host's hardware health (docs/design/host-health-alerts.md
//! section 5.1), one function per input. Built: [`storage_failing`] of a nearly full filesystem,
//! from the heartbeat's `HostHealth.filesystems`. The derivations from the hardware event log, the
//! store of each host's last health and the alerts' raising and clearing are HH5's, which calls
//! these.
//!
//! **A nearly full filesystem** is `storage_failing` at 90% used a warning and at 97% critical
//! ([`FULL_WARNING_PERCENT`], [`FULL_CRITICAL_PERCENT`]), "used" being what an unprivileged writer
//! can no longer write: the size less the available bytes, so the blocks reserved for root count as
//! used. It is not a hardware record: nothing of it is written to the host's log but the alert's
//! own raising and clearing. And it never cordons the host ([`HealthAlert::cordons`] is false at
//! every level): a full disk is cleaned, not repaired, and draining a host for it would stop its
//! work for nothing. Of the inputs of `storage_failing`, only a critical `disk` record cordons.
//!
//! **Clearing.** An alert of a filesystem ends at the first heartbeat that reports it under the
//! threshold, or reports it no longer: as each other input of `storage_failing` ends at the first
//! read that no longer finds it (a disk that passes SMART again, a filesystem writable again),
//! with no margin under the threshold. A heartbeat that reports no filesystem at all (a daemon of
//! a release before, or a heartbeat that left them out to keep its bound) says nothing of
//! fullness: [`storage_failing`] gives none, and the caller keeps what it had.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;

/// The percent used at which a filesystem is a warning.
pub const FULL_WARNING_PERCENT: u64 = 90;

/// The percent used at which a filesystem is critical.
pub const FULL_CRITICAL_PERCENT: u64 = 97;

/// The code of a nearly full filesystem's finding (`filesystem_full/<mount>`).
pub const FILESYSTEM_FULL: &str = "filesystem_full";

/// One alert a host's health derives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HealthAlert {
    /// Its kind.
    pub kind: proto::AlertKind,
    /// Its severity.
    pub severity: proto::AlertSeverity,
    /// What it is about within its kind (`Alert.finding`): the code and the subject,
    /// `filesystem_full/<mount>` for a filesystem.
    pub finding: String,
    /// One line a person reads.
    pub message: String,
    /// Whether it cordons the host for repair (design section 6).
    pub cordons: bool,
}

/// The percent of `filesystem` used, rounded down: its size less its available bytes, of its
/// size; none for a filesystem of no size. Available bytes past the size are read as all of it.
#[must_use]
pub fn used_percent(filesystem: &agent::FilesystemUse) -> Option<u64> {
    let size = u128::from(filesystem.size_bytes);
    let available = u128::from(filesystem.available_bytes).min(size);
    let percent = ((size - available) * 100).checked_div(size)?;
    u64::try_from(percent).ok()
}

/// The `storage_failing` alerts of the filesystems `health` reports (the module's doc), in the
/// order it lists them: one for each at 90% used or more, critical from 97%, none cordoning. None
/// when `health` lists no filesystem: no word on fullness, which ends no alert.
#[must_use]
pub fn storage_failing(health: &agent::HostHealth) -> Option<Vec<HealthAlert>> {
    if health.filesystems.is_empty() {
        return None;
    }
    let alerts = health.filesystems.iter().filter_map(|filesystem| {
        let percent = used_percent(filesystem)?;
        let severity = if percent >= FULL_CRITICAL_PERCENT {
            proto::AlertSeverity::Critical
        } else if percent >= FULL_WARNING_PERCENT {
            proto::AlertSeverity::Warning
        } else {
            return None;
        };
        Some(HealthAlert {
            kind: proto::AlertKind::StorageFailing,
            severity,
            finding: format!("{FILESYSTEM_FULL}/{}", filesystem.mount),
            message: format!(
                "filesystem {} is {percent}% full: {} of {} bytes available",
                filesystem.mount, filesystem.available_bytes, filesystem.size_bytes
            ),
            cordons: false,
        })
    });
    Some(alerts.collect())
}

#[cfg(test)]
#[path = "alerts_health_tests.rs"]
mod tests;
