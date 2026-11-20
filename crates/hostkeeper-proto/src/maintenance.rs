//! The bounds of a maintenance report (`hostkeeper.agent.v1.MaintenanceReport`, the
//! `maintenance` of a heartbeat), as `hostkeeper/agent/v1/maintenance.proto` gives them field by
//! field, and [`check_report`], which refuses a report that breaks one. The bounds keep a report
//! at every bound within [`MAX_REPORT_BYTES`] encoded, well inside the heartbeat's 16 KiB.

use crate::hostkeeper::agent::v1::MaintenanceReport;

/// The most bytes a report keeping every bound encodes to.
pub const MAX_REPORT_BYTES: usize = 5120;

/// The most Xcodes of `xcodes`.
pub const MAX_XCODES: usize = 6;
/// The most items of `due`.
pub const MAX_DUE: usize = 12;
/// The most keys of `mac.mdm_conflicts`.
pub const MAX_MDM_CONFLICTS: usize = 16;
/// The most kinds of `run.applied`.
pub const MAX_APPLIED: usize = 4;

/// The most bytes of each of `host.os_name`, `host.os_version`, `host.os_build` and
/// `host.hardware_serial`.
pub const MAX_OS_TEXT: usize = 32;
/// The most bytes of each of `host.running_kernel`, `host.newest_kernel` and `host.boot_id`.
pub const MAX_KERNEL_TEXT: usize = 64;
/// The most bytes of an Xcode's `version` and of its `build`.
pub const MAX_XCODE_VERSION: usize = 16;
/// The most bytes of an Xcode's `path`.
pub const MAX_XCODE_PATH: usize = 128;
/// The most bytes of `mac.command_line_tools.version` and of each key of `mac.mdm_conflicts`.
pub const MAX_MAC_TEXT: usize = 64;
/// The most bytes of a due item's `from` and of its `to`.
pub const MAX_DUE_VERSION: usize = 32;
/// The most bytes of a due item's `label`.
pub const MAX_DUE_LABEL: usize = 64;
/// The most bytes of `run.run_id` and of `run.order_id`.
pub const MAX_RUN_ID: usize = 64;
/// The most bytes of `run.halted_reason`.
pub const MAX_HALTED_REASON: usize = 256;
/// The most bytes of `look_failed`.
pub const MAX_LOOK_FAILED: usize = 256;

/// The bound of a maintenance report a report breaks. A field is named by its path in the
/// report, a list's entries all by the list's name (`due.label`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MaintenanceReportError {
    /// A list holds more entries than its bound.
    #[error("{field} holds {count} entries, more than {max}")]
    TooMany {
        /// The list.
        field: &'static str,
        /// How many entries it holds.
        count: usize,
        /// Its bound.
        max: usize,
    },
    /// A text is longer than its bound.
    #[error("{field} is {len} bytes, more than {max}")]
    TooLong {
        /// The text.
        field: &'static str,
        /// Its length in bytes.
        len: usize,
        /// Its bound.
        max: usize,
    },
}

/// Whether `report` keeps every bound: first each list's count, in the order of the fields, then
/// each text, in the same order.
///
/// # Errors
/// [`MaintenanceReportError`]: the first bound it breaks.
pub fn check_report(report: &MaintenanceReport) -> Result<(), MaintenanceReportError> {
    let mac = report.mac.as_ref();
    let run = report.run.as_ref();
    let counts = [
        ("xcodes", report.xcodes.len(), MAX_XCODES),
        (
            "mac.mdm_conflicts",
            mac.map_or(0, |m| m.mdm_conflicts.len()),
            MAX_MDM_CONFLICTS,
        ),
        ("due", report.due.len(), MAX_DUE),
        (
            "run.applied",
            run.map_or(0, |r| r.applied.len()),
            MAX_APPLIED,
        ),
    ];
    if let Some((field, count, max)) = counts.into_iter().find(|&(_, count, max)| count > max) {
        return Err(MaintenanceReportError::TooMany { field, count, max });
    }
    let host = report.host.iter().flat_map(|h| {
        [
            ("host.os_name", h.os_name.as_str(), MAX_OS_TEXT),
            ("host.os_version", h.os_version.as_str(), MAX_OS_TEXT),
            ("host.os_build", h.os_build.as_str(), MAX_OS_TEXT),
            (
                "host.hardware_serial",
                h.hardware_serial.as_str(),
                MAX_OS_TEXT,
            ),
            (
                "host.running_kernel",
                h.running_kernel.as_str(),
                MAX_KERNEL_TEXT,
            ),
            (
                "host.newest_kernel",
                h.newest_kernel.as_str(),
                MAX_KERNEL_TEXT,
            ),
            ("host.boot_id", h.boot_id.as_str(), MAX_KERNEL_TEXT),
        ]
    });
    let xcodes = report.xcodes.iter().flat_map(|x| {
        [
            ("xcodes.version", x.version.as_str(), MAX_XCODE_VERSION),
            ("xcodes.build", x.build.as_str(), MAX_XCODE_VERSION),
            ("xcodes.path", x.path.as_str(), MAX_XCODE_PATH),
        ]
    });
    let mac_texts = mac.into_iter().flat_map(|m| {
        m.command_line_tools
            .iter()
            .map(|tools| {
                (
                    "mac.command_line_tools.version",
                    tools.version.as_str(),
                    MAX_MAC_TEXT,
                )
            })
            .chain(
                m.mdm_conflicts
                    .iter()
                    .map(|key| ("mac.mdm_conflicts", key.as_str(), MAX_MAC_TEXT)),
            )
    });
    let due = report.due.iter().flat_map(|d| {
        [
            ("due.from", d.from.as_str(), MAX_DUE_VERSION),
            ("due.to", d.to.as_str(), MAX_DUE_VERSION),
            ("due.label", d.label.as_str(), MAX_DUE_LABEL),
        ]
    });
    let run_texts = run.into_iter().flat_map(|r| {
        [
            ("run.run_id", r.run_id.as_str(), MAX_RUN_ID),
            ("run.order_id", r.order_id.as_str(), MAX_RUN_ID),
            (
                "run.halted_reason",
                r.halted_reason.as_str(),
                MAX_HALTED_REASON,
            ),
        ]
    });
    let look_failed = [("look_failed", report.look_failed.as_str(), MAX_LOOK_FAILED)];
    match host
        .chain(xcodes)
        .chain(mac_texts)
        .chain(due)
        .chain(run_texts)
        .chain(look_failed)
        .find(|&(_, text, max)| text.len() > max)
    {
        Some((field, text, max)) => Err(MaintenanceReportError::TooLong {
            field,
            len: text.len(),
            max,
        }),
        None => Ok(()),
    }
}
